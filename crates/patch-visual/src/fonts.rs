//! Real fonts for the titles: the system's bold faces, rasterised into
//! character cells (a cell a pixel across, two pixels high: cells are
//! twice as tall as wide, so letters keep their shapes). Besides our own
//! 5×7 block letters, a song's title plays in sans, serif, narrow,
//! geometric, italic or monospaced type.

/// Faces tried, by fontconfig pattern (those not installed are skipped).
const FACES: &[(&str, &str)] = &[
    ("sans", "Nimbus Sans:bold"),
    ("narrow", "Nimbus Sans Narrow:bold"),
    ("serif", "Nimbus Roman:bold"),
    ("book", "C059:bold"),
    ("gothic", "URW Gothic:demi"),
    ("palatino", "P052:bold"),
    ("italic", "Liberation Sans:bold:italic"),
    ("duo", "iA Writer Duospace:bold"),
    ("mono", "Nimbus Mono PS:bold"),
    ("jetbrains", "JetBrainsMono Nerd Font:bold"),
];

pub struct Fonts {
    faces: Vec<(&'static str, crate::face::Face)>,
}

impl std::fmt::Debug for Fonts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.faces.iter().map(|x| x.0))
            .finish()
    }
}

/// Lit cells of a rendered text, its size in cells, and each cell's own
/// 2×4 sub-pixel pattern ([`crate::grid::shape`]).
pub struct Art {
    pub w: usize,
    pub h: usize,
    pub on: Vec<bool>,
    pub shape: Vec<char>,
}

impl Fonts {
    /// The installed faces (fontconfig), each once.
    pub fn load() -> Fonts {
        let mut seen = std::collections::HashSet::new();
        let mut faces = vec![];
        for (name, pattern) in FACES {
            let Some(path) = std::process::Command::new("fc-match")
                .args(["-f", "%{file}", pattern])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            else {
                continue;
            };
            // fontconfig falls back to something else for a face it lacks:
            // only the file the pattern names by family is wanted.
            let family = pattern
                .split(':')
                .next()
                .unwrap_or("")
                .to_lowercase()
                .replace(' ', "");
            let file = std::path::Path::new(&path)
                .file_name()
                .map(|f| f.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let first = family
                .split(' ')
                .next()
                .unwrap_or("")
                .chars()
                .take(5)
                .collect::<String>();
            if !file.contains(&first) || !seen.insert(path.clone()) {
                continue;
            }
            if let Some(font) = std::fs::read(&path)
                .ok()
                .and_then(crate::face::Face::from_bytes)
            {
                faces.push((*name, font));
            }
        }
        Fonts { faces }
    }

    pub fn len(&self) -> usize {
        self.faces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    pub fn name(&self, i: usize) -> &'static str {
        self.faces.get(i).map_or("block", |f| f.0)
    }

    /// `text` in face `i`, as large as fits `cols` × `rows` cells on one
    /// line (None when even small it doesn't, or there is no such face).
    pub fn art(&self, i: usize, text: &str, cols: usize, rows: usize) -> Option<Art> {
        let font = &self.faces.get(i)?.1;
        if text.trim().is_empty() || cols < 4 || rows < 3 {
            return None;
        }
        // Size from the widths at 100 px, then fitted to both limits. A
        // cell is two pixels across and four down (square pixels: cells
        // are twice as tall as wide): each cell gets its own pattern.
        let width_at = |px: f32| text.chars().map(|c| font.advance(c, px)).sum::<f32>();
        let lm = font.line_metrics(100.0);
        let line_h = lm.ascent - lm.descent;
        let by_w = cols as f32 * 2.0 / width_at(100.0).max(1.0) * 100.0;
        let by_h = rows as f32 * 4.0 / line_h * 100.0;
        let px = by_w.min(by_h);
        if px < 20.0 {
            return None;
        }
        let lm = font.line_metrics(px);
        let w = width_at(px).ceil() as usize + 1;
        let hp = (lm.ascent - lm.descent).ceil() as usize + 1;
        let mut canvas = vec![0u8; w * hp];
        let mut pen = 0.0f32;
        for ch in text.chars() {
            let (m, bitmap) = font.rasterize(ch, px);
            let x0 = (pen + m.xmin as f32).round() as i32;
            let y0 = (lm.ascent - m.height as f32 - m.ymin as f32).round() as i32;
            for y in 0..m.height {
                for x in 0..m.width {
                    let (cx, cy) = (x0 + x as i32, y0 + y as i32);
                    if cx >= 0 && cy >= 0 && (cx as usize) < w && (cy as usize) < hp {
                        let c = &mut canvas[cy as usize * w + cx as usize];
                        *c = (*c).max(bitmap[y * m.width + x]);
                    }
                }
            }
            pen += m.advance_width;
        }
        // 2×4 pixels a cell, each its own bit (a cell used to be one
        // pixel across and two down, lit when either was ink: curves in
        // stairs a cell high).
        let (pxw, h) = (w, hp.div_ceil(4));
        let w = pxw.div_ceil(2);
        // With how much ink each has (0…3): the edge's place within a
        // sub-pixel, for a smooth contour.
        let mut mask = vec![0u16; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut m = 0u16;
                for sy in 0..4 {
                    for sx in 0..2 {
                        let (px_x, px_y) = (2 * x + sx, 4 * y + sy);
                        if px_x < pxw && px_y < hp {
                            let level = ((canvas[px_y * pxw + px_x] as u32 * 3 + 127) / 255) as u16;
                            m |= level << ((sy * 2 + sx) * 2);
                        }
                    }
                }
                mask[y * w + x] = m;
            }
        }
        // Lit as before (a sub-pixel mostly inked): the faintest fringe
        // alone doesn't join letters (effects find letters by the gaps).
        let on: Vec<bool> = mask
            .iter()
            .map(|m| (0..8).any(|b| (m >> (b * 2)) & 3 >= 2))
            .collect();
        // Trim empty rows and columns around it.
        let rows_used: Vec<usize> = (0..h).filter(|y| (0..w).any(|x| on[y * w + x])).collect();
        let cols_used: Vec<usize> = (0..w).filter(|x| (0..h).any(|y| on[y * w + x])).collect();
        let (&y0, &y1, &x0, &x1) = (
            rows_used.first()?,
            rows_used.last()?,
            cols_used.first()?,
            cols_used.last()?,
        );
        let (tw, th) = (x1 - x0 + 1, y1 - y0 + 1);
        let mut out = vec![false; tw * th];
        let mut shape = vec![' '; tw * th];
        for y in 0..th {
            for x in 0..tw {
                out[y * tw + x] = on[(y0 + y) * w + x0 + x];
                shape[y * tw + x] = crate::grid::shape_inked(mask[(y0 + y) * w + x0 + x]);
            }
        }
        Some(Art {
            w: tw,
            h: th,
            on: out,
            shape,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_in_real_type_fits_its_room() {
        let f = Fonts::load();
        if f.is_empty() {
            return; // no system fonts (a bare CI box)
        }
        for i in 0..f.len() {
            let a = f
                .art(i, "Midnight Transit", 120, 20)
                .unwrap_or_else(|| panic!("{}", f.name(i)));
            assert!(a.w <= 121 && a.h <= 20, "{}: {}×{}", f.name(i), a.w, a.h);
            assert!(
                a.on.iter().filter(|x| **x).count() > 150,
                "{}: ink",
                f.name(i)
            );
        }
    }
}
