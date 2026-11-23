//! A font face, read lazily: glyphs are parsed when drawn (ab_glyph), so
//! memory stays about the file's size. fontdue parsed every glyph up
//! front: a Nerd Font's 12 608 glyphs held 48 MB, the title faces 74 MB.

use ab_glyph::{Font as _, FontVec, PxScale, ScaleFont as _};

pub struct Face {
    font: FontVec,
    /// px per em → ab_glyph's scale (the ascent-to-descent height).
    em_to_height: f32,
}

/// A glyph's bitmap and where it sits: `xmin` from the pen, `ymin` from
/// the baseline up to the bitmap's bottom (fontdue's convention).
pub struct Metrics {
    pub xmin: i32,
    pub ymin: i32,
    pub width: usize,
    pub height: usize,
    pub advance_width: f32,
}

/// Ascent (up, positive) and descent (down, negative) at a size.
pub struct LineMetrics {
    pub ascent: f32,
    pub descent: f32,
}

impl Face {
    pub fn from_bytes(bytes: Vec<u8>) -> Option<Face> {
        let font = FontVec::try_from_vec(bytes).ok()?;
        let em = font.units_per_em().unwrap_or(1000.0);
        let em_to_height = font.height_unscaled() / em;
        Some(Face { font, em_to_height })
    }

    fn scale(&self, px: f32) -> PxScale {
        PxScale::from(px * self.em_to_height)
    }

    pub fn advance(&self, c: char, px: f32) -> f32 {
        let f = self.font.as_scaled(self.scale(px));
        f.h_advance(self.font.glyph_id(c))
    }

    pub fn line_metrics(&self, px: f32) -> LineMetrics {
        let f = self.font.as_scaled(self.scale(px));
        LineMetrics {
            ascent: f.ascent(),
            descent: f.descent(),
        }
    }

    /// `c` at `px` per em: its metrics and coverage (0…255, rows top down).
    pub fn rasterize(&self, c: char, px: f32) -> (Metrics, Vec<u8>) {
        let id = self.font.glyph_id(c);
        let advance_width = self.advance(c, px);
        let glyph = id.with_scale_and_position(self.scale(px), ab_glyph::point(0.0, 0.0));
        let Some(outline) = self.font.outline_glyph(glyph) else {
            return (
                Metrics {
                    xmin: 0,
                    ymin: 0,
                    width: 0,
                    height: 0,
                    advance_width,
                },
                vec![],
            );
        };
        let b = outline.px_bounds();
        let (width, height) = (b.width() as usize, b.height() as usize);
        let mut bitmap = vec![0u8; width * height];
        outline.draw(|x, y, cov| {
            let (x, y) = (x as usize, y as usize);
            if x < width && y < height {
                bitmap[y * width + x] = (cov.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
        });
        (
            Metrics {
                xmin: b.min.x as i32,
                ymin: -(b.max.y as i32),
                width,
                height,
                advance_width,
            },
            bitmap,
        )
    }
}
