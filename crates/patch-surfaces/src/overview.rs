//! A deck's track overview as an image: one column a waveform bin, drawn
//! once per track (and theme). Slint drew it as 600 rectangles a deck,
//! each coloured by the playhead: all of them re-evaluated and drawn every
//! frame. Now a played and an unplayed copy; the Deck shows the played one
//! clipped to the playhead.

use slint::{Rgba8Pixel, SharedPixelBuffer};

/// Rows of the image (scaled to the tape's height).
pub const HEIGHT: u32 = 192;

/// Peak/RMS bins as a solid loudness body and faint peak envelope. Leave
/// headroom even for limited masters: their peaks must not fill the tape.
/// Width of the track map's lane image (it is stretched to the deck).
pub const LANE_W: u32 = 1200;

/// The track map's lane, 4 px high: per part (0…1 span, kick playing,
/// a mix spot) a hairline on the second row, solid where the kick plays,
/// dotted where it is out, two rows thick in `spot` for a mix spot; a tick
/// the lane's height where a part starts.
pub fn render_lane(
    parts: &[(f32, f32, bool, bool)],
    line: [u8; 4],
    faint: [u8; 4],
    spot: [u8; 4],
) -> SharedPixelBuffer<Rgba8Pixel> {
    let (w, h) = (LANE_W, 4u32);
    let mut img = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
    let px = img.make_mut_slice();
    let put = |px: &mut [Rgba8Pixel], x: u32, y: u32, c: [u8; 4]| {
        if x < w && y < h {
            px[(y * w + x) as usize] = Rgba8Pixel { r: c[0], g: c[1], b: c[2], a: c[3] };
        }
    };
    for (k, &(from, to, kicked, ranked)) in parts.iter().enumerate() {
        let (a, b) = ((from * w as f32) as u32, (to * w as f32).ceil() as u32);
        for x in a..b.min(w) {
            if ranked {
                put(px, x, 1, spot);
                put(px, x, 2, spot);
            } else if kicked {
                put(px, x, 1, line);
            } else if (x / 3) % 2 == 0 {
                put(px, x, 1, faint);
            }
        }
        if k > 0 {
            for y in 0..h {
                put(px, a, y, faint);
            }
        }
    }
    img
}

pub fn render(wave: &[[f32; 2]], col: [u8; 4]) -> SharedPixelBuffer<Rgba8Pixel> {
    let w = wave.len().max(1) as u32;
    let mut img = SharedPixelBuffer::<Rgba8Pixel>::new(w, HEIGHT);
    let px = img.make_mut_slice();
    let h = HEIGHT as f32;
    for (x, [peak, rms]) in wave.iter().enumerate() {
        let peak_h = (h * 0.78 * peak.clamp(0.0, 1.0)).max(1.0);
        // RMS preserves the differences the peak-only strip hid.
        // A very dense master still has a body below 60% of the tape.
        let body_h = (h * 0.58 * (rms * 1.5).clamp(0.0, 1.0))
            .max(1.0)
            .min(peak_h);
        for y in 0..HEIGHT {
            let cover = |bar: f32| {
                let top = (h - bar) / 2.0;
                ((y as f32 + 1.0).min(top + bar) - (y as f32).max(top)).clamp(0.0, 1.0)
            };
            let alpha = (0.22 * cover(peak_h)).max(cover(body_h));
            if alpha > 0.0 {
                px[(y * w) as usize + x] = Rgba8Pixel {
                    r: col[0],
                    g: col[1],
                    b: col[2],
                    a: (col[3] as f32 * alpha).round() as u8,
                };
            }
        }
    }
    img
}

/// Strokes along the AI timeline: 128 (64 read as dots, not a waveform).
pub const ARC_BINS: usize = 128;
/// Half a stroke's width, as a share of its cell (the rest is the gap).
pub const ARC_STROKE: f32 = 0.34;
/// A full-loudness stroke reaches this far either side of the rail (px).
pub fn arc_reach(compact: bool) -> f32 {
    if compact { 9.0 } else { 12.0 }
}

/// Coarse RMS groups preserve the track's broad dynamics, with deliberate gaps.
pub fn arc_bins(wave: &[[f32; 2]]) -> Vec<f32> {
    if wave.is_empty() {
        return vec![];
    }
    (0..ARC_BINS)
        .map(|i| {
            let lo = i * wave.len() / ARC_BINS;
            let hi = (((i + 1) * wave.len() / ARC_BINS).max(lo + 1)).min(wave.len());
            let bins = &wave[lo.min(wave.len() - 1)..hi];
            ((bins.iter().map(|b| b[1] * b[1]).sum::<f32>() / bins.len() as f32).sqrt() * 1.5)
                .clamp(0.0, 1.0)
        })
        .collect()
}

/// Sparse, taller loudness strokes following the mission rail; cached per track.
pub fn render_arc(wave: &[[f32; 2]], col: [u8; 4], compact: bool) -> SharedPixelBuffer<Rgba8Pixel> {
    let width = 1024u32;
    let height = if compact { 56 } else { 64 };
    let base = if compact { 34.0 } else { 42.0 };
    let rise = if compact { 14.0 } else { 20.0 };
    let mut image = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    let bins = arc_bins(wave);
    if bins.is_empty() {
        return image;
    }
    let pixels = image.make_mut_slice();
    for x in 0..width {
        let p = (x as f32 + 0.5) / width as f32;
        let cell = p * bins.len() as f32;
        if (cell.fract() - 0.5).abs() > ARC_STROKE {
            continue;
        }
        let half = 1.0 + arc_reach(compact) * bins[(cell as usize).min(bins.len() - 1)];
        let center = base - 4.0 * rise * p * (1.0 - p);
        for y in (center - half).floor().max(0.0) as u32
            ..=(center + half).ceil().min((height - 1) as f32) as u32
        {
            let cover = ((y as f32 + 1.0).min(center + half) - (y as f32).max(center - half))
                .clamp(0.0, 1.0);
            pixels[(y * width + x) as usize] = Rgba8Pixel {
                r: col[0],
                g: col[1],
                b: col[2],
                a: (col[3] as f32 * cover) as u8,
            };
        }
    }
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limited_peaks_leave_headroom_and_reveal_loudness_changes() {
        let img = render(&[[1.0, 0.8], [1.0, 0.3], [0.0, 0.0]], [10, 20, 30, 255]);
        let px = img.as_slice();
        let solid = |x: usize| {
            (0..HEIGHT as usize)
                .filter(|y| px[y * 3 + x].a > 127)
                .count()
        };
        assert!(solid(0) > solid(1) * 2, "equal peaks retain RMS contrast");
        assert!(solid(0) < HEIGHT as usize * 3 / 5);
        assert!(solid(2) <= 2, "silence is a hairline");
        for x in 0..3 {
            assert_eq!(px[x].a, 0);
            assert_eq!(px[(HEIGHT as usize - 1) * 3 + x].a, 0);
        }
        assert!(px[HEIGHT as usize / 2 * 3].a > 127);
    }

    #[test]
    fn envelope_is_symmetric_and_empty_tracks_are_safe() {
        let img = render(&[[1.0, 0.5]], [10, 20, 30, 255]);
        let px = img.as_slice();
        for y in 0..HEIGHT as usize {
            assert_eq!(px[y].a, px[HEIGHT as usize - 1 - y].a);
        }
        let empty = render(&[], [255; 4]);
        assert_eq!(empty.width(), 1);
        assert!(empty.as_slice().iter().all(|p| p.a == 0));
    }
}
