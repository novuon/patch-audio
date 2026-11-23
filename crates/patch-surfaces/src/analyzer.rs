//! The Analyzer Unit: a 1/6-octave real-time analyser, vectorscope, phase
//! correlation, balance, width and per-channel RMS / peak / crest / DC of the
//! program, as measured by the engine (`patch-meter`'s analyzer, fed from
//! the program ring). The engine only computes it while an Analyzer window
//! asks for it.
//!
//! Without a live engine (simulated provider, screenshot runs) the same
//! measurements run here on a synthetic program, and the window says so.
//!
//! The RTA and the vectorscope are drawn in Rust into pixel images at the
//! window's physical size, so lines stay crisp and thousands of points cost
//! far less than UI elements.

use patch_meter::analyzer::{Rta, Scope, StereoMeter};
use serde_json::Value;
use slint::{Color, Rgba8Pixel, SharedPixelBuffer};

/// Display range of the RTA, dBFS.
pub const TOP_DB: f32 = 0.0;
pub const FLOOR_DB: f32 = -90.0;

/// One measurement, from the engine or the local simulation.
#[derive(Clone, Debug, Default)]
pub struct View {
    pub correlation: f32,
    pub balance_db: f32,
    pub width_db: f32,
    pub rms: [f32; 2],
    pub peak: [f32; 2],
    pub crest: [f32; 2],
    pub dc: [f32; 2],
    pub mid: f32,
    pub side: f32,
    pub bands: Vec<f32>,
    pub fast: Vec<f32>,
    pub slow: Vec<f32>,
    pub hold: Vec<f32>,
    /// Vectorscope points (side, mid), full scale = ±1.
    pub scope: Vec<[f32; 2]>,
    pub rate: u32,
}

impl View {
    /// Parse the engine's `analyzer` reply; `None` while it has no data yet.
    pub fn from_json(v: &Value) -> Option<View> {
        let f = |x: &Value| x.as_f64().unwrap_or(-140.0) as f32;
        let pair = |x: &Value| [f(&x[0]), f(&x[1])];
        let list = |x: &Value| {
            x.as_array()
                .map(|a| a.iter().map(f).collect())
                .unwrap_or_default()
        };
        let view = View {
            correlation: v["correlation"].as_f64().unwrap_or(0.0) as f32,
            balance_db: v["balance_db"].as_f64().unwrap_or(0.0) as f32,
            width_db: f(&v["width_db"]),
            rms: pair(&v["rms"]),
            peak: pair(&v["peak"]),
            crest: pair(&v["crest"]),
            dc: pair(&v["dc"]),
            mid: f(&v["mid"]),
            side: f(&v["side"]),
            bands: list(&v["bands"]),
            fast: list(&v["fast"]),
            slow: list(&v["slow"]),
            hold: list(&v["hold"]),
            scope: match v["scope16"].as_str() {
                Some(packed) => patch_core::pack::unpack_i16(packed)
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|p| [p[0] as f32 / 1e4, p[1] as f32 / 1e4])
                    .collect(),
                // An engine from before the packed scope.
                None => v["scope"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|p| {
                                [
                                    p[0].as_f64().unwrap_or(0.0) as f32,
                                    p[1].as_f64().unwrap_or(0.0) as f32,
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            },
            rate: v["rate"].as_u64().unwrap_or(0) as u32,
        };
        (view.rate != 0 && !view.bands.is_empty()).then_some(view)
    }

    /// Is there any signal at all (above −100 dBFS RMS)?
    pub fn has_signal(&self) -> bool {
        self.rms[0].max(self.rms[1]) > -100.0
    }
}

/// A synthetic program for when there is no live engine: pink-ish noise
/// (Paul Kellet's filter) with a decorrelated stereo component, a 55 Hz
/// bass tone and a 2.2 kHz lead; the mix sits ½ dB left. Deterministic.
pub struct Sim {
    stereo: StereoMeter,
    rta: Rta,
    scope: Scope,
    seed: u32,
    b: [[f32; 7]; 2],
    hp: [(f32, f32); 2],
    t: u64,
}

impl Sim {
    pub const RATE: u32 = 48_000;

    pub fn new() -> Sim {
        let mut s = Sim {
            stereo: StereoMeter::new(Self::RATE),
            rta: Rta::new(Self::RATE),
            scope: Scope::new(Self::RATE as usize / 25),
            seed: 0x1234_5678,
            b: [[0.0; 7]; 2],
            hp: [(0.0, 0.0); 2],
            t: 0,
        };
        // Settled from the first frame (screenshots show a steady picture).
        s.run(3.0);
        s
    }

    fn white(&mut self) -> f32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        self.seed as f32 / u32::MAX as f32 * 2.0 - 1.0
    }

    fn pink(&mut self, c: usize) -> f32 {
        let w = self.white();
        let b = &mut self.b[c];
        b[0] = 0.99886 * b[0] + w * 0.0555179;
        b[1] = 0.99332 * b[1] + w * 0.0750759;
        b[2] = 0.96900 * b[2] + w * 0.153_852;
        b[3] = 0.86650 * b[3] + w * 0.3104856;
        b[4] = 0.55000 * b[4] + w * 0.5329522;
        b[5] = -0.7616 * b[5] - w * 0.0168980;
        let p = b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + w * 0.5362;
        b[6] = w * 0.115926;
        p * 0.11
    }

    fn pink_ac(&mut self, c: usize) -> f32 {
        // One-pole DC blocker (≈ 5 Hz): the pink filter drifts.
        let x = self.pink(c);
        let y = x - self.hp[c].0 + 0.9993 * self.hp[c].1;
        self.hp[c] = (x, y);
        y
    }

    /// Advance the program by `secs` (at most 0.25 s per call).
    pub fn run(&mut self, secs: f64) {
        let n = (secs.min(if self.t == 0 { 3.0 } else { 0.25 }) * Self::RATE as f64) as usize;
        let mut buf = Vec::with_capacity(n * 2);
        for _ in 0..n {
            let t = self.t as f64 / Self::RATE as f64;
            self.t += 1;
            let common = self.pink_ac(0);
            let wide = self.pink_ac(1) * 0.45;
            let bass = (std::f64::consts::TAU * 55.0 * t).sin() as f32
                * 0.22
                * (0.6 + 0.4 * (std::f64::consts::TAU * 2.0 * t).sin().max(0.0) as f32);
            let lead = (std::f64::consts::TAU * 2200.0 * t).sin() as f32 * 0.04;
            buf.push(0.5 * common + wide + bass + lead * 1.2);
            buf.push((0.5 * common - wide + bass + lead * 0.8) * 0.94);
        }
        for block in buf.chunks(1920) {
            self.stereo.push(block);
            self.rta.push(block);
            self.scope.push(block);
        }
    }

    pub fn view(&self) -> View {
        let st = self.stereo.reading();
        let r = self.rta.reading();
        let f = |x: f64| if x.is_finite() { x as f32 } else { -140.0 };
        View {
            correlation: st.correlation as f32,
            balance_db: f(st.balance_db),
            width_db: f(st.width_db),
            rms: st.rms_dbfs.map(f),
            peak: st.peak_dbfs.map(f),
            crest: st.crest_db.map(f),
            dc: st.dc_dbfs.map(f),
            mid: f(st.mid_dbfs),
            side: f(st.side_dbfs),
            bands: r.centres.iter().map(|c| *c as f32).collect(),
            fast: r.fast,
            slow: r.slow,
            hold: r.peak,
            scope: self.scope.points(),
            rate: Self::RATE,
        }
    }
}

pub struct Colors {
    pub bg: Color,
    pub grid: Color,
    pub grid_strong: Color,
    pub bar: Color,
    pub bar_hot: Color,
    /// Each bar's top: the accent, fading into `bar_hot` and then `bar`.
    pub top: Color,
    pub line: Color,
    pub hold: Color,
    pub over: Color,
}

fn px(c: Color) -> Rgba8Pixel {
    Rgba8Pixel {
        r: c.red(),
        g: c.green(),
        b: c.blue(),
        a: 255,
    }
}

fn mix(a: Rgba8Pixel, b: Color, alpha: f32) -> Rgba8Pixel {
    let m = |x: u8, y: u8| {
        (x as f32 + (y as f32 - x as f32) * alpha)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Rgba8Pixel {
        r: m(a.r, b.red()),
        g: m(a.g, b.green()),
        b: m(a.b, b.blue()),
        a: 255,
    }
}

/// y (0 = top) of a level in an RTA of height `h`.
pub fn level_y(db: f32, h: usize) -> f32 {
    ((TOP_DB - db) / (TOP_DB - FLOOR_DB)).clamp(0.0, 1.0) * h as f32
}

/// Horizontal position (0…1) of a frequency on the band axis.
pub fn freq_pos(hz: f32, bands: &[f32]) -> f32 {
    let (Some(first), n) = (bands.first(), bands.len()) else {
        return 0.0;
    };
    ((hz / first).log2() * 6.0 + 0.5) / n.max(1) as f32
}

/// The RTA: band bars (fast or slow ballistics), the other ballistics as
/// a line, peak-hold ticks, a 10 dB grid and octave lines.
pub fn render_rta(
    width: u32,
    height: u32,
    v: &View,
    bars_fast: bool,
    c: &Colors,
) -> SharedPixelBuffer<Rgba8Pixel> {
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(width.max(1), height.max(1));
    let (w, h) = (width.max(1) as usize, height.max(1) as usize);
    let p = buf.make_mut_slice();
    p.fill(px(c.bg));
    // Grid: every 10 dB (every 30 dB stronger), octave centres.
    let mut db = TOP_DB;
    while db >= FLOOR_DB {
        let y = (level_y(db, h - 1)).round() as usize;
        let col = if (db as i32) % 30 == 0 {
            c.grid_strong
        } else {
            c.grid
        };
        for x in 0..w {
            p[y * w + x] = px(col);
        }
        db -= 10.0;
    }
    for hz in [
        31.5f32, 63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
    ] {
        let x = (freq_pos(hz, &v.bands) * w as f32).round() as usize;
        if x < w {
            let col = if hz == 1000.0 { c.grid_strong } else { c.grid };
            for y in 0..h {
                p[y * w + x] = px(col);
            }
        }
    }
    let n = v.bands.len();
    if n == 0 {
        return buf;
    }
    let (bars, line) = if bars_fast {
        (&v.fast, &v.slow)
    } else {
        (&v.slow, &v.fast)
    };
    let span = |i: usize| {
        let (a, b) = (i * w / n, (i + 1) * w / n);
        // 1 px gap between bars once they are wide enough.
        (a, if b - a >= 4 { b - 1 } else { b })
    };
    for i in 0..n.min(bars.len()) {
        let (x0, x1) = span(i);
        let top = level_y(bars[i], h) as usize;
        for y in top..h {
            // The accent at the top, so the level reads at the bar's edge,
            // into the quiet body and darker toward the floor.
            let depth = (y - top) as f32 / h as f32;
            let col = if bars[i] > -6.0 && y < level_y(-6.0, h) as usize {
                px(c.over)
            } else if depth < 0.15 {
                mix(px(c.top), c.bar_hot, depth / 0.15)
            } else {
                // Down to the floor over the bar's own length: every bar
                // ends in the panel's colour, short or tall.
                let start = top as f32 + 0.15 * h as f32;
                let t = (y as f32 - start) / (h as f32 - start).max(1.0);
                mix(px(c.bar_hot), c.bar, t.clamp(0.0, 1.0))
            };
            for x in x0..x1 {
                p[y * w + x] = col;
            }
        }
    }
    // The other ballistics: a 2 px line through the band centres.
    let pts: Vec<(f32, f32)> = (0..n.min(line.len()))
        .map(|i| {
            (
                ((i as f32 + 0.5) / n as f32) * w as f32,
                level_y(line[i], h - 2),
            )
        })
        .collect();
    for seg in pts.windows(2) {
        let ((xa, ya), (xb, yb)) = (seg[0], seg[1]);
        let steps = (xb - xa).abs().max((yb - ya).abs()).ceil().max(1.0) as usize;
        for s in 0..=steps {
            let k = s as f32 / steps as f32;
            let (xf, yf) = (xa + (xb - xa) * k, ya + (yb - ya) * k);
            // Engine meters can briefly contain non-finite values during a
            // device change. Reject them before float-to-index conversion;
            // negative infinity casts to usize::MAX and `y + 1` can wrap.
            if !xf.is_finite() || !yf.is_finite() || line[0] <= FLOOR_DB - 50.0 {
                continue;
            }
            let x = (xf as usize).min(w - 1);
            let y = (yf as usize).min(h.saturating_sub(2));
            let i = y * w + x;
            if let Some(pixel) = p.get_mut(i) {
                *pixel = px(c.line);
            }
            if let Some(pixel) = p.get_mut(i + w) {
                *pixel = px(c.line);
            }
        }
    }
    // Peak hold: 2 px ticks.
    for i in 0..n.min(v.hold.len()) {
        if v.hold[i] <= FLOOR_DB {
            continue;
        }
        let (x0, x1) = span(i);
        let y = (level_y(v.hold[i], h) as usize).min(h - 2);
        for x in x0..x1 {
            p[y * w + x] = px(c.hold);
            p[(y + 1) * w + x] = px(c.hold);
        }
    }
    buf
}

/// The vectorscope (goniometer): mono vertical, left/right on the
/// diagonals (L upper left, as on a goniometer), out-of-phase horizontal. `gain` scales the trace (1 = full
/// scale at the edge); the trace is drawn as connected points with
/// additive intensity, so density reads as brightness.
pub fn render_scope(size: u32, v: &View, gain: f32, c: &Colors) -> SharedPixelBuffer<Rgba8Pixel> {
    let size = size.max(8);
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(size, size);
    let s = size as usize;
    let p = buf.make_mut_slice();
    p.fill(px(c.bg));
    let mid = (s as f32 - 1.0) / 2.0;
    let r = mid * 0.96;
    // Axes: M (vertical), S (horizontal), L and R diagonals; outer circle.
    for i in 0..s {
        let t = i as f32 - mid;
        for (x, y, col) in [
            (mid, i as f32, c.grid_strong),
            (i as f32, mid, c.grid),
            (
                mid + t * std::f32::consts::FRAC_1_SQRT_2,
                mid - t * std::f32::consts::FRAC_1_SQRT_2,
                c.grid,
            ),
            (
                mid + t * std::f32::consts::FRAC_1_SQRT_2,
                mid + t * std::f32::consts::FRAC_1_SQRT_2,
                c.grid,
            ),
        ] {
            let (x, y) = (x.round() as usize, y.round() as usize);
            if x < s && y < s && ((x as f32 - mid).powi(2) + (y as f32 - mid).powi(2)).sqrt() <= r {
                p[y * s + x] = px(col);
            }
        }
    }
    for k in 0..(s * 4) {
        let a = k as f32 / (s * 4) as f32 * std::f32::consts::TAU;
        let (x, y) = (
            (mid + r * a.cos()).round() as usize,
            (mid + r * a.sin()).round() as usize,
        );
        if x < s && y < s {
            p[y * s + x] = px(c.grid);
        }
    }
    // Intensity accumulation, then tone-mapped onto the trace colour.
    let mut acc = vec![0.0f32; s * s];
    let plot = |acc: &mut Vec<f32>, x: f32, y: f32| {
        let (xi, yi) = (x.round() as isize, y.round() as isize);
        if xi >= 0 && yi >= 0 && (xi as usize) < s && (yi as usize) < s {
            acc[yi as usize * s + xi as usize] += 1.0;
        }
    };
    let to_xy = |pt: [f32; 2]| {
        (
            mid - (pt[0] * gain).clamp(-1.05, 1.05) * r,
            mid - (pt[1] * gain).clamp(-1.05, 1.05) * r,
        )
    };
    let mut last: Option<(f32, f32)> = None;
    for &pt in &v.scope {
        let (x, y) = to_xy(pt);
        if let Some((lx, ly)) = last {
            let steps = ((x - lx).abs().max((y - ly).abs()).ceil() as usize).clamp(1, s);
            for k in 1..=steps {
                let t = k as f32 / steps as f32;
                plot(&mut acc, lx + (x - lx) * t, ly + (y - ly) * t);
            }
        } else {
            plot(&mut acc, x, y);
        }
        last = Some((x, y));
    }
    // Normalise by the busy pixels so the picture holds its brightness
    // whatever the level and point count.
    let mut busy: Vec<f32> = acc.iter().copied().filter(|a| *a > 0.0).collect();
    if busy.is_empty() {
        return buf;
    }
    let k = busy.len() * 9 / 10;
    let norm = *busy.select_nth_unstable_by(k, f32::total_cmp).1;
    for (i, a) in acc.iter().enumerate() {
        if *a > 0.0 {
            let v = (a / norm.max(1.0)).sqrt().min(1.0);
            p[i] = mix(p[i], c.line, 0.25 + 0.75 * v);
        }
    }
    buf
}

/// Auto gain for the vectorscope: bring the louder of M/S to about −6 dB
/// of the edge, between 0 and +30 dB, in 6 dB steps (so it does not pump).
pub fn auto_gain(v: &View) -> f32 {
    let loud = v.rms[0].max(v.rms[1]);
    if loud < -100.0 {
        return 1.0;
    }
    let want = (-9.0 - loud).clamp(0.0, 30.0);
    10f32.powf((want / 6.0).floor() * 6.0 / 20.0)
}

pub fn db_text(db: f32) -> String {
    if db <= -100.0 {
        "−∞".into()
    } else {
        crate::num::signed(db, 1).replace('-', "−")
    }
}

/// DC offset: −∞ when there is none to speak of (below −96 dBFS).
pub fn dc_text(db: f32) -> String {
    if db <= -96.0 {
        "none".into()
    } else {
        db_text(db)
    }
}

pub fn freq_text(hz: f32) -> String {
    if hz >= 1000.0 {
        let k = hz / 1000.0;
        if (k - k.round()).abs() < 0.05 {
            format!("{}k", k.round())
        } else {
            format!("{k:.1}k")
        }
    } else if hz < 100.0 && hz.fract() != 0.0 {
        format!("{hz:.1}")
    } else {
        format!("{}", hz.round())
    }
}

/// Correlation readout with its meaning.
pub fn correlation_text(c: f32, signal: bool) -> String {
    if !signal {
        return "no signal".into();
    }
    let what = if c > 0.995 {
        "mono"
    } else if c > 0.9 {
        "narrow stereo"
    } else if c > 0.3 {
        "stereo"
    } else if c > -0.2 {
        "wide"
    } else {
        "OUT OF PHASE"
    };
    format!("{} · {what}", crate::num::signed(c, 2)).replace('-', "−")
}

pub fn balance_text(b: f32, signal: bool) -> String {
    if !signal {
        return "—".into();
    }
    if b.abs() < 0.05 {
        "centre".into()
    } else {
        format!("{} {:.1} dB", if b > 0.0 { "L" } else { "R" }, b.abs())
    }
}

pub fn width_text(w: f32, signal: bool) -> String {
    if !signal {
        "—".into()
    } else if w <= -60.0 {
        "S/M −∞ (mono)".into()
    } else {
        format!("S/M {} dB", db_text(w))
    }
}

/// The Analyzer windows' state in the app: polls the engine at 30 Hz
/// while a window is open (which keeps the engine's analyzer awake),
/// renders the images per window at its physical size, and sets the text
/// at 5 Hz so numbers stay readable.
#[derive(Default)]
pub struct Panel {
    sim: std::cell::RefCell<Option<Sim>>,
    polls: crate::poll::Poller,
    view: std::cell::RefCell<Option<View>>,
    /// Engine unreachable on the last poll.
    offline: std::cell::Cell<bool>,
    /// The engine runs but predates the Analyzer (restart it to update).
    outdated: std::cell::Cell<bool>,
    last_poll: std::cell::Cell<f64>,
    last_text: std::cell::Cell<f64>,
    slow_bars: std::cell::Cell<bool>,
    auto_gain: std::cell::Cell<bool>,
    reset: std::cell::Cell<bool>,
    /// Engine snapshots, and how late to show them: the output path's
    /// latency, so the Analyzer shows what is heard, in step with the
    /// meters (the same delay the Meter uses).
    history: std::cell::RefCell<std::collections::VecDeque<(std::time::Instant, View)>>,
    delay_ms: std::cell::Cell<f32>,
    /// The engine's latency reply (polled twice a second) and the note
    /// from the last button press.
    latency: std::cell::RefCell<Option<Value>>,
    last_latency: std::cell::Cell<f64>,
    lat_note: std::cell::RefCell<String>,
    /// When the program was last above silence (for the slow poll).
    heard_at: std::cell::Cell<f64>,
}

/// Output latency lines: (level, text, note, busy, saved) from the engine's
/// `latency` reply.
pub fn latency_lines(v: Option<&Value>, now_s: u64) -> (String, String, String, bool, bool) {
    let Some(v) = v else {
        return (
            "".into(),
            "needs the engine".into(),
            String::new(),
            false,
            false,
        );
    };
    let m = &v["measure"];
    let phase = m["phase"].as_str().unwrap_or("idle");
    let output = v["output"].as_str().unwrap_or("");
    let saved = &v["saved"][output];
    let busy = phase == "measuring";
    let note = match phase {
        "measuring" => format!(
            "Playing 4 test sweeps (≈ 5 s, program muted) · hold the headphones or speaker close to {}",
            m["mic"].as_str().map(mic_name).unwrap_or_else(|| "the microphone".into())
        ),
        "failed" => format!("Measurement failed: {}", m["error"].as_str().unwrap_or("unknown")),
        _ if saved.is_object() => String::new(),
        _ => "Measure plays short sweeps and listens with a microphone; meters then show what you hear, when you hear it.".into(),
    };
    if let Some(ms) = saved["output_ms"].as_f64() {
        let ago = now_s.saturating_sub(saved["at"].as_u64().unwrap_or(now_s));
        let when = match ago {
            0..=59 => "just now".to_string(),
            60..=3599 => format!("{} min ago", ago / 60),
            3600..=86_399 => format!("{} h ago", ago / 3600),
            _ => format!("{} d ago", ago / 86_400),
        };
        let text = format!(
            "{ms:.1} ms ±{:.1} · round trip {:.1} − mic {:.1} · {when}",
            saved["spread_ms"].as_f64().unwrap_or(0.0) / 2.0,
            saved["round_trip_ms"].as_f64().unwrap_or(0.0),
            saved["input_ms"].as_f64().unwrap_or(0.0),
        );
        ("MEASURED".into(), text, note, busy, true)
    } else {
        (
            "REPORTED".into(),
            "not measured · meters use the latency PipeWire reports".into(),
            note,
            busy,
            false,
        )
    }
}

/// "alsa_input.usb-046d_0825_…" → "the USB microphone", roughly.
fn mic_name(node: &str) -> String {
    if node.contains("usb") {
        "the USB microphone".into()
    } else if node.starts_with("alsa_input") {
        "the built-in microphone".into()
    } else {
        node.into()
    }
}

impl Panel {
    pub fn bind(self: &std::rc::Rc<Self>, w: &crate::ui::PatchWindow) {
        use slint::ComponentHandle;
        let a = w.global::<crate::ui::AnalyzerState>();
        let bands: Vec<f32> = patch_meter::analyzer::band_centres()
            .iter()
            .map(|c| *c as f32)
            .collect();
        let labels: Vec<crate::ui::AnLabel> = [
            31.5f32, 63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
        ]
        .iter()
        .map(|hz| crate::ui::AnLabel {
            text: freq_text(*hz).into(),
            pos: freq_pos(*hz, &bands),
        })
        .collect();
        a.set_freq_labels(std::rc::Rc::new(slint::VecModel::from(labels)).into());
        a.set_bars_fast(!self.slow_bars.get());
        let me = self.clone();
        a.on_toggle_fast(move || me.slow_bars.set(!me.slow_bars.get()));
        let me = self.clone();
        a.on_reset_hold(move || me.reset.set(true));
        let me = self.clone();
        a.on_toggle_gain(move || me.auto_gain.set(!me.auto_gain.get()));
        let me = self.clone();
        a.on_measure_latency(move || {
            let note = match patch_engine::client::call(
                &serde_json::json!({"op": "latency", "do": "measure"}),
            ) {
                Ok(_) => String::new(),
                Err(e) => format!("Can't measure: {e}"),
            };
            *me.lat_note.borrow_mut() = note;
            me.last_latency.set(f64::NEG_INFINITY);
        });
        let me = self.clone();
        a.on_forget_latency(move || {
            let note = match patch_engine::client::call(
                &serde_json::json!({"op": "latency", "do": "forget"}),
            ) {
                Ok(_) => "Forgotten · meters use the reported latency again".to_string(),
                Err(e) => format!("Can't forget: {e}"),
            };
            *me.lat_note.borrow_mut() = note;
            me.last_latency.set(f64::NEG_INFINITY);
        });
        // Draw straight away (no blank first frame).
        self.last_poll.set(f64::NEG_INFINITY);
        self.last_text.set(f64::NEG_INFINITY);
    }

    /// Per frame. `live`: a real engine should be there; otherwise (and in
    /// screenshot runs) the simulated program is analysed here.
    /// Show engine snapshots this late (the meters' output delay, ms).
    pub fn set_delay_ms(&self, ms: f32) {
        self.delay_ms.set(ms.clamp(0.0, 2500.0));
    }

    pub fn tick(&self, wins: &[crate::ui::PatchWindow], t: f64, live: bool, frozen: bool) {
        use slint::ComponentHandle;
        if wins.is_empty() {
            *self.sim.borrow_mut() = None;
            return;
        }
        // 30 Hz with signal; after a second of silence 10 Hz is plenty.
        let quiet = t - self.heard_at.get() > 1.0;
        if t - self.last_poll.get() < if quiet { 0.1 } else { 1.0 / 30.0 } {
            return;
        }
        let dt = (t - self.last_poll.get()).clamp(0.0, 0.25);
        self.last_poll.set(t);
        let simulated = frozen || !live;
        if simulated {
            let mut sim = self.sim.borrow_mut();
            let sim = sim.get_or_insert_with(Sim::new);
            if !frozen {
                sim.run(dt);
            }
            if self.reset.take() {
                sim.rta.reset_peaks();
            }
            *self.view.borrow_mut() = Some(sim.view());
            self.offline.set(false);
        } else {
            let req = serde_json::json!({"op": "analyzer", "reset": self.reset.get()});
            if self.polls.request("analyzer", req) {
                self.reset.set(false);
            }
            if let Some(reply) = self.polls.take("analyzer") {
                match reply.result {
                    Ok(v) => {
                        self.offline.set(false);
                        if let Some(view) = View::from_json(&v) {
                            let now = std::time::Instant::now();
                            let mut h = self.history.borrow_mut();
                            h.push_back((reply.at, view));
                            while h
                                .front()
                                .is_some_and(|(at, _)| now.duration_since(*at).as_secs_f64() > 3.0)
                            {
                                h.pop_front();
                            }
                            // The newest snapshot that is being heard by now.
                            let due = now
                                .checked_sub(std::time::Duration::from_secs_f64(
                                    self.delay_ms.get() as f64 / 1000.0,
                                ))
                                .unwrap_or(now);
                            if let Some((_, v)) = h.iter().rev().find(|(at, _)| *at <= due) {
                                *self.view.borrow_mut() = Some(v.clone());
                            }
                        }
                    }
                    Err(e) => {
                        self.offline.set(true);
                        self.outdated.set(e.contains("unknown op"));
                        // Keep the last measured frame while reconnecting.
                    }
                }
            }
            // Every 3 s; twice a second while a measurement runs.
            let measuring = self.latency.borrow().as_ref().is_some_and(|v| {
                v["measure"]["phase"]
                    .as_str()
                    .is_some_and(|p| p != "idle" && p != "done")
            });
            if t - self.last_latency.get() >= if measuring { 0.5 } else { 3.0 } {
                self.last_latency.set(t);
                self.polls
                    .request("latency", serde_json::json!({"op": "latency"}));
            }
            if let Some(reply) = self.polls.take("latency") {
                let r = reply.result.ok();
                // A finished measurement replaces the button's note.
                if r.as_ref().is_some_and(|v| v["measure"]["phase"] != "idle") {
                    self.lat_note.borrow_mut().clear();
                }
                *self.latency.borrow_mut() = r;
            }
        }
        let view = self.view.borrow();
        let empty = View::default();
        let v = view.as_ref().unwrap_or(&empty);
        let signal = v.has_signal();
        if signal {
            self.heard_at.set(t);
        }
        let gain = if self.auto_gain.get() {
            auto_gain(v)
        } else {
            1.0
        };
        let text = t - self.last_text.get() >= 0.2;
        if text {
            self.last_text.set(t);
        }
        for w in wins {
            let th = w.global::<crate::ui::Theme>();
            let colors = Colors {
                bg: th.get_bg(),
                // A quiet grid: the bars carry the picture.
                grid: th.get_border().mix(&th.get_bg(), 0.5),
                grid_strong: th.get_border(),
                // The body fades into the panel: no dark grey pooling at
                // the floor.
                bar: th.get_bg().mix(&th.get_text_faint(), 0.85),
                bar_hot: th.get_text_faint().mix(&th.get_accent(), 0.6),
                top: th.get_accent(),
                line: th.get_text_bright(),
                hold: th.get_accent(),
                over: th.get_error(),
            };
            let scale = w.window().scale_factor();
            let a = w.global::<crate::ui::AnalyzerState>();
            let (rw, rh) = (
                (a.get_rta_w() * scale).round().max(1.0) as u32,
                (a.get_rta_h() * scale).round().max(1.0) as u32,
            );
            let ss = (a.get_scope_size() * scale).round().max(8.0) as u32;
            // Silence holds still: draw only when what is shown changes.
            let key = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                for x in v
                    .fast
                    .iter()
                    .chain(&v.slow)
                    .chain(&v.hold)
                    .chain(v.scope.iter().flatten())
                {
                    x.to_bits().hash(&mut h);
                }
                (
                    rw,
                    rh,
                    ss,
                    gain.to_bits(),
                    self.slow_bars.get(),
                    colors.bg.as_argb_encoded(),
                    colors.hold.as_argb_encoded(),
                )
                    .hash(&mut h);
                h.finish()
            };
            // Kept in the window (a reopened window draws again).
            let key = (key as i64 & 0x7fff_ffff) as i32;
            if a.get_drawn_key() != key {
                a.set_drawn_key(key);
                a.set_rta(slint::Image::from_rgba8(render_rta(
                    rw.min(4096),
                    rh.min(2048),
                    v,
                    !self.slow_bars.get(),
                    &colors,
                )));
                a.set_scope(slint::Image::from_rgba8(render_scope(
                    ss.min(2048),
                    v,
                    gain,
                    // The scope keeps its dark well; the RTA sits on the panel.
                    &Colors {
                        bg: th.get_bg_deeper(),
                        ..colors
                    },
                )));
            }
            a.set_correlation(if signal { v.correlation } else { 0.0 });
            a.set_balance(if signal { v.balance_db } else { 0.0 });
            a.set_bars_fast(!self.slow_bars.get());
            if !text {
                continue;
            }
            a.set_source(
                if simulated {
                    "SIMULATED SIGNAL"
                } else if self.outdated.get() && self.offline.get() {
                    "ENGINE OUTDATED"
                } else if self.offline.get() {
                    "NO ENGINE"
                } else {
                    "LIVE SIGNAL"
                }
                .into(),
            );
            a.set_message(
                if self.offline.get() && self.outdated.get() {
                    "The running engine predates the Analyzer · `omarchy-patch engine upgrade` swaps it without a gap"
                } else if self.offline.get() {
                    "Engine not running · nothing to analyse"
                } else if view.is_none() {
                    "Starting the analyzer…"
                } else if !signal {
                    "Silence · program below −100 dBFS"
                } else {
                    ""
                }
                .into(),
            );
            a.set_status(
                format!(
                    "program{} · 1/6 oct · {}",
                    if simulated { " (simulated)" } else { "" },
                    if v.rate > 0 {
                        format!("{:.1} kHz", v.rate as f32 / 1000.0)
                    } else {
                        "—".into()
                    }
                )
                .into(),
            );
            a.set_corr_text(correlation_text(v.correlation, signal).into());
            a.set_bal_text(balance_text(v.balance_db, signal).into());
            a.set_width_text(width_text(v.width_db, signal).into());
            a.set_ms_text(format!("M {} · S {} dBFS", db_text(v.mid), db_text(v.side)).into());
            a.set_gain_text(
                (if self.auto_gain.get() {
                    format!("auto +{:.0} dB", 20.0 * gain.log10())
                } else {
                    "0 dB".into()
                })
                .into(),
            );
            let row = |key: &str, l: String, r: String, warn: bool| crate::ui::AnStat {
                key: key.into(),
                l: l.into(),
                r: r.into(),
                warn,
            };
            let stats = vec![
                row("RMS", db_text(v.rms[0]), db_text(v.rms[1]), false),
                row(
                    "peak",
                    db_text(v.peak[0]),
                    db_text(v.peak[1]),
                    v.peak[0].max(v.peak[1]) > -0.1,
                ),
                row(
                    "crest",
                    if signal {
                        format!("{:.1}", v.crest[0])
                    } else {
                        "—".into()
                    },
                    if signal {
                        format!("{:.1}", v.crest[1])
                    } else {
                        "—".into()
                    },
                    false,
                ),
                // Above −60 dBFS DC is worth fixing (it eats headroom).
                row(
                    "DC",
                    dc_text(v.dc[0]),
                    dc_text(v.dc[1]),
                    v.dc[0].max(v.dc[1]) > -60.0,
                ),
            ];
            a.set_stats(std::rc::Rc::new(slint::VecModel::from(stats)).into());
            let lat = self.latency.borrow();
            let now_s = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let (level, text, note, busy, saved) = latency_lines(lat.as_ref(), now_s);
            let extra = self.lat_note.borrow();
            a.set_lat_level(level.into());
            a.set_lat_text(text.into());
            a.set_lat_note((if note.is_empty() { extra.clone() } else { note }).into());
            a.set_lat_busy(busy);
            a.set_lat_saved(saved);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colors() -> Colors {
        let g = |v: u8| Color::from_rgb_u8(v, v, v);
        Colors {
            bg: g(0),
            grid: g(30),
            grid_strong: g(50),
            bar: g(90),
            bar_hot: g(200),
            top: g(220),
            line: g(250),
            hold: Color::from_rgb_u8(255, 140, 0),
            over: Color::from_rgb_u8(255, 0, 0),
        }
    }

    fn view(level: f32) -> View {
        let bands: Vec<f32> = patch_meter::analyzer::band_centres()
            .iter()
            .map(|c| *c as f32)
            .collect();
        let n = bands.len();
        View {
            bands,
            fast: vec![level; n],
            slow: vec![level; n],
            hold: vec![level + 10.0; n],
            rate: 48_000,
            ..Default::default()
        }
    }

    #[test]
    fn bars_reach_their_level_and_hold_ticks_sit_above() {
        let img = render_rta(590, 180, &view(-45.0), true, &colors());
        let p = img.as_slice();
        // Band 20 spans x 200…209; −45 dBFS is half height (y 90).
        assert!(
            p[95 * 590 + 204].r >= 90 && p[85 * 590 + 204].r < 90,
            "bar top at y≈90"
        );
        // Hold at −35 dBFS: y = 70.
        let tick = p[70 * 590 + 204];
        assert_eq!((tick.r, tick.g), (255, 140));
    }

    #[test]
    fn engine_reply_parses() {
        let v = serde_json::json!({
            "rate": 48000, "correlation": 0.8, "balance_db": 1.5, "width_db": -12.0,
            "rms": [-18.0, -19.5], "peak": [-3.0, -4.0], "crest": [18.0, 18.5], "dc": [-140.0, -80.0],
            "mid": -18.0, "side": -30.0, "bands": [22.0, 25.0], "fast": [-40.0, -41.0], "slow": [-42.0, -43.0], "hold": [-30.0, -31.0],
            "scope": [[0.1, 0.2], [0.0, -0.3]],
        });
        let view = View::from_json(&v).unwrap();
        assert_eq!(view.scope.len(), 2);
        assert!(view.has_signal() && (view.correlation - 0.8).abs() < 1e-6);
        assert!(View::from_json(&serde_json::json!({"rate": 0})).is_none());
    }

    #[test]
    fn the_simulated_program_measures_plausibly() {
        let v = Sim::new().view();
        assert!(
            v.correlation > 0.2 && v.correlation < 0.95,
            "correlation {}",
            v.correlation
        );
        assert!(v.rms[0] > -30.0 && v.rms[0] < -6.0, "rms {:?}", v.rms);
        assert!(v.balance_db > 0.0, "lead is left of centre");
        let i55 = v
            .bands
            .iter()
            .position(|b| (b / 55.0).log2().abs() < 1.0 / 12.0)
            .unwrap();
        assert!(
            v.slow[i55] > v.slow[i55 + 3] + 6.0,
            "the bass tone stands out"
        );
    }

    #[test]
    fn scope_draws_mono_vertically() {
        let mut v = view(-20.0);
        v.scope = (0..2000)
            .map(|i| [0.0, (i as f32 * 0.01).sin() * 0.8])
            .collect();
        let img = render_scope(101, &v, 1.0, &colors());
        let p = img.as_slice();
        assert!(p[20 * 101 + 50].r > 200, "trace on the M axis");
        assert!(p[50 * 101 + 20].r < 60, "nothing on the S axis");
    }

    #[test]
    fn renders_fast() {
        let mut v = view(-20.0);
        v.scope = Sim::new().view().scope;
        let t = std::time::Instant::now();
        for _ in 0..20 {
            std::hint::black_box(render_rta(1400, 500, &v, true, &colors()));
            std::hint::black_box(render_scope(420, &v, 1.0, &colors()));
        }
        let ms = t.elapsed().as_secs_f64() * 1e3 / 20.0;
        assert!(ms < 6.0, "one frame took {ms:.2} ms");
    }

    #[test]
    fn labels() {
        assert_eq!(freq_text(1000.0), "1k");
        assert_eq!(freq_text(31.5), "31.5");
        assert_eq!(freq_text(16000.0), "16k");
        assert_eq!(correlation_text(-0.5, true), "−0.50 · OUT OF PHASE");
        assert_eq!(correlation_text(0.96, true), "+0.96 · narrow stereo");
        assert_eq!(balance_text(-2.04, true), "R 2.0 dB");
    }
}
