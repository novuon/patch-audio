//! Scrolling deck waveform ("scope"), rendered in Rust into a pixel image.
//!
//! One image per deck per frame: a window of the track centred on the
//! playhead, drawn from the analysis' detailed waveform (three bands) with
//! the beat grid (beats, bars from the downbeat, 8-bar phrases), hot cues,
//! the loop and clipping marks. Positions come from the engine's stamped
//! playhead, so the picture moves with what is heard. Drawing a few hundred
//! thousand pixels is far cheaper than thousands of UI elements, and the
//! colours come from the Omarchy theme.

use patch_analysis::detail::Detail;
use slint::{Color, Rgba8Pixel, SharedPixelBuffer};

pub struct Colors {
    pub bg: Color,
    pub low: Color,
    pub mid: Color,
    pub high: Color,
    pub beat: Color,
    pub bar: Color,
    pub accent: Color,
    pub clip: Color,
}

/// How the waveform is drawn (switchable on the deck, kept in
/// `~/.config/omarchy-patch/waveform`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Style {
    /// Peaks of three bands per pixel, the classic DJ waveform.
    #[default]
    Classic,
    /// The same bands as soft, smoothed envelopes with clean edges.
    Smooth,
    /// The track as a step sequencer: one cell per 16th note on the beat
    /// grid, each band's height its level there (without a grid: Smooth).
    /// (Tried as the default: the kicks' shapes were gone, the beats hard
    /// to see.)
    Steps,
}

impl Style {
    pub const ALL: [Style; 3] = [Style::Classic, Style::Smooth, Style::Steps];

    pub fn name(self) -> &'static str {
        match self {
            Style::Classic => "classic",
            Style::Smooth => "smooth",
            Style::Steps => "steps",
        }
    }

    pub fn parse(s: &str) -> Option<Style> {
        Style::ALL.into_iter().find(|x| x.name() == s.trim())
    }

    pub fn next(self) -> Style {
        let i = Style::ALL.iter().position(|x| *x == self).unwrap_or(0);
        Style::ALL[(i + 1) % Style::ALL.len()]
    }

    fn path() -> Option<std::path::PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
            })?;
        Some(base.join("omarchy-patch").join("waveform"))
    }

    /// The saved choice (the default when none).
    pub fn load() -> Style {
        Style::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| Style::parse(&s))
            .unwrap_or_default()
    }

    pub fn save(self) {
        if let Some(p) = Style::path() {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(p, format!("{}\n", self.name()));
        }
    }
}

pub struct View<'a> {
    pub detail: &'a Detail,
    /// Playhead, seconds (track time).
    pub at: f64,
    /// Seconds shown across the width.
    pub span: f64,
    /// (bpm, first beat seconds, downbeat index 0…3).
    pub grid: Option<(f64, f64, Option<u8>)>,
    /// Every cue the deck holds, not just the four the booth draws: the loop
    /// below walks whatever it is given, so a pad above four still shows here.
    pub cues: crate::dj::Cues,
    pub looping: Option<(f64, f64)>,
    pub loop_on: bool,
    /// Draw the playhead line into the image, and dim what played (the deck
    /// lays its own over a strip it scrolls, veil included: dimmed in the
    /// strip, the played part changed only when it was redrawn; tests and
    /// one-off pictures draw both in).
    pub playhead: bool,
    pub style: Style,
}

fn px(c: Color) -> Rgba8Pixel {
    Rgba8Pixel {
        r: c.red(),
        g: c.green(),
        b: c.blue(),
        a: 255,
    }
}

fn blend(a: Rgba8Pixel, b: Color, alpha: f32) -> Rgba8Pixel {
    let m = |x: u8, y: u8| (x as f32 * (1.0 - alpha) + y as f32 * alpha) as u8;
    Rgba8Pixel {
        r: m(a.r, b.red()),
        g: m(a.g, b.green()),
        b: m(a.b, b.blue()),
        a: 255,
    }
}

pub fn render(width: u32, height: u32, v: &View, c: &Colors) -> SharedPixelBuffer<Rgba8Pixel> {
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    let (w, h) = (width as usize, height as usize);
    let pixels = buf.make_mut_slice();
    let bg = px(c.bg);
    pixels.fill(bg);
    let mid_y = h as f32 / 2.0;
    let t0 = v.at - v.span / 2.0;
    let per_px = v.span / w as f64;
    let cps = v.detail.columns_per_second as f64;
    let cols = &v.detail.columns;
    // Loop region tint.
    if let Some((a, b)) = v.looping {
        let (xa, xb) = (((a - t0) / per_px) as i64, ((b - t0) / per_px) as i64);
        for x in xa.max(0)..xb.min(w as i64) {
            for y in 0..h {
                let p = &mut pixels[y * w + x as usize];
                *p = blend(*p, c.accent, if v.loop_on { 0.16 } else { 0.06 });
            }
        }
    }
    let style = match (v.style, v.grid) {
        (Style::Steps, None) => Style::Smooth,
        (s, _) => s,
    };
    // One column of the three bands (levels 0…1 of the half height) from
    // x, shaded as one gradient rather than three flat fills: the highs'
    // colour at the centre line, blending through the mids' to the lows'
    // at the outer envelope, whose tip is anti-aliased. (Flat fills met in
    // hard steps: two-coloured, harsh edges.) Past columns dimmed
    // (one-off pictures).
    let rgb = |c: Color| [c.red() as f32, c.green() as f32, c.blue() as f32];
    let (lo_c, mid_c, hi_c, bg_c) = (rgb(c.low), rgb(c.mid), rgb(c.high), rgb(c.bg));
    let lerp = |a: [f32; 3], b: [f32; 3], t: f32| {
        [
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
        ]
    };
    // Colour stops along the distance from the centre: the highs' at 0,
    // halfway highs/mids at the highs' envelope, halfway mids/lows at the
    // mids', the lows' at the outer edge.
    let (hm, ml) = (lerp(hi_c, mid_c, 0.5), lerp(mid_c, lo_c, 0.5));
    let column = |pixels: &mut [Rgba8Pixel], x: usize, levels: [f32; 3], past: bool| {
        let half = |l: f32| l.clamp(0.0, 1.0) * mid_y * 0.95;
        let outer = half(levels[0]).max(half(levels[1])).max(half(levels[2]));
        if outer < 0.5 {
            return;
        }
        // Envelopes in order from the centre (a loud high band can reach
        // past the mids: never a stop beyond the edge).
        let e_hi = half(levels[2]).min(outer);
        let e_mid = half(levels[1]).clamp(e_hi, outer);
        let full = outer as usize;
        let frac = outer - full as f32;
        let shade = |r: f32| -> [f32; 3] {
            if r <= e_hi {
                lerp(hi_c, hm, if e_hi > 0.0 { r / e_hi } else { 1.0 })
            } else if r <= e_mid {
                lerp(hm, ml, (r - e_hi) / (e_mid - e_hi).max(1e-3))
            } else {
                lerp(ml, lo_c, (r - e_mid) / (outer - e_mid).max(1e-3))
            }
        };
        let put = |pixels: &mut [Rgba8Pixel], y: usize, col: [f32; 3], a: f32| {
            let col = if past { lerp(col, bg_c, 0.45) } else { col };
            let p = &mut pixels[y * w + x];
            let k = a.clamp(0.0, 1.0);
            p.r = (p.r as f32 + (col[0] - p.r as f32) * k) as u8;
            p.g = (p.g as f32 + (col[1] - p.g as f32) * k) as u8;
            p.b = (p.b as f32 + (col[2] - p.b as f32) * k) as u8;
        };
        let m = mid_y as usize;
        for d in 0..full {
            let col = shade(d as f32 + 0.5);
            if m + d < h {
                put(pixels, m + d, col, 1.0);
            }
            if let Some(y) = (m).checked_sub(d + 1) {
                put(pixels, y, col, 1.0);
            }
        }
        // The tip, anti-aliased: no stair-stepped outline.
        if frac > 0.02 {
            let col = lo_c;
            if m + full < h {
                put(pixels, m + full, col, frac);
            }
            if let Some(y) = m.checked_sub(full + 1) {
                put(pixels, y, col, frac);
            }
        }
    };
    // Band levels at time t (seconds): the columns between, linearly.
    let level_at = |t: f64, band: usize| -> f32 {
        let f = t * cps;
        if f < 0.0 {
            return 0.0;
        }
        let i = f as usize;
        let (a, b) = (
            cols.get(i).map_or(0.0, |c| c[band] as f32),
            cols.get(i + 1).map_or(0.0, |c| c[band] as f32),
        );
        let k = (f - i as f64) as f32;
        (a + (b - a) * k) / 255.0
    };
    match style {
        Style::Classic | Style::Smooth => {
            // Classic: per pixel column, the peak of the columns it covers,
            // low band widest (darkest), then mid, then high on top
            // (brightest). Smooth: each band's envelope, a small triangle
            // over ±12 ms (no jagged peaks), each tip anti-aliased.
            let reach = 0.012;
            for x in 0..w {
                let (ta, tb) = (t0 + x as f64 * per_px, t0 + (x + 1) as f64 * per_px);
                if tb < 0.0 {
                    continue;
                }
                if (ta * cps) as usize >= cols.len() {
                    break;
                }
                let levels = if style == Style::Classic {
                    // A hair of slack: a pixel edge on a column edge (the
                    // deck's strip) takes the same columns in every redraw,
                    // not one more or less by rounding.
                    let ia = (ta.max(0.0) * cps + 1e-6) as usize;
                    let ib = ((tb * cps - 1e-6).ceil() as usize).max(ia + 1);
                    let mut m = [0u8; 3];
                    for col in &cols[ia..ib.min(cols.len())] {
                        for k in 0..3 {
                            m[k] = m[k].max(col[k]);
                        }
                    }
                    m.map(|v| v as f32 / 255.0)
                } else {
                    let tc = (ta + tb) / 2.0;
                    let mut out = [0.0f32; 3];
                    for band in 0..3 {
                        let (mut sum, mut wsum) = (0.0f32, 0.0f32);
                        for k in -4..=4 {
                            let wt = 1.0 - (k as f32).abs() / 5.0;
                            sum += level_at(tc + reach * k as f64 / 4.0, band) * wt;
                            wsum += wt;
                        }
                        // A touch louder than the mean: the smoothing takes
                        // the edge off transients, not their size.
                        out[band] = (sum / wsum * 1.15).min(1.0);
                    }
                    out
                };
                column(pixels, x, levels, v.playhead && tb <= v.at);
                // Clipping: full-scale peaks marked at the top and bottom edge.
                let i = (ta.max(0.0) * cps) as usize;
                if cols.get(i).is_some_and(|c| c[3] >= 254) {
                    for y in [0, 1, h - 2, h - 1] {
                        pixels[y * w + x] = px(c.clip);
                    }
                }
            }
        }
        Style::Steps => {
            // One cell per 16th: its peak per band, drawn as a block with a
            // gap between cells (wider between beats), corners rounded.
            let (bpm, off, _) = v.grid.unwrap_or((120.0, 0.0, None));
            let step = 60.0 / bpm / 4.0;
            let first = ((t0 - off) / step).floor() as i64;
            let last = ((t0 + v.span - off) / step).ceil() as i64;
            for k in first..=last {
                let (ta, tb) = (off + k as f64 * step, off + (k + 1) as f64 * step);
                if tb <= 0.0 {
                    continue;
                }
                let (ia, ib) = ((ta.max(0.0) * cps) as usize, (tb * cps).ceil() as usize);
                if ia >= cols.len() {
                    break;
                }
                let mut m = [0u8; 4];
                for col in &cols[ia..ib.min(cols.len())] {
                    for b in 0..4 {
                        m[b] = m[b].max(col[b]);
                    }
                }
                let gap = if k.rem_euclid(4) == 0 { 3.0 } else { 1.0 };
                let xa = ((ta - t0) / per_px + gap).round() as i64;
                let xb = ((tb - t0) / per_px).round() as i64;
                let levels = [m[0], m[1], m[2]].map(|v| v as f32 / 255.0);
                for x in xa.max(0)..xb.min(w as i64) {
                    let past = v.playhead && off + (k + 1) as f64 * step <= v.at;
                    column(pixels, x as usize, levels, past);
                }
                // Rounded corners: the outer envelope's end pixels pulled
                // in by one (the inner bands are one gradient now).
                let outer = levels
                    .iter()
                    .map(|l| (l.clamp(0.0, 1.0) * mid_y * 0.95) as usize)
                    .max()
                    .unwrap_or(0);
                if outer >= 3 {
                    for x in [xa, xb - 1] {
                        if x < 0 || x >= w as i64 {
                            continue;
                        }
                        for y in [mid_y as usize - outer, mid_y as usize + outer - 1] {
                            if y < h {
                                pixels[y * w + x as usize] = bg;
                            }
                        }
                    }
                }
                if m[3] >= 254 {
                    for x in xa.max(0)..xb.min(w as i64) {
                        for y in [0, 1, h - 2, h - 1] {
                            pixels[y * w + x as usize] = px(c.clip);
                        }
                    }
                }
            }
        }
    }
    // Beat grid: beats faint, bars (from the downbeat) stronger, 8-bar
    // phrases in the accent colour, as short ticks top and bottom.
    if let Some((bpm, off, down)) = v.grid {
        let per = 60.0 / bpm;
        let first = ((t0 - off) / per).floor() as i64;
        let last = ((t0 + v.span - off) / per).ceil() as i64;
        for k in first..=last {
            let t = off + k as f64 * per;
            let x = ((t - t0) / per_px).round() as i64;
            if x < 0 || x >= w as i64 {
                continue;
            }
            let pos = down.map(|d| (k - d as i64).rem_euclid(32));
            let (color, len) = match pos {
                Some(0) => (c.accent, h / 4),
                Some(p) if p % 4 == 0 => (c.bar, h / 6),
                _ => (c.beat, h / 12),
            };
            for y in (0..len).chain(h - len..h) {
                pixels[y * w + x as usize] = px(color);
            }
        }
    }
    // Hot cues: full-height accent lines with a small flag.
    for t in v.cues.iter().flatten() {
        let x = ((t - t0) / per_px).round() as i64;
        if (0..w as i64).contains(&x) {
            for y in 0..h {
                pixels[y * w + x as usize] = px(c.accent);
            }
            for y in 0..6.min(h) {
                for dx in 1..6 {
                    if (x + dx) < w as i64 {
                        pixels[y * w + (x + dx) as usize] = px(c.accent);
                    }
                }
            }
        }
    }
    // Playhead: centre line, 2 px.
    let cx = w / 2;
    for y in (0..h).filter(|_| v.playhead) {
        pixels[y * w + cx] = px(c.accent);
        if cx + 1 < w {
            pixels[y * w + cx + 1] = px(c.accent);
        }
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The deck strip (2400 × 112, 16 s) per style, as drawn on the UI
    /// thread: cargo test --release -p patch-surfaces strip_cost -- --ignored --nocapture
    #[test]
    #[ignore]
    fn strip_cost() {
        let detail = Detail {
            columns_per_second: 150,
            columns: (0..150 * 300)
                .map(|i| {
                    [
                        (i * 7 % 255) as u8,
                        (i * 13 % 255) as u8,
                        (i * 3 % 255) as u8,
                        200,
                    ]
                })
                .collect(),
        };
        let colors = Colors {
            bg: Color::from_rgb_u8(0, 0, 0),
            low: Color::from_rgb_u8(80, 80, 80),
            mid: Color::from_rgb_u8(150, 150, 150),
            high: Color::from_rgb_u8(230, 230, 230),
            beat: Color::from_rgb_u8(60, 60, 60),
            bar: Color::from_rgb_u8(120, 120, 120),
            accent: Color::from_rgb_u8(255, 140, 0),
            clip: Color::from_rgb_u8(255, 0, 0),
        };
        for style in Style::ALL {
            let v = View {
                detail: &detail,
                at: 100.0,
                span: 16.0,
                grid: Some((128.0, 0.1, Some(0))),
                cues: [Some(100.5), None, None, None, None, None, None, None],
                looping: None,
                loop_on: false,
                playhead: false,
                style,
            };
            let t = std::time::Instant::now();
            for _ in 0..10 {
                std::hint::black_box(render(2400, 112, &v, &colors));
            }
            println!(
                "{}: {:.1} ms",
                style.name(),
                t.elapsed().as_secs_f64() * 100.0
            );
        }
    }

    #[test]
    fn draws_the_playhead_grid_and_waveform() {
        let detail = Detail {
            columns_per_second: 150,
            columns: vec![[200, 120, 60, 210]; 150 * 20],
        };
        let colors = Colors {
            bg: Color::from_rgb_u8(0, 0, 0),
            low: Color::from_rgb_u8(80, 80, 80),
            mid: Color::from_rgb_u8(150, 150, 150),
            high: Color::from_rgb_u8(230, 230, 230),
            beat: Color::from_rgb_u8(60, 60, 60),
            bar: Color::from_rgb_u8(120, 120, 120),
            accent: Color::from_rgb_u8(255, 140, 0),
            clip: Color::from_rgb_u8(255, 0, 0),
        };
        let v = View {
            detail: &detail,
            at: 10.0,
            span: 8.0,
            grid: Some((120.0, 0.0, Some(0))),
            cues: [Some(10.5), None, None, None, None, None, None, None],
            looping: None,
            loop_on: false,
            playhead: true,
            style: Style::Classic,
        };
        let img = render(400, 80, &v, &colors);
        let p = img.as_slice();
        let at = |x: usize, y: usize| p[y * 400 + x];
        assert_eq!(
            (at(200, 40).r, at(200, 40).g),
            (255, 140),
            "playhead in the middle"
        );
        // 0.5 s after the playhead = +25 px: the hot cue.
        assert_eq!(at(225, 70).r, 255);
        // A beat every 0.5 s = 25 px; beats sit at x = 200 ± 25k.
        assert_ne!(at(175, 1).r, 0, "beat tick at the top edge");
    }

    #[test]
    fn renders_fast() {
        let detail = Detail {
            columns_per_second: 150,
            columns: vec![[200, 120, 60, 210]; 150 * 400],
        };
        let colors = Colors {
            bg: Color::from_rgb_u8(0, 0, 0),
            low: Color::from_rgb_u8(80, 80, 80),
            mid: Color::from_rgb_u8(150, 150, 150),
            high: Color::from_rgb_u8(230, 230, 230),
            beat: Color::from_rgb_u8(60, 60, 60),
            bar: Color::from_rgb_u8(120, 120, 120),
            accent: Color::from_rgb_u8(255, 140, 0),
            clip: Color::from_rgb_u8(255, 0, 0),
        };
        let v = View {
            detail: &detail,
            at: 120.0,
            span: 8.0,
            grid: Some((124.0, 0.1, Some(1))),
            cues: [Some(121.0), Some(119.0), None, None, None, None, None, None],
            looping: Some((118.0, 122.0)),
            loop_on: true,
            playhead: true,
            style: Style::Classic,
        };
        let t = std::time::Instant::now();
        for _ in 0..20 {
            std::hint::black_box(render(1200, 128, &v, &colors));
        }
        let ms = t.elapsed().as_secs_f64() * 1e3 / 20.0;
        assert!(ms < 3.0, "one frame took {ms:.2} ms");
    }

    /// The deck redraws its strip around a new anchor on the strip's pixel
    /// grid: where old and new overlap, every pixel is the same (no twitch
    /// when the new strip replaces the old).
    #[test]
    fn a_strip_redrawn_on_its_grid_matches_the_one_it_replaces() {
        let detail = crate::dj::demo_detail(0);
        let c = Color::from_rgb_u8;
        let colors = Colors {
            bg: c(0, 0, 0),
            low: c(80, 80, 80),
            mid: c(150, 150, 150),
            high: c(230, 230, 230),
            beat: c(60, 60, 60),
            bar: c(120, 120, 120),
            accent: c(255, 140, 0),
            clip: c(255, 0, 0),
        };
        let px: f64 = 16.0 / 2400.0;
        let strip = |at: f64| {
            let v = View {
                detail: &detail,
                at,
                span: 16.0,
                grid: None,
                cues: [None; crate::dj::HOT_CUES],
                looping: None,
                loop_on: false,
                playhead: false,
                style: Style::Classic,
            };
            render(2400, 112, &v, &colors)
        };
        for start in [37.0f64, 61.3, 120.77] {
            let a = (start / px).round() * px;
            let (old, new) = (strip(a), strip(a + 525.0 * px));
            let (o, n) = (old.as_slice(), new.as_slice());
            for y in 0..112 {
                for x in 0..2400 - 525 {
                    assert_eq!(n[y * 2400 + x], o[y * 2400 + x + 525], "x {x} y {y} from {start}");
                }
            }
        }
    }
}
