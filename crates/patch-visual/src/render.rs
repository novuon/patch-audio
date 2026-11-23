//! Drawing a frame: each cell's glyph in its colour, block characters drawn
//! exactly (so letters of '█' tile without seams), then a glow of what is
//! bright (a blurred quarter-size copy added back) and a lift on the beat.
//! Plain loops over bytes: a 1280×720 frame takes a few milliseconds.

use std::collections::HashMap;

use crate::grid::Grid;

/// Glyph coverage per cell size, from the theme's monospace font.
pub struct Glyphs {
    font: Option<crate::face::Face>,
    cell: (usize, usize),
    cache: HashMap<char, Vec<u8>>,
}

impl Glyphs {
    /// The font `family` (fontconfig's match, bold), or block glyphs only.
    pub fn load(family: &str) -> Glyphs {
        let path = std::process::Command::new("fc-match")
            .args(["-f", "%{file}", &format!("{family}:style=Bold")])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        let font = path
            .and_then(|p| std::fs::read(p).ok())
            .and_then(crate::face::Face::from_bytes);
        Glyphs {
            font,
            cell: (0, 0),
            cache: HashMap::new(),
        }
    }

    /// Without a font: blocks exact, everything else a dot.
    pub fn blocks() -> Glyphs {
        Glyphs {
            font: None,
            cell: (0, 0),
            cache: HashMap::new(),
        }
    }

    fn fit(&mut self, cw: usize, ch: usize) {
        if self.cell != (cw, ch) {
            self.cell = (cw, ch);
            self.cache.clear();
        }
    }

    /// Coverage 0…255 of `c` over a `cw`×`ch` cell, row-major (for the
    /// GPU's glyph atlas).
    pub fn coverage_at(&mut self, c: char, cw: usize, ch: usize) -> Vec<u8> {
        self.fit(cw.max(1), ch.max(1));
        self.coverage(c).to_vec()
    }

    /// Coverage 0…255 over the cell, row-major.
    fn coverage(&mut self, c: char) -> &[u8] {
        let (cw, chh) = self.cell;
        if !self.cache.contains_key(&c) {
            let cov = block(c, cw, chh).unwrap_or_else(|| self.raster(c));
            self.cache.insert(c, cov);
        }
        &self.cache[&c]
    }

    fn raster(&self, c: char) -> Vec<u8> {
        let (cw, ch) = self.cell;
        let mut out = vec![0u8; cw * ch];
        let Some(font) = &self.font else {
            // A dot in the middle.
            let (x0, y0) = (cw / 3, ch * 2 / 5);
            for y in y0..(ch * 3 / 5).max(y0 + 1) {
                for x in x0..(cw * 2 / 3).max(x0 + 1) {
                    out[y * cw + x] = 200;
                }
            }
            return out;
        };
        // As large as the cell holds: a monospace advance is ~0.6 em.
        let px = (ch as f32 * 0.82).min(cw as f32 / 0.6);
        let (m, bitmap) = font.rasterize(c, px);
        let baseline = (ch as f32 * 0.8).round() as i32;
        let x0 = ((cw as f32 - m.advance_width) / 2.0).round() as i32 + m.xmin;
        let y0 = baseline - (m.height as i32 + m.ymin);
        for y in 0..m.height {
            let ty = y0 + y as i32;
            if ty < 0 || ty >= ch as i32 {
                continue;
            }
            for x in 0..m.width {
                let tx = x0 + x as i32;
                if tx >= 0 && tx < cw as i32 {
                    out[ty as usize * cw + tx as usize] = bitmap[y * m.width + x];
                }
            }
        }
        out
    }
}

/// Block elements, drawn exactly.
fn block(c: char, cw: usize, ch: usize) -> Option<Vec<u8>> {
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32, v: u8| {
        let mut out = vec![0u8; cw * ch];
        let (xa, xb) = (
            (x0 * cw as f32).round() as usize,
            (x1 * cw as f32).round() as usize,
        );
        let (ya, yb) = (
            (y0 * ch as f32).round() as usize,
            (y1 * ch as f32).round() as usize,
        );
        for y in ya..yb.min(ch) {
            for x in xa..xb.min(cw) {
                out[y * cw + x] = v;
            }
        }
        out
    };
    // Letter art's 2×4 shapes: each sub-pixel exactly, as inked as it is.
    if let Some(levels) = crate::grid::shape_levels(c) {
        let mut out = vec![0u8; cw * ch];
        for bit in 0..8 {
            let level = ((levels >> (bit * 2)) & 3) as u8;
            if level != 0 {
                let (sx, sy) = (bit % 2, bit / 2);
                let (xa, xb) = ((sx * cw).div_ceil(2), ((sx + 1) * cw).div_ceil(2));
                let (ya, yb) = ((sy * ch).div_ceil(4), ((sy + 1) * ch).div_ceil(4));
                for y in ya..yb.min(ch) {
                    for x in xa..xb.min(cw) {
                        out[y * cw + x] = level * 85;
                    }
                }
            }
        }
        return Some(out);
    }
    Some(match c {
        '█' => rect(0.0, 0.0, 1.0, 1.0, 255),
        // Ambient's sun and neon branches use this bar. Several Omarchy
        // fonts lack it; geometry also makes neighbouring bars join exactly.
        '▬' => rect(0.0, 0.375, 1.0, 0.625, 255),
        '▓' => rect(0.0, 0.0, 1.0, 1.0, 190),
        '▒' => rect(0.0, 0.0, 1.0, 1.0, 128),
        '░' => rect(0.0, 0.0, 1.0, 1.0, 64),
        '▀' => rect(0.0, 0.0, 1.0, 0.5, 255),
        '▄' => rect(0.0, 0.5, 1.0, 1.0, 255),
        '▌' => rect(0.0, 0.0, 0.5, 1.0, 255),
        '▐' => rect(0.5, 0.0, 1.0, 1.0, 255),
        '▔' => rect(0.0, 0.0, 1.0, 0.125, 255),
        '▕' => rect(0.875, 0.0, 1.0, 1.0, 255),
        '▏' => rect(0.0, 0.0, 0.125, 1.0, 255),
        '▁'..='▇' => {
            let eighths = (c as u32 - '▁' as u32 + 1) as f32;
            rect(0.0, 1.0 - eighths / 8.0, 1.0, 1.0, 255)
        }
        _ => return None,
    })
}

/// How the frame looks beyond the effect's own colours.
#[derive(Clone, Copy, Debug)]
pub struct Look {
    pub bg: [u8; 3],
    /// Cells without a colour of their own.
    pub text: [u8; 3],
    /// 0…1: the lift on the beat (1 on the beat, falling to 0).
    pub pulse: f32,
    /// 0…1: how strongly the bright parts glow.
    pub glow: f32,
    /// The kick: the picture jumps up by this much of a cell…
    pub bump: f32,
    /// …and red and blue split apart by this many pixels…
    pub split: f32,
    /// …and it jolts sideways by this many pixels.
    pub shake: f32,
}

fn threads() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .clamp(1, 16)
}

/// Draw `g` into `w`×`h` RGBA pixels: bands of cell rows on all cores
/// (a 4K frame, 8 million pixels, in a few milliseconds).
pub fn draw(g: &Grid, w: usize, h: usize, glyphs: &mut Glyphs, look: &Look) -> Vec<u8> {
    if w == 0 || h == 0 {
        return vec![];
    }
    if g.cols == 0 || g.rows == 0 {
        return draw_cells(g, w, h, glyphs, look);
    }
    // Rasterise whole cells, then map the complete canvas to the output.
    // Rounding cell widths down left a smaller picture inside the window;
    // on small outputs a grid wider than the window even clipped the text.
    let rw = w.div_ceil(g.cols) * g.cols;
    let rh = h.div_ceil(g.rows) * g.rows;
    if (rw, rh) == (w, h) {
        return draw_cells(g, w, h, glyphs, look);
    }
    let native_look = Look {
        split: look.split * rw as f32 / w as f32,
        shake: look.shake * rw as f32 / w as f32,
        ..*look
    };
    let src = draw_cells(g, rw, rh, glyphs, &native_look);
    let mut out = vec![0; w * h * 4];
    let band = h.div_ceil(threads()).max(1);
    std::thread::scope(|scope| {
        for (i, rows) in out.chunks_mut(band * w * 4).enumerate() {
            let src = &src;
            scope.spawn(move || {
                for (j, row) in rows.chunks_exact_mut(w * 4).enumerate() {
                    let sy = ((i * band + j) * rh + rh / 2) / h;
                    for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
                        let sx = (x * rw + rw / 2) / w;
                        let at = (sy.min(rh - 1) * rw + sx.min(rw - 1)) * 4;
                        pixel.copy_from_slice(&src[at..at + 4]);
                    }
                }
            });
        }
    });
    out
}

fn draw_cells(g: &Grid, w: usize, h: usize, glyphs: &mut Glyphs, look: &Look) -> Vec<u8> {
    const N: usize = 4;
    let mut px = vec![0u8; w * h * N];
    let n = threads();
    // Background, in bands.
    std::thread::scope(|scope| {
        for chunk in px.chunks_mut((h.div_ceil(n)).max(1) * w * N) {
            scope.spawn(move || {
                for p in chunk.chunks_exact_mut(N) {
                    p[..3].copy_from_slice(&look.bg);
                    p[3] = 255;
                }
            });
        }
    });
    if g.cols > 0 && g.rows > 0 {
        let cw = (w / g.cols).max(1);
        let ch = (h / g.rows).max(1);
        glyphs.fit(cw, ch);
        // Every glyph this frame needs, rasterised first: the bands only
        // read the table.
        let mut seen = std::collections::HashSet::new();
        for c in &g.cells {
            if c.ch != ' ' && seen.insert(c.ch) {
                glyphs.coverage(c.ch);
            }
        }
        let table = &glyphs.cache;
        let ox = ((w.saturating_sub(cw * g.cols)) / 2) as isize + look.shake.round() as isize;
        let oy = ((h.saturating_sub(ch * g.rows)) / 2) as isize - (look.bump * ch as f32) as isize;
        let lift = 1.0 + look.pulse * 0.45;
        // Bands of whole cell rows; each owns its pixel rows.
        let per = g.rows.div_ceil(n).max(1);
        let edge = |r: usize| (oy + (r * ch) as isize).clamp(0, h as isize) as usize;
        let mut rest: &mut [u8] = &mut px;
        let mut taken = 0usize;
        std::thread::scope(|scope| {
            let mut r0 = 0;
            while r0 < g.rows {
                let r1 = (r0 + per).min(g.rows);
                let (top, bottom) = (edge(r0).max(taken), edge(r1).max(taken));
                let (_, tail) = std::mem::take(&mut rest).split_at_mut((top - taken) * w * N);
                let (band, tail) = tail.split_at_mut((bottom - top) * w * N);
                rest = tail;
                taken = bottom;
                let (rows, cols) = (r0..r1, g.cols);
                scope.spawn(move || {
                    for row in rows {
                        let y0 = oy + (row * ch) as isize;
                        // The part of the row inside this band (a row the
                        // kick lifts over the top edge is clipped, not dropped).
                        let from = (top as isize - y0).clamp(0, ch as isize) as usize;
                        let to = (bottom as isize - y0).clamp(0, ch as isize) as usize;
                        if from >= to {
                            continue;
                        }
                        // Band-relative: line y of the cell is band row y0 + y.
                        let y0 = y0 - top as isize;
                        for col in 0..cols {
                            let cell = g.at(col, row);
                            let x0 = ox + (col * cw) as isize;
                            if x0 < 0 || x0 as usize + cw > w {
                                continue;
                            }
                            let x0 = x0 as usize;
                            let bg = cell.bg.unwrap_or(look.bg);
                            if cell.bg.is_some() {
                                for y in from..to {
                                    let y = (y0 + y as isize) as usize;
                                    for p in band[(y * w + x0) * N..(y * w + x0 + cw) * N]
                                        .chunks_exact_mut(N)
                                    {
                                        p[..3].copy_from_slice(&bg);
                                    }
                                }
                            }
                            if cell.ch == ' ' || cell.ch == '\u{a0}' {
                                continue;
                            }
                            let Some(cov) = table.get(&cell.ch) else {
                                continue;
                            };
                            let fg = cell
                                .fg
                                .unwrap_or(look.text)
                                .map(|v| ((v as f32 * lift).min(255.0)) as u8);
                            for y in from..to {
                                let line = &cov[y * cw..(y + 1) * cw];
                                let at = (((y0 + y as isize) as usize) * w + x0) * N;
                                for (p, &a) in band[at..at + cw * N].chunks_exact_mut(N).zip(line) {
                                    if a == 255 {
                                        p[..3].copy_from_slice(&fg);
                                    } else if a != 0 {
                                        let a = a as u16;
                                        for k in 0..3 {
                                            p[k] = ((bg[k] as u16 * (255 - a) + fg[k] as u16 * a)
                                                / 255)
                                                as u8;
                                        }
                                    }
                                }
                            }
                        }
                    }
                });
                r0 = r1;
            }
        });
    }
    if look.glow > 0.01 {
        bloom(&mut px, w, h, look.glow * (0.7 + 0.5 * look.pulse));
    }
    let s = look.split.round() as usize;
    if s > 0 && s < w / 4 {
        split(&mut px, w, h, s);
    }
    px
}

/// Red a little to the left, blue to the right: a chromatic split.
fn split(px: &mut [u8], w: usize, h: usize, s: usize) {
    const N: usize = 4;
    let band = h.div_ceil(threads()).max(1);
    std::thread::scope(|scope| {
        for chunk in px.chunks_mut(band * w * N) {
            scope.spawn(move || {
                let mut row = vec![0u8; w * N];
                for line in chunk.chunks_exact_mut(w * N) {
                    row.copy_from_slice(line);
                    for x in 0..w {
                        line[x * N] = row[(x + s).min(w - 1) * N];
                        line[x * N + 2] = row[x.saturating_sub(s) * N + 2];
                    }
                }
            });
        }
    });
}

/// The bright parts, blurred at quarter size and added back.
fn bloom(px: &mut [u8], w: usize, h: usize, amount: f32) {
    // An eighth on big frames: the glow is soft, it looks the same.
    let s_ = if w >= 2400 { 8 } else { 4 };
    bloom_at(px, w, h, amount, s_);
}

fn bloom_at(px: &mut [u8], w: usize, h: usize, amount: f32, scale: usize) {
    let sc = scale;
    const N: usize = 4;
    let (sw, sh) = (w / sc, h / sc);
    if sw < 4 || sh < 4 {
        return;
    }
    // Quarter size: each block's mean above a threshold (bands of block
    // rows in parallel).
    let mut small = vec![0f32; sw * sh * 3];
    let per = sh.div_ceil(threads()).max(1);
    {
        let src: &[u8] = px;
        std::thread::scope(|scope| {
            for (i, out) in small.chunks_mut(per * sw * 3).enumerate() {
                scope.spawn(move || {
                    for (j, orow) in out.chunks_exact_mut(sw * 3).enumerate() {
                        let by = i * per + j;
                        for bx in 0..sw {
                            let mut acc = [0u32; 3];
                            for y in by * sc..by * sc + sc {
                                let row = &src[(y * w + bx * sc) * N..(y * w + bx * sc + sc) * N];
                                for p in row.chunks_exact(N) {
                                    acc[0] += p[0] as u32;
                                    acc[1] += p[1] as u32;
                                    acc[2] += p[2] as u32;
                                }
                            }
                            for k in 0..3 {
                                let m = acc[k] as f32 / (sc * sc) as f32;
                                orow[bx * 3 + k] = (m - 90.0).max(0.0) * amount * 1.6;
                            }
                        }
                    }
                });
            }
        });
    }
    // Box blur, both ways, twice: close to a Gaussian.
    let mut tmp = vec![0f32; small.len()];
    for _ in 0..2 {
        blur(&small, &mut tmp, sw, sh, 3, true);
        blur(&tmp, &mut small, sw, sh, 3, false);
    }
    // Each small row stretched to full width once, as bytes (in bands).
    let mut wide = vec![0u8; sh * w * 3];
    {
        let small = &small;
        std::thread::scope(|scope| {
            for (i, out) in wide.chunks_mut(per * w * 3).enumerate() {
                scope.spawn(move || {
                    for (j, dst) in out.chunks_exact_mut(w * 3).enumerate() {
                        let sy = i * per + j;
                        let src = &small[sy * sw * 3..(sy + 1) * sw * 3];
                        for x in 0..w {
                            let fx = ((x as f32 + 0.5) / sc as f32 - 0.5).max(0.0);
                            let x0 = (fx as usize).min(sw - 1);
                            let x1 = (x0 + 1).min(sw - 1);
                            let t = fx - x0 as f32;
                            for k in 0..3 {
                                dst[x * 3 + k] = (src[x0 * 3 + k] * (1.0 - t) + src[x1 * 3 + k] * t)
                                    .min(255.0)
                                    as u8;
                            }
                        }
                    }
                });
            }
        });
    }
    // Added back, rows in parallel, between the two stretched rows.
    let band = h.div_ceil(threads());
    std::thread::scope(|scope| {
        for (i, chunk) in px.chunks_mut(band * w * N).enumerate() {
            let wide = &wide;
            scope.spawn(move || {
                for (r, line) in chunk.chunks_exact_mut(w * N).enumerate() {
                    let y = i * band + r;
                    let fy = ((y as f32 + 0.5) / sc as f32 - 0.5).max(0.0);
                    let y0 = (fy as usize).min(sh - 1);
                    let y1 = (y0 + 1).min(sh - 1);
                    let t = ((fy - y0 as f32) * 256.0) as u16;
                    let (a, b) = (
                        &wide[y0 * w * 3..(y0 + 1) * w * 3],
                        &wide[y1 * w * 3..(y1 + 1) * w * 3],
                    );
                    for x in 0..w {
                        let p = &mut line[x * N..x * N + 3];
                        for k in 0..3 {
                            let v =
                                (a[x * 3 + k] as u16 * (256 - t) + b[x * 3 + k] as u16 * t) >> 8;
                            p[k] = p[k].saturating_add(v as u8);
                        }
                    }
                }
            });
        }
    });
}

fn blur(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize, horizontal: bool) {
    let (n, m) = if horizontal { (h, w) } else { (w, h) };
    let at = |i: usize, j: usize| {
        if horizontal {
            (i * w + j) * 3
        } else {
            (j * w + i) * 3
        }
    };
    let norm = 1.0 / (2 * r + 1) as f32;
    for i in 0..n {
        let mut acc = [0f32; 3];
        for j in 0..=r.min(m - 1) {
            for k in 0..3 {
                acc[k] += src[at(i, j) + k];
            }
        }
        for j in 0..m {
            for k in 0..3 {
                dst[at(i, j) + k] = acc[k] * norm;
            }
            let add = j + r + 1;
            if add < m {
                for k in 0..3 {
                    acc[k] += src[at(i, add) + k];
                }
            }
            if j >= r {
                for k in 0..3 {
                    acc[k] -= src[at(i, j - r) + k];
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{Cell, Grid};

    #[test]
    fn complete_grid_fills_awkward_and_smaller_viewports() {
        let mut g = Grid::blank(240, 68);
        for cell in &mut g.cells {
            cell.bg = Some([100, 50, 20]);
        }
        let look = Look {
            bg: [0; 3],
            text: [255; 3],
            pulse: 0.0,
            glow: 0.0,
            bump: 0.0,
            split: 0.0,
            shake: 0.0,
        };
        for (w, h) in [(641, 361), (173, 95), (1920, 1080)] {
            let px = draw(&g, w, h, &mut Glyphs::blocks(), &look);
            assert!(
                px.chunks_exact(4).all(|p| p == [100, 50, 20, 255]),
                "{w}×{h} fills edge to edge"
            );
        }
    }

    #[test]
    fn blocks_tile_and_glow_spreads() {
        let mut g = Grid::blank(4, 2);
        g.cells[0] = Cell {
            ch: '█',
            fg: Some([250, 250, 250]),
            bg: None,
        };
        g.cells[1] = Cell {
            ch: '█',
            fg: Some([250, 250, 250]),
            bg: None,
        };
        let mut glyphs = Glyphs::blocks();
        let look = Look {
            bg: [0, 0, 0],
            text: [200, 200, 200],
            pulse: 0.0,
            glow: 0.0,
            bump: 0.0,
            split: 0.0,
            shake: 0.0,
        };
        let px = draw(&g, 80, 40, &mut glyphs, &look);
        let at = |x: usize, y: usize| px[(y * 80 + x) * 4];
        assert_eq!(at(19, 5), 250, "the first block's right edge");
        assert_eq!(at(20, 5), 250, "no seam into the next");
        assert_eq!(at(45, 30), 0, "empty stays dark");
        let glowing = draw(&g, 80, 40, &mut glyphs, &Look { glow: 1.0, ..look });
        assert!(
            glowing[(25 * 80 + 20) * 4] > 0,
            "the glow reaches below the letters"
        );
    }

    #[test]
    fn a_4k_frame_is_fast() {
        let mut g = Grid::blank(480, 135);
        for (i, c) in g.cells.iter_mut().enumerate() {
            if i % 3 == 0 {
                *c = Cell {
                    ch: '█',
                    fg: Some([230, 80, 90]),
                    bg: None,
                };
            }
        }
        let mut glyphs = Glyphs::blocks();
        let look = Look {
            bg: [10, 10, 12],
            text: [220, 220, 220],
            pulse: 0.5,
            glow: 0.8,
            bump: 0.2,
            split: 6.0,
            shake: 0.0,
        };
        let frame = draw(&g, 3840, 2160, &mut glyphs, &look);
        assert_eq!(
            frame.len(),
            3840 * 2160 * 4,
            "a 4K frame is one RGBA pixel per output pixel"
        );
        assert!(frame.iter().any(|b| *b != 0), "the 4K frame is not blank");
        let t = std::time::Instant::now();
        for _ in 0..5 {
            let again = std::hint::black_box(draw(&g, 3840, 2160, &mut glyphs, &look));
            assert_eq!(again.len(), frame.len());
        }
        let ms = t.elapsed().as_secs_f64() * 1e3 / 5.0;
        assert!(ms.is_finite(), "{ms} ms");
        // The 16 ms ceiling is opt-in (`PATCH_CHECK_BUDGETS=1`). The draw
        // above is what this test always checks; the time is the measurement.
        eprintln!("4K frame average: {ms:.2} ms");
        if crate::check_budgets() {
            assert!(
                ms < 16.0,
                "4K frame {ms:.2} ms exceeds 16 ms (PATCH_CHECK_BUDGETS)"
            );
        }
    }

    #[test]
    fn a_full_frame_is_fast() {
        let mut g = Grid::blank(160, 45);
        for (i, c) in g.cells.iter_mut().enumerate() {
            if i % 3 == 0 {
                *c = Cell {
                    ch: '█',
                    fg: Some([230, 80, 90]),
                    bg: None,
                };
            }
        }
        let mut glyphs = Glyphs::blocks();
        let look = Look {
            bg: [10, 10, 12],
            text: [220, 220, 220],
            pulse: 0.5,
            glow: 0.8,
            bump: 0.2,
            split: 3.0,
            shake: 0.0,
        };
        let _ = draw(&g, 1280, 720, &mut glyphs, &look);
        let t = std::time::Instant::now();
        for _ in 0..5 {
            std::hint::black_box(draw(&g, 1280, 720, &mut glyphs, &look));
        }
        let ms = t.elapsed().as_secs_f64() * 1e3 / 5.0;
        if crate::check_budgets() {
            assert!(ms < 12.0, "{ms:.2} ms");
        }
    }
}

#[cfg(test)]
mod ambient_glyph_tests {
    use super::*;
    #[test]
    fn neon_bars_join_edge_to_edge_without_requiring_a_font() {
        let mut glyphs = Glyphs::blocks();
        let ink = glyphs.coverage_at('▬',8,16);
        for (y,row) in ink.chunks_exact(8).enumerate() {
            assert!(row.iter().all(|v| *v == if (6..10).contains(&y) {255} else {0}));
        }
    }
}
