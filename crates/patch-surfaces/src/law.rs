//! The crossfader, drawn: each deck's gain across the throw as a filled,
//! softly lit shape (deck A from the left, deck B from the right), straight
//! using the familiar equal-power visual guide, lit by what each deck
//! is playing into the mix, with the current position. Adaptive output
//! gain is intentionally separate from this illustrative shape.
//!
//! Rendered at the window's physical resolution with exact coverage
//! anti-aliasing (per-pixel distance to the edge), so every edge is smooth
//! at any scale on any renderer. Transparent where nothing is drawn.

use slint::{Rgba8Pixel, SharedPixelBuffer};

/// Familiar visual shape; independent of adaptive audio gain.
fn guide(x: f32) -> (f32, f32) {
    let t = x.clamp(0.0, 1.0) as f64 * std::f64::consts::FRAC_PI_2;
    let snap = |g: f64| if g < 1e-6 { 0.0 } else { g as f32 };
    (snap(t.cos()), snap(t.sin()))
}

/// Vertical placement: gain 1 at 10 % from the top, gain 0 at 95 %.
pub fn gain_y(gain: f32, height: f32) -> f32 {
    height * (0.95 - 0.85 * gain)
}

pub struct Colors {
    pub a: [u8; 3],
    pub b: [u8; 3],
    pub grid: [u8; 3],
    pub marker: [u8; 3],
}

/// What changes while it plays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Live {
    /// Crossfader position 0 (A) … 1 (B).
    pub pos: f32,
    /// Each deck's level in the mix, 0…1 (lights its shape).
    pub level: [f32; 2],
}

/// Premultiplied RGBA accumulator (over-compositing, then un-premultiplied).
struct Canvas {
    w: u32,
    h: u32,
    px: Vec<[f32; 4]>,
}

impl Canvas {
    fn over(&mut self, x: u32, y: u32, c: [u8; 3], a: f32) {
        if x >= self.w || y >= self.h || a <= 0.0 {
            return;
        }
        let p = &mut self.px[(y * self.w + x) as usize];
        let a = a.min(1.0);
        for k in 0..3 {
            p[k] = c[k] as f32 / 255.0 * a + p[k] * (1.0 - a);
        }
        p[3] = a + p[3] * (1.0 - a);
    }

    fn into_buffer(self) -> SharedPixelBuffer<Rgba8Pixel> {
        let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(self.w, self.h);
        for (d, p) in buf.make_mut_slice().iter_mut().zip(&self.px) {
            let a = p[3];
            *d = if a <= 1e-4 {
                Rgba8Pixel {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 0,
                }
            } else {
                let u = |v: f32| ((v / a).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                Rgba8Pixel {
                    r: u(p[0]),
                    g: u(p[1]),
                    b: u(p[2]),
                    a: (a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                }
            };
        }
        buf
    }
}

/// Render into `w` × `h` physical pixels; `inset` (physical px) keeps the
/// shapes clear of the A / B labels; `scale` is the window's scale factor.
pub fn render(
    w: u32,
    h: u32,
    inset: f32,
    scale: f32,
    c: &Colors,
    live: Live,
) -> SharedPixelBuffer<Rgba8Pixel> {
    let (w, h) = (w.max(2), h.max(2));
    let mut cv = Canvas {
        w,
        h,
        px: vec![[0.0; 4]; (w * h) as usize],
    };
    let hf = h as f32;
    let plot_w = (w as f32 - 2.0 * inset).max(1.0);
    let x0 = inset;
    let base = gain_y(0.0, hf);
    let top = gain_y(1.0, hf);

    // Reference lines: 0 dB (top) and −∞ (base) faint, −3 dB (where the
    // decks cross) dashed.
    let row = |cv: &mut Canvas, y: f32, alpha: f32, dash: bool| {
        let y = y.round() as u32;
        for x in x0.round() as u32..((x0 + plot_w).round() as u32).min(w) {
            if dash && ((x as f32 - x0) / (4.0 * scale)) as u32 % 2 == 1 {
                continue;
            }
            cv.over(x, y, c.grid, alpha);
        }
    };
    row(&mut cv, top, 0.35, false);
    row(&mut cv, base, 0.6, false);
    row(
        &mut cv,
        gain_y(std::f32::consts::FRAC_1_SQRT_2, hf),
        0.55,
        true,
    );

    // Each deck's shape: filled under its gain curve, a vertical gradient
    // (brighter towards the edge), lit by the deck's level, an exact
    // anti-aliased edge, then a crisp rim along the curve.
    for deck in 0..2 {
        let col = if deck == 0 { c.a } else { c.b };
        let lit = live.level[deck].clamp(0.0, 1.0);
        let fill_top = 0.16 + 0.34 * lit;
        let fill_base = 0.03 + 0.05 * lit;
        let rim = 0.55 + 0.45 * lit;
        let gain_at = |x: f32| {
            let t = ((x - x0) / plot_w).clamp(0.0, 1.0);
            let (a, b) = guide(t);
            gain_y(if deck == 0 { a } else { b }, hf)
        };
        for x in x0.floor() as u32..((x0 + plot_w).ceil() as u32).min(w) {
            // Coverage per column: 4 sub-samples across the pixel.
            let ys: Vec<f32> = (0..4)
                .map(|k| gain_at(x as f32 + (k as f32 + 0.5) / 4.0))
                .collect();
            let edge = ys.iter().copied().fold(f32::MAX, f32::min);
            for y in (edge.floor().max(0.0) as u32)..(base.ceil() as u32).min(h) {
                let yc = y as f32 + 0.5;
                // Horizontal × vertical coverage of the area under the curve.
                let cov = ys
                    .iter()
                    .map(|e| ((yc + 0.5) - e).clamp(0.0, 1.0))
                    .sum::<f32>()
                    / 4.0
                    * ((base - (yc - 0.5)).clamp(0.0, 1.0));
                if cov <= 0.0 {
                    continue;
                }
                let depth = ((yc - top) / (base - top)).clamp(0.0, 1.0);
                let a = fill_top + (fill_base - fill_top) * depth;
                cv.over(x, y, col, a * cov);
            }
        }
        // The rim: distance-field stroke along the curve, 1.6 logical px.
        let r = 0.8 * scale;
        let n = ((plot_w / 1.5) as usize).clamp(48, 2048);
        let pts: Vec<(f32, f32)> = (0..=n)
            .map(|i| {
                let x = x0 + plot_w * i as f32 / n as f32;
                (x, gain_at(x))
            })
            .collect();
        let mut cover = vec![0f32; (w * h) as usize];
        for s in pts.windows(2) {
            stroke(&mut cover, w, h, s[0], s[1], r);
        }
        for (i, v) in cover.iter().enumerate() {
            if *v > 0.0 {
                cv.over(i as u32 % w, i as u32 / w, col, v * rim);
            }
        }
    }

    // Position: a hairline and a lit dot on each shape at its current gain.
    let px = x0 + plot_w * live.pos.clamp(0.0, 1.0);
    for y in top.floor() as u32..(base.ceil() as u32).min(h) {
        let d = (px - (px.floor() + 0.5)).abs();
        cv.over(px.floor() as u32, y, c.marker, 0.55 * (1.0 - d).max(0.4));
    }
    let (ga, gb) = guide(live.pos);
    for (deck, g) in [(0usize, ga), (1, gb)] {
        let col = if deck == 0 { c.a } else { c.b };
        dot(&mut cv, px, gain_y(g, hf), 3.2 * scale, col, 1.0);
        dot(&mut cv, px, gain_y(g, hf), 1.4 * scale, c.marker, 0.9);
    }
    cv.into_buffer()
}

fn dot(cv: &mut Canvas, cx: f32, cy: f32, r: f32, c: [u8; 3], a: f32) {
    for y in (cy - r - 1.0).floor().max(0.0) as u32..=(cy + r + 1.0).ceil() as u32 {
        for x in (cx - r - 1.0).floor().max(0.0) as u32..=(cx + r + 1.0).ceil() as u32 {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            let cov = (r + 0.5 - d).clamp(0.0, 1.0);
            cv.over(x, y, c, a * cov);
        }
    }
}

/// Coverage of a round-capped segment of radius `r`, max-combined so joints
/// between segments don't double up.
fn stroke(cover: &mut [f32], w: u32, h: u32, p0: (f32, f32), p1: (f32, f32), r: f32) {
    let (x0, x1) = (p0.0.min(p1.0) - r - 1.0, p0.0.max(p1.0) + r + 1.0);
    let (y0, y1) = (p0.1.min(p1.1) - r - 1.0, p0.1.max(p1.1) + r + 1.0);
    let (dx, dy) = (p1.0 - p0.0, p1.1 - p0.1);
    let len2 = (dx * dx + dy * dy).max(1e-9);
    for y in (y0.floor().max(0.0) as u32)..(y1.ceil().max(0.0) as u32).min(h) {
        for x in (x0.floor().max(0.0) as u32)..(x1.ceil().max(0.0) as u32).min(w) {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let t = (((px - p0.0) * dx + (py - p0.1) * dy) / len2).clamp(0.0, 1.0);
            let (qx, qy) = (p0.0 + t * dx - px, p0.1 + t * dy - py);
            let c = (r + 0.5 - (qx * qx + qy * qy).sqrt()).clamp(0.0, 1.0);
            let i = (y * w + x) as usize;
            if c > cover[i] {
                cover[i] = c;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colors() -> Colors {
        Colors {
            a: [220, 220, 220],
            b: [230, 140, 20],
            grid: [70, 70, 70],
            marker: [255, 255, 255],
        }
    }

    #[test]
    fn shapes_keep_the_familiar_guide_with_soft_edges() {
        let (w, h) = (400u32, 160u32);
        let img = render(
            w,
            h,
            20.0,
            1.0,
            &colors(),
            Live {
                pos: 0.5,
                level: [0.5, 0.5],
            },
        );
        let px = img.as_slice();
        // Left end: A full (filled up to the top), B empty at the base.
        let x = 22u32;
        let a_top = gain_y(1.0, h as f32) as u32 + 3;
        assert!(
            px[(a_top * w + x) as usize].a > 60,
            "A filled near the top at the left"
        );
        // Soft edges: partial coverage exists.
        let partial = px.iter().filter(|p| p.a > 10 && p.a < 200).count();
        assert!(partial > 500, "anti-aliased ({partial})");
        // Nothing drawn above the 0 dB line except the reference line.
        let above = gain_y(1.0, h as f32) as u32 - 3;
        assert!((30..w - 30).all(|x| px[(above * w + x) as usize].a == 0));
    }

    #[test]
    fn a_louder_deck_lights_its_shape() {
        let (w, h) = (300u32, 120u32);
        let quiet = render(
            w,
            h,
            16.0,
            1.0,
            &colors(),
            Live {
                pos: 0.0,
                level: [0.0, 0.0],
            },
        );
        let loud = render(
            w,
            h,
            16.0,
            1.0,
            &colors(),
            Live {
                pos: 0.0,
                level: [1.0, 0.0],
            },
        );
        let at = ((gain_y(0.6, h as f32) as u32) * w + 40) as usize;
        assert!(loud.as_slice()[at].a > quiet.as_slice()[at].a + 20);
    }
}

#[cfg(test)]
mod preview {
    /// `PATCH_LAW_SHOT=file.ppm cargo test -p patch-surfaces law_shot -- --ignored`
    #[test]
    #[ignore]
    fn law_shot() {
        let Ok(path) = std::env::var("PATCH_LAW_SHOT") else {
            return;
        };
        let c = super::Colors {
            a: [205, 214, 206],
            b: [122, 162, 118],
            grid: [52, 64, 56],
            marker: [235, 240, 235],
        };
        let (w, h) = (720u32, 260u32);
        let bg = [22u8, 30, 25];
        let mut out = format!("P6 {w} {} 255\n", h * 3).into_bytes();
        for live in [
            super::Live {
                pos: 0.12,
                level: [0.8, 0.05],
            },
            super::Live {
                pos: 0.5,
                level: [0.6, 0.6],
            },
            super::Live {
                pos: 0.8,
                level: [0.2, 0.9],
            },
        ] {
            let img = super::render(w, h, 30.0, 1.6, &c, live);
            for p in img.as_slice() {
                let a = p.a as f32 / 255.0;
                out.extend([
                    (p.r as f32 * a + bg[0] as f32 * (1.0 - a)) as u8,
                    (p.g as f32 * a + bg[1] as f32 * (1.0 - a)) as u8,
                    (p.b as f32 * a + bg[2] as f32 * (1.0 - a)) as u8,
                ]);
            }
        }
        std::fs::write(path, out).unwrap();
    }
}
