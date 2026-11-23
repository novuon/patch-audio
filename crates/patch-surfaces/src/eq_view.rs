//! EQ windows: the master EQ's response, drawn in Rust at physical
//! resolution, and the controls that change it (validated commands to the
//! service). Redrawn only when the bands, the size, the theme or (with the
//! spectrum shown, 15 times a second) the program's spectrum changed.

use patch_core::eq::{BANDS, EqBand, EqKind, EqState, FREQ_RANGE, GAIN_RANGE, Q_RANGE};
use patch_engine::master_eq::coefficients;
#[cfg(test)]
use patch_engine::master_eq::response_db;
use std::cell::{Cell, RefCell};
use std::time::Instant;

/// The plot's range: 20 Hz … 20 kHz, ±24 dB.
const DB_RANGE: f32 = 24.0;

pub fn x_of(hz: f32) -> f32 {
    ((hz / FREQ_RANGE.0).ln() / (FREQ_RANGE.1 / FREQ_RANGE.0).ln()).clamp(0.0, 1.0)
}

pub fn hz_of(x: f32) -> f32 {
    FREQ_RANGE.0 * (FREQ_RANGE.1 / FREQ_RANGE.0).powf(x.clamp(0.0, 1.0))
}

pub fn y_of(db: f32) -> f32 {
    (0.5 - db / (2.0 * DB_RANGE)).clamp(0.0, 1.0)
}

pub fn db_of(y: f32) -> f32 {
    (0.5 - y) * 2.0 * DB_RANGE
}

pub fn hz_text(hz: f32) -> String {
    if hz >= 1000.0 {
        let k = hz / 1000.0;
        if k >= 10.0 {
            format!("{k:.1} kHz")
        } else {
            format!("{k:.2} kHz")
        }
    } else {
        format!("{hz:.0} Hz")
    }
}

fn db_text(g: f32) -> String {
    format!("{} dB", crate::num::signed(g, 1)).replace('-', "−")
}

#[derive(Clone, Copy)]
pub struct Colors {
    pub band: [u8; 3],
    pub curve: [u8; 3],
    pub accent: [u8; 3],
    pub spectrum: [u8; 3],
}

/// Premultiplied RGBA8, composited in place: the image Slint shows as is
/// (no float canvas, no conversion pass).
struct Canvas<'a> {
    w: usize,
    h: usize,
    px: &'a mut [slint::Rgba8Pixel],
}

impl Canvas<'_> {
    #[inline]
    fn over(&mut self, x: i64, y: i64, c: [u8; 3], a: f32) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h || a <= 0.004 {
            return;
        }
        let p = &mut self.px[y as usize * self.w + x as usize];
        let a = (a.min(1.0) * 256.0) as u32;
        let k = 256 - a;
        let mix = |src: u8, dst: u8| ((src as u32 * a + dst as u32 * k) >> 8) as u8;
        p.r = mix(c[0], p.r);
        p.g = mix(c[1], p.g);
        p.b = mix(c[2], p.b);
        p.a = ((255 * a + p.a as u32 * k) >> 8) as u8;
    }
}

/// Render the response: grid, the program spectrum (optional), each band's
/// shape, the total curve and the band nodes.
pub fn render(
    w: usize,
    h: usize,
    st: &EqState,
    selected: usize,
    scale: f32,
    c: &Colors,
) -> slint::SharedPixelBuffer<slint::Rgba8Pixel> {
    let (w, h) = (w.max(2), h.max(2));
    let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(w as u32, h as u32);
    let mut cv = Canvas {
        w,
        h,
        px: buf.make_mut_slice(),
    };
    let (wf, hf) = (w as f32, h as f32);
    let rate = 48_000.0;
    // The grid is drawn by the view (static items); this image holds
    // what changes: the band shapes, the total curve and the nodes.
    // Responses per column: each band, and the total.
    let coef: Vec<_> = st
        .bands
        .iter()
        .map(|b| {
            if st.bypass {
                coefficients(&EqBand { on: false, ..*b }, rate)
            } else {
                coefficients(b, rate)
            }
        })
        .collect();
    let trig = column_trig(w, rate);
    let mut total = vec![0f32; w];
    let zero = y_of(0.0) * hf;
    for (i, b) in st.bands.iter().enumerate() {
        if !b.active() || st.bypass {
            continue;
        }
        let sel = i == selected;
        for x in 0..w {
            let db = response_at(&coef[i], &trig[x]);
            total[x] += db;
            // This band's shape: filled between 0 dB and its curve.
            let y = y_of(db.clamp(-DB_RANGE, DB_RANGE)) * hf;
            let (a, bnd) = (y.min(zero), y.max(zero));
            for yy in a.floor().max(0.0) as usize..(bnd.ceil() as usize).min(h) {
                let cov = ((yy as f32 + 1.0).min(bnd) - (yy as f32).max(a)).clamp(0.0, 1.0);
                cv.over(
                    x as i64,
                    yy as i64,
                    if sel { c.accent } else { c.band },
                    cov * if sel { 0.22 } else { 0.1 },
                );
            }
        }
    }
    // The total curve: an anti-aliased 1.8 px line in the accent; the
    // band nodes stay in the curve colour.
    let r = 0.9 * scale;
    let pts: Vec<(f32, f32)> = (0..w)
        .map(|x| {
            (
                x as f32 + 0.5,
                y_of(total[x].clamp(-DB_RANGE, DB_RANGE)) * hf,
            )
        })
        .collect();
    // Coverage, max-combined so segment joints don't double up; only the
    // rows the curve touches are visited when compositing.
    let mut cover = vec![0u8; w * h];
    let (mut y_lo, mut y_hi) = (h, 0);
    for s in pts.windows(2) {
        stroke(&mut cover, w, h, s[0], s[1], r);
        y_lo = y_lo.min((s[0].1.min(s[1].1) - r - 2.0).max(0.0) as usize);
        y_hi = y_hi.max(((s[0].1.max(s[1].1) + r + 2.0) as usize).min(h));
    }
    let dim = if st.bypass { 0.35 } else { 1.0 };
    for y in y_lo..y_hi {
        for x in 0..w {
            let v = cover[y * w + x];
            if v > 0 {
                cv.over(x as i64, y as i64, c.accent, v as f32 / 255.0 * dim);
            }
        }
    }
    // Nodes: a ring per band (filled when selected, hollow when off).
    for (i, b) in st.bands.iter().enumerate() {
        let (nx, ny) = node(b);
        let (px, py) = (nx * wf, ny * hf);
        let sel = i == selected;
        let col = if sel { c.accent } else { c.curve };
        let rad = if sel { 5.5 } else { 4.5 } * scale;
        if b.on && sel {
            disc(&mut cv, px, py, rad, col, 1.0);
        } else {
            // A ring: on bands solid, off bands faint.
            ring(
                &mut cv,
                px,
                py,
                rad,
                1.6 * scale,
                col,
                if b.on { 1.0 } else { 0.45 },
            );
        }
    }
    buf
}

/// The program's spectrum (after the EQ) as its own layer, behind the
/// response: a soft fill from the bottom, at the size given (half the
/// plot's resolution is plenty), written straight into bytes.
pub fn render_spectrum(
    w: usize,
    h: usize,
    sp: &[f32],
    c: [u8; 3],
) -> slint::SharedPixelBuffer<slint::Rgba8Pixel> {
    let (w, h) = (w.max(2), h.max(2));
    let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(w as u32, h as u32);
    if sp.len() < 3 {
        return buf;
    }
    let px = buf.make_mut_slice();
    let n = sp.len() as f32;
    let span = (20_000.0f32 / 25.0).ln();
    let fill = slint::Rgba8Pixel {
        r: c[0],
        g: c[1],
        b: c[2],
        a: 44,
    };
    for x in 0..w {
        // Bands are log-spaced 25 Hz … 20 kHz; −72 … 0 dBFS fills the height.
        let hz = hz_of((x as f32 + 0.5) / w as f32);
        let u = ((hz / 25.0).ln() / span * n - 0.5).clamp(0.0, n - 1.0);
        let (i, f) = (u.floor() as usize, u.fract());
        let v = sp[i] * (1.0 - f) + sp[(i + 1).min(sp.len() - 1)] * f;
        let top = h as f32 * (1.0 - ((v + 72.0) / 72.0).clamp(0.0, 1.0));
        let t = top.ceil() as usize;
        if t > 0 && t <= h {
            let a = ((t as f32 - top) * 44.0) as u8;
            px[(t - 1) * w + x] = slint::Rgba8Pixel { a, ..fill };
        }
        for y in t.min(h)..h {
            px[y * w + x] = fill;
        }
    }
    buf
}

/// cos ω, sin ω, cos 2ω, sin 2ω for each column's frequency: a band's
/// response per column is then a handful of multiplications.
fn column_trig(w: usize, rate: f64) -> Vec<[f64; 4]> {
    (0..w)
        .map(|x| {
            let hz = hz_of((x as f32 + 0.5) / w as f32) as f64;
            let om = std::f64::consts::TAU * hz / rate;
            [om.cos(), om.sin(), (2.0 * om).cos(), (2.0 * om).sin()]
        })
        .collect()
}

/// |H| in dB from the cached trig (same as `response_db`).
#[inline]
fn response_at(c: &patch_engine::master_eq::Coefficients, t: &[f64; 4]) -> f32 {
    let nr = c.b0 + c.b1 * t[0] + c.b2 * t[2];
    let ni = c.b1 * t[1] + c.b2 * t[3];
    let dr = 1.0 + c.a1 * t[0] + c.a2 * t[2];
    let di = c.a1 * t[1] + c.a2 * t[3];
    let m2 = (nr * nr + ni * ni) / (dr * dr + di * di).max(1e-300);
    (10.0 * m2.max(1e-24).log10()) as f32
}

/// Where a band's node sits in the plot (0…1): its frequency, and its gain
/// (bells and shelves) or the 0 dB line.
pub fn node(b: &EqBand) -> (f32, f32) {
    (
        x_of(b.freq),
        y_of(if b.kind.has_gain() { b.gain_db } else { 0.0 }),
    )
}

fn disc(cv: &mut Canvas, cx: f32, cy: f32, r: f32, c: [u8; 3], a: f32) {
    for y in (cy - r - 1.0).floor() as i64..=(cy + r + 1.0).ceil() as i64 {
        for x in (cx - r - 1.0).floor() as i64..=(cx + r + 1.0).ceil() as i64 {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            cv.over(x, y, c, a * (r + 0.5 - d).clamp(0.0, 1.0));
        }
    }
}

fn ring(cv: &mut Canvas, cx: f32, cy: f32, r: f32, width: f32, c: [u8; 3], a: f32) {
    let inner = r - width;
    for y in (cy - r - 1.0).floor() as i64..=(cy + r + 1.0).ceil() as i64 {
        for x in (cx - r - 1.0).floor() as i64..=(cx + r + 1.0).ceil() as i64 {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            let cov = (r + 0.5 - d).clamp(0.0, 1.0) - (inner + 0.5 - d).clamp(0.0, 1.0);
            cv.over(x, y, c, a * cov.max(0.0));
        }
    }
}

fn stroke(cover: &mut [u8], w: usize, h: usize, p0: (f32, f32), p1: (f32, f32), r: f32) {
    let (x0, x1) = (p0.0.min(p1.0) - r - 1.0, p0.0.max(p1.0) + r + 1.0);
    let (y0, y1) = (p0.1.min(p1.1) - r - 1.0, p0.1.max(p1.1) + r + 1.0);
    let (dx, dy) = (p1.0 - p0.0, p1.1 - p0.1);
    let len2 = (dx * dx + dy * dy).max(1e-9);
    for y in (y0.floor().max(0.0) as usize)..(y1.ceil().max(0.0) as usize).min(h) {
        for x in (x0.floor().max(0.0) as usize)..(x1.ceil().max(0.0) as usize).min(w) {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let t = (((px - p0.0) * dx + (py - p0.1) * dy) / len2).clamp(0.0, 1.0);
            let (qx, qy) = (p0.0 + t * dx - px, p0.1 + t * dy - py);
            let v = ((r + 0.5 - (qx * qx + qy * qy).sqrt()).clamp(0.0, 1.0) * 255.0) as u8;
            let i = y * w + x;
            if v > cover[i] {
                cover[i] = v;
            }
        }
    }
}

fn ui_path() -> std::path::PathBuf {
    patch_engine::session::state_dir().join("eq-ui.json")
}

/// The EQ windows' state: every target's settings as last seen or edited
/// here ("master", "bus", "deck-a", "deck-b").
pub struct Panel {
    polls: crate::poll::Poller,
    changed_at: Cell<Option<Instant>>,
    pub states: RefCell<std::collections::BTreeMap<String, EqState>>,
    /// Last local edit (while editing, the service's copy doesn't overwrite).
    edited: Cell<Option<Instant>>,
    last_poll: Cell<f64>,
    last_spectrum: Cell<f64>,
    pub spectrum: Cell<bool>,
    pub live: Cell<bool>,
    /// Presets as the service lists them: (id, name, built in).
    pub presets: RefCell<Vec<(String, String, bool)>>,
    /// Applications with their own EQ, and the ones playing now.
    pub apps: RefCell<Vec<String>>,
    pub app_labels: RefCell<std::collections::BTreeMap<String, String>>,
    pub app_channels: RefCell<std::collections::BTreeMap<String, String>>,
    pub mixer_labels: RefCell<std::collections::BTreeMap<String, String>>,
    pub playing: RefCell<Vec<String>>,
    last_presets: Cell<f64>,
    /// The tabs before the applications, as the Mixer's channels: Master,
    /// Apps, the DJ Booth when DJ is installed, then each mic with a channel.
    pub fixed: RefCell<Vec<(String, String)>>,
    setup: RefCell<patch_core::setup::Watched>,
    /// Every stream's label as last seen: a stream that went (a browser
    /// closes an idle one, a live stream rebuffers) keeps its tab, and the
    /// window follows it when the same application plays again.
    pub seen_labels: RefCell<std::collections::BTreeMap<String, String>>,
    /// The tab last saved for the next EQ window.
    saved_target: RefCell<Option<String>>,
}

impl Default for Panel {
    fn default() -> Self {
        Panel {
            polls: Default::default(),
            changed_at: Default::default(),
            states: RefCell::new(
                patch_core::eq::TARGETS
                    .iter()
                    .map(|(t, _)| (t.to_string(), EqState::default()))
                    .collect(),
            ),
            edited: Cell::new(None),
            last_poll: Cell::new(f64::NEG_INFINITY),
            last_spectrum: Cell::new(0.0),
            spectrum: Cell::new(true),
            live: Cell::new(false),
            presets: RefCell::default(),
            apps: RefCell::default(),
            app_labels: RefCell::default(),
            app_channels: RefCell::default(),
            mixer_labels: RefCell::default(),
            playing: RefCell::default(),
            fixed: RefCell::new(
                patch_core::eq::TARGETS[..2]
                    .iter()
                    .map(|(t, l)| (t.to_string(), l.to_uppercase()))
                    .collect(),
            ),
            setup: RefCell::default(),
            last_presets: Cell::new(f64::NEG_INFINITY),
            seen_labels: RefCell::default(),
            saved_target: RefCell::default(),
        }
    }
}

impl Panel {
    /// A gone stream's tab (`app:<key>`) under its new stream: the one
    /// playing stream with the same label, if exactly one.
    pub fn reopened(&self, target: &str) -> Option<String> {
        let key = target.strip_prefix("app:")?;
        let label = self.seen_labels.borrow().get(key)?.clone();
        let labels = self.app_labels.borrow();
        let mut same = self
            .apps
            .borrow()
            .iter()
            .filter(|a| a.as_str() != key && labels.get(*a) == Some(&label))
            .cloned()
            .collect::<Vec<_>>();
        (same.len() == 1).then(|| format!("app:{}", same.remove(0)))
    }

    /// The tab a new EQ window opens on: the one last chosen.
    pub fn remembered_target(&self) -> Option<String> {
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(ui_path()).ok()?).ok()?;
        let t = v["target"].as_str()?.to_string();
        *self.saved_target.borrow_mut() = Some(t.clone());
        patch_core::eq::valid_target(&t).then_some(t)
    }

    /// Remember the shown tab (only with the service: never from tests or
    /// shots).
    fn save_target(&self, target: &str) {
        if !self.live.get() || self.saved_target.borrow().as_deref() == Some(target) {
            return;
        }
        *self.saved_target.borrow_mut() = Some(target.to_string());
        let _ = std::fs::create_dir_all(patch_engine::session::state_dir());
        let _ = std::fs::write(ui_path(), serde_json::json!({ "target": target }).to_string());
    }

    /// A target's settings.
    pub fn get(&self, target: &str) -> EqState {
        self.states
            .borrow()
            .get(target)
            .cloned()
            .unwrap_or_default()
    }

    pub fn set(&self, target: &str, st: EqState) {
        self.states.borrow_mut().insert(target.to_string(), st);
    }

    /// Poll the service at the next tick (after it changed something).
    pub fn refresh_now(&self) {
        self.last_poll.set(f64::NEG_INFINITY);
        self.last_presets.set(f64::NEG_INFINITY);
        self.edited.set(None);
        self.changed_at.set(Some(Instant::now()));
    }

    /// The preset after (dir +1) or before (−1) the one loaded on `target`.
    pub fn step_preset(&self, target: &str, dir: i32) -> Option<String> {
        let list = cycle_order(&self.presets.borrow());
        if list.is_empty() {
            return None;
        }
        let cur = self.get(target).preset;
        let i = list
            .iter()
            .position(|n| *n == cur)
            .map(|i| i as i32)
            .unwrap_or(if dir > 0 { -1 } else { 0 });
        let n = list.len() as i32;
        Some(list[((i + dir).rem_euclid(n)) as usize].clone())
    }

    /// A name for saving from the window: the loaded user preset (it's
    /// overwritten), else the first free "Custom n".
    pub fn save_name(&self, target: &str) -> String {
        let cur = self.get(target).preset;
        let list = self.presets.borrow();
        if list.iter().any(|(_, n, builtin)| *n == cur && !builtin) {
            return cur;
        }
        (1..)
            .map(|k| format!("Custom {k}"))
            .find(|n| !list.iter().any(|(_, x, _)| x == n))
            .unwrap_or_default()
    }

    pub fn touch(&self) {
        self.edited.set(Some(Instant::now()));
        self.changed_at.set(Some(Instant::now()));
    }

    /// Follow the service (twice a second, not while editing here).
    pub fn poll(&self, t: f64, live: bool) {
        if t - self.last_poll.get() < 0.5 {
            return;
        }
        self.last_poll.set(t);
        self.live.set(live);
        if live && t - self.last_presets.get() >= 2.0 {
            self.last_presets.set(t);
            self.polls
                .request("apps", serde_json::json!({"op": "eq_apps"}));
            self.polls
                .request("presets", serde_json::json!({"op": "eq_presets"}));
        }
        if live {
            if let Some(reply) = self.polls.take("apps")
                && let Ok(v) = reply.result
            {
                let mut apps: Vec<String> = v["assigned"]
                    .as_object()
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default();
                apps.retain(|key| {
                    let shown_in_mixer = v["assigned"][key].as_str().is_some_and(|ch| self.mixer_labels.borrow().contains_key(ch));
                    shown_in_mixer || !v["hidden"].as_array().is_some_and(|hidden| hidden.iter().any(|h| h.as_str() == Some(key.as_str())))
                });
                apps.sort();
                *self.apps.borrow_mut() = apps;
                *self.app_channels.borrow_mut() = serde_json::from_value(v["assigned"].clone()).unwrap_or_default();
                *self.app_labels.borrow_mut() = serde_json::from_value(v["labels"].clone()).unwrap_or_default();
                self.seen_labels.borrow_mut().extend(self.app_labels.borrow().iter().map(|(k, l)| (k.clone(), l.clone())));
                *self.playing.borrow_mut() = v["playing"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect();
            }
            if let Some(reply) = self.polls.take("presets")
                && let Ok(v) = reply.result
            {
                *self.presets.borrow_mut() = v
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| {
                        Some((
                            p["id"].as_str()?.to_string(),
                            p["name"].as_str()?.to_string(),
                            p["builtin"].as_bool().unwrap_or(false),
                        ))
                    })
                    .collect();
            }
        }
        if !live
            || self
                .edited
                .get()
                .is_some_and(|e| e.elapsed().as_millis() < 600)
        {
            return;
        }
        self.polls
            .request("eq", serde_json::json!({"op": "master_eq", "all": true}));
        if let Some(reply) = self.polls.take("eq")
            && self.changed_at.get().is_none_or(|at| at <= reply.requested)
            && let Ok(v) = reply.result
            && let Some(all) = v.as_object()
        {
            for (t, e) in all {
                if let Ok(st) = serde_json::from_value::<EqState>(e.clone()) {
                    let st = st.normalized();
                    if self.get(t) != st {
                        self.set(t, st);
                    }
                }
            }
            let dj = self.setup.borrow_mut().get().dj;
            let mut fixed: Vec<(String, String)> = patch_core::eq::TARGETS
                .iter()
                .filter(|(t, _)| *t != "dj" || dj)
                .map(|(t, l)| (t.to_string(), l.to_uppercase()))
                .collect();
            let mut mics: Vec<&String> = all.keys().filter(|t| patch_core::mic::channel(t)).collect();
            mics.sort_by_key(|m| m.trim_start_matches("mic-").parse::<usize>().unwrap_or(usize::MAX));
            fixed.extend(mics.into_iter().map(|m| {
                (m.clone(), format!("MIC {}", m.trim_start_matches("mic-")))
            }));
            if *self.fixed.borrow() != fixed {
                *self.fixed.borrow_mut() = fixed;
            }
        }
    }

    /// Update the windows: rows, and the image when anything it shows changed.
    pub fn tick(&self, wins: &[crate::ui::PatchWindow], t: f64, spectrum: &[f32]) {
        use slint::{ComponentHandle, Model};
        if wins.is_empty() {
            return;
        }
        // The spectrum moves the picture 15 times a second at most.
        let sp_tick = self.spectrum.get() && t - self.last_spectrum.get() >= 1.0 / 15.0;
        if sp_tick {
            self.last_spectrum.set(t);
        }
        for w in wins {
            let e = w.global::<crate::ui::EqState>();
            let tname = e.get_target().to_string();
            let playing = tname
                .strip_prefix("app:")
                .is_some_and(|a| self.apps.borrow().iter().any(|x| x == a));
            let target: String = if patch_core::mic::channel(&tname) || playing {
                tname.clone()
            } else if tname.starts_with("app:") && patch_core::eq::valid_target(&tname) {
                // Gone for now: stay on it (it used to jump to Master), and
                // follow it when its application plays again.
                self.reopened(&tname).unwrap_or_else(|| tname.clone())
            } else {
                patch_core::eq::target(&tname).unwrap_or("master").to_string()
            };
            if target != tname {
                e.set_target(target.as_str().into());
            }
            self.save_target(&target);
            let target = target.as_str();
            let st = self.get(target);
            let sel = (e.get_selected().max(0) as usize).min(BANDS - 1);
            let th = w.global::<crate::ui::Theme>();
            let rgb = |c: slint::Color| [c.red(), c.green(), c.blue()];
            let colors = Colors {
                band: rgb(th.get_text_dim()),
                curve: rgb(th.get_text_bright()),
                accent: rgb(th.get_accent()),
                spectrum: rgb(th.get_text_faint()),
            };
            let scale = w.window().scale_factor();
            let (pw, ph) = (
                (e.get_plot_w() * scale).round().max(2.0) as usize,
                (e.get_plot_h() * scale).round().max(2.0) as usize,
            );
            // What the image shows, hashed: redraw only when it changed.
            let key = {
                use std::hash::{Hash, Hasher};
                let mut hsh = std::collections::hash_map::DefaultHasher::new();
                serde_json::to_string(&st)
                    .unwrap_or_default()
                    .hash(&mut hsh);
                target.hash(&mut hsh);
                (
                    sel,
                    pw.min(4096),
                    ph.min(2048),
                    th.get_accent().as_argb_encoded(),
                )
                    .hash(&mut hsh);
                hsh.finish()
            };
            // Per window (its own state): a reopened window draws again.
            let key = (key as i64 & 0x7fff_ffff) as i32;
            if e.get_drawn_key() != key {
                e.set_drawn_key(key);
                let img = render(pw.min(4096), ph.min(2048), &st, sel, scale, &colors);
                e.set_plot(slint::Image::from_rgba8_premultiplied(img));
                self.rows(&e, &st);
            }
            let show_spectrum = self.spectrum.get() && !patch_core::mic::channel(target);
            if sp_tick || (!show_spectrum && e.get_spectrum()) {
                let img = if show_spectrum {
                    render_spectrum(
                        (pw / 2).min(2048),
                        (ph / 2).min(1024),
                        spectrum,
                        colors.spectrum,
                    )
                } else {
                    slint::SharedPixelBuffer::new(1, 1)
                };
                e.set_spectrum_img(slint::Image::from_rgba8(img));
            }
            e.set_bypass(st.bypass);
            e.set_gain(st.gain_db);
            let preset = if st.preset.is_empty() {
                String::new()
            } else if st.edited {
                format!("{}*", st.preset)
            } else {
                st.preset.clone()
            };
            if e.get_preset().as_str() != preset {
                e.set_preset(preset.into());
            }
            e.set_live(self.live.get());
            e.set_spectrum(show_spectrum);
            let on = st.bands.iter().filter(|b| b.active()).count();
            let what = if target == "master" {
                "the program · after the mix, before Tune"
            } else if target.starts_with("app:stream:") {
                "this playback stream only · its own channel"
            } else if target.starts_with("app:") {
                "this application only · its own channel"
            } else {
                "this channel · before its fader"
            };
            e.set_status(
                if st.bypass {
                    "bypassed · passes untouched".to_string()
                } else if on == 0 && st.gain_db.abs() < 0.01 {
                    "flat · nothing processed".to_string()
                } else {
                    format!(
                        "{on} band{} shaping {what} · 0 latency",
                        if on == 1 { "" } else { "s" }
                    )
                }
                .into(),
            );
            // The tabs: fixed targets, then applications with their own EQ.
            let mut tabs: Vec<crate::ui::EqTarget> = self
                .fixed
                .borrow()
                .iter()
                .map(|(t, l)| crate::ui::EqTarget {
                    id: t.into(),
                    label: l.into(),
                    busy: self.get(t).active(),
                    app: false,
                })
                .collect();
            if patch_core::mic::channel(target) && !tabs.iter().any(|t| t.id == target) {
                tabs.push(crate::ui::EqTarget { id: target.into(), label: target.to_uppercase().into(), busy: self.get(target).active(), app: false });
            }
            for a in self.apps.borrow().iter() {
                let id = format!("app:{a}");
                tabs.push(crate::ui::EqTarget {
                    busy: self.get(&id).active(),
                    label: self.app_channels.borrow().get(a).and_then(|ch| self.mixer_labels.borrow().get(ch).cloned())
                        .unwrap_or_else(|| self.app_labels.borrow().get(a).unwrap_or(a).clone()).to_uppercase().into(),
                    id: id.into(),
                    app: !patch_core::stream::is_key(a),
                });
            }
            // A gone stream's tab stays while the window shows it.
            if target.starts_with("app:") && !tabs.iter().any(|t| t.id == target) {
                let key = target.trim_start_matches("app:");
                tabs.push(crate::ui::EqTarget {
                    busy: false,
                    label: self.seen_labels.borrow().get(key).map_or(key, |l| l.as_str()).to_uppercase().into(),
                    id: target.into(),
                    app: !patch_core::stream::is_key(key),
                });
            }
            let same = e.get_targets().row_count() == tabs.len()
                && tabs
                    .iter()
                    .enumerate()
                    .all(|(i, t)| e.get_targets().row_data(i).as_ref() == Some(t));
            if !same {
                e.set_targets(std::rc::Rc::new(slint::VecModel::from(tabs)).into());
            }
            let cands: Vec<slint::SharedString> = self
                .playing
                .borrow()
                .iter()
                .filter(|p| !self.apps.borrow().contains(p))
                .map(|p| p.as_str().into())
                .collect();
            if e.get_candidates().row_count() != cands.len()
                || cands
                    .iter()
                    .enumerate()
                    .any(|(i, c)| e.get_candidates().row_data(i).as_ref() != Some(c))
            {
                e.set_candidates(std::rc::Rc::new(slint::VecModel::from(cands)).into());
            }
        }
    }

    fn rows(&self, e: &crate::ui::EqState, st: &EqState) {
        use slint::Model;
        let rows: Vec<crate::ui::EqRow> = st
            .bands
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let (x, y) = node(b);
                crate::ui::EqRow {
                    n: format!("{}", i + 1).into(),
                    kind: b.kind.short().into(),
                    freq: hz_text(b.freq).into(),
                    gain: if b.kind.has_gain() {
                        db_text(b.gain_db).into()
                    } else {
                        "—".into()
                    },
                    q: format!("{:.2}", b.q).into(),
                    on: b.on,
                    x,
                    y,
                }
            })
            .collect();
        // In place: a new model would recreate the plot's touch targets and
        // drop the node being dragged (pressing one selects it, a redraw).
        let model = e.get_bands();
        if model.row_count() == rows.len() {
            for (i, r) in rows.into_iter().enumerate() {
                if model.row_data(i).as_ref() != Some(&r) {
                    model.set_row_data(i, r);
                }
            }
        } else {
            e.set_bands(std::rc::Rc::new(slint::VecModel::from(rows)).into());
        }
        if e.get_freq_marks().row_count() > 0 {
            return;
        }
        let marks = |v: Vec<(&str, f32)>| -> slint::ModelRc<crate::ui::EqMark> {
            std::rc::Rc::new(slint::VecModel::from(
                v.into_iter()
                    .map(|(t, p)| crate::ui::EqMark {
                        text: t.into(),
                        pos: p,
                    })
                    .collect::<Vec<_>>(),
            ))
            .into()
        };
        e.set_freq_marks(marks(vec![
            ("50", x_of(50.0)),
            ("100", x_of(100.0)),
            ("200", x_of(200.0)),
            ("500", x_of(500.0)),
            ("1k", x_of(1000.0)),
            ("2k", x_of(2000.0)),
            ("5k", x_of(5000.0)),
            ("10k", x_of(10_000.0)),
        ]));
        e.set_db_marks(marks(vec![
            ("+12", y_of(12.0)),
            ("+6", y_of(6.0)),
            ("0", y_of(0.0)),
            ("−6", y_of(-6.0)),
            ("−12", y_of(-12.0)),
        ]));
    }
}

/// A band after a drag to plot position (x, y).
pub fn dragged(b: &EqBand, x: f32, y: f32) -> EqBand {
    let mut n = *b;
    n.freq = hz_of(x).clamp(FREQ_RANGE.0, FREQ_RANGE.1);
    if b.kind.has_gain() {
        // Snap near 0 dB, so flat is easy to find again.
        let g = db_of(y).clamp(-GAIN_RANGE, GAIN_RANGE);
        n.gain_db = if g.abs() < 0.3 {
            0.0
        } else {
            (g * 10.0).round() / 10.0
        };
    }
    n.on = true;
    n
}

/// A band after a key: ←→ frequency (a semitone, ⇧ a third of an octave),
/// ↑↓ gain (0.5 dB, ⇧ 3 dB), [ ] Q, 0 back to flat. Moving a band that is
/// off turns it on, as a drag does: a tester toggled one off with Space and
/// its keys then seemed dead (nothing heard, no curve).
pub fn keyed(b: &EqBand, key: &str, shift: bool) -> Option<EqBand> {
    let mut n = *b;
    if key != "0" {
        n.on = true;
    }
    let semis = if shift { 4.0 } else { 1.0 };
    match key {
        "\u{f702}" => n.freq /= 2f32.powf(semis / 12.0),
        "\u{f703}" => n.freq *= 2f32.powf(semis / 12.0),
        "\u{f700}" if b.kind.has_gain() => n.gain_db += if shift { 3.0 } else { 0.5 },
        "\u{f701}" if b.kind.has_gain() => n.gain_db -= if shift { 3.0 } else { 0.5 },
        "[" => n.q /= 1.25,
        "]" => n.q *= 1.25,
        "0" => n.gain_db = 0.0,
        _ => return None,
    }
    n.freq = n.freq.clamp(FREQ_RANGE.0, FREQ_RANGE.1);
    n.gain_db = n.gain_db.clamp(-GAIN_RANGE, GAIN_RANGE);
    n.q = n.q.clamp(Q_RANGE.0, Q_RANGE.1);
    Some(n)
}

/// A typed value for a band field ("freq", "gain", "q"): "400", "2.5k",
/// "-3", "0,7". Out of range clamps; text that isn't a number is None (the
/// window flashes). The band comes on, as when it's moved.
pub fn typed(b: &EqBand, field: &str, text: &str) -> Option<EqBand> {
    let t = text.trim().replace(',', ".");
    let (num, scale) = match t.strip_suffix(['k', 'K']) {
        Some(n) if field == "freq" => (n, 1000.0),
        Some(_) => return None,
        None => (t.as_str(), 1.0),
    };
    let v = num.parse::<f32>().ok().filter(|v| v.is_finite())? * scale;
    let mut n = *b;
    match field {
        "freq" => n.freq = v.clamp(FREQ_RANGE.0, FREQ_RANGE.1),
        "gain" if b.kind.has_gain() => n.gain_db = v.clamp(-GAIN_RANGE, GAIN_RANGE),
        "q" if v > 0.0 => n.q = v.clamp(Q_RANGE.0, Q_RANGE.1),
        _ => return None,
    }
    n.on = true;
    Some(n)
}

/// What the window shows while a value is typed.
pub fn entry_prompt(field: &str, text: &str) -> String {
    let (what, unit) = match field {
        "freq" => ("frequency", " Hz (2.5k = 2.5 kHz)"),
        "gain" => ("gain", " dB"),
        _ => ("Q", ""),
    };
    format!("{what}: {text}▏{unit} · enter sets · esc cancels")
}

/// A plausible mastering-style curve for screenshots (labelled simulated).
pub fn demo() -> EqState {
    let mut st = EqState::default();
    st.bands[0].on = true;
    st.bands[1].gain_db = 2.0;
    st.bands[2] = EqBand {
        gain_db: -3.5,
        q: 1.6,
        ..st.bands[2]
    };
    st.bands[4] = EqBand {
        gain_db: 2.5,
        q: 0.9,
        ..st.bands[4]
    };
    st.bands[6].gain_db = 1.5;
    st
}

/// The command that sets band `i` of `target` to `b`.
pub fn command(target: &str, i: usize, b: &EqBand) -> patch_core::command::Command {
    patch_core::command::Command::EqBand {
        target: Some(target.to_string()),
        band: i as u8,
        on: Some(b.on),
        kind: Some(b.kind),
        freq: Some(b.freq),
        gain_db: Some(b.gain_db),
        q: Some(b.q),
    }
}

pub fn next_kind(k: EqKind) -> EqKind {
    k.next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plot_mapping_round_trips() {
        for hz in [20.0f32, 100.0, 1000.0, 12_345.0, 20_000.0] {
            assert!((hz_of(x_of(hz)) - hz).abs() / hz < 1e-4);
        }
        for db in [-18.0f32, -3.5, 0.0, 12.0] {
            assert!((db_of(y_of(db)) - db).abs() < 1e-4);
        }
    }

    #[test]
    fn dragging_snaps_to_flat_and_keys_clamp() {
        let b = EqBand::default();
        let d = dragged(&b, x_of(440.0), y_of(0.2));
        assert!((d.freq - 440.0).abs() < 0.5 && d.gain_db == 0.0);
        let mut k = EqBand { gain_db: 17.8, ..b };
        k = keyed(&k, "\u{f700}", true).unwrap();
        assert_eq!(k.gain_db, 18.0);
        // An off band comes back on when moved; flattening leaves it as it is.
        let off = EqBand { on: false, ..b };
        assert!(keyed(&off, "\u{f703}", false).unwrap().on);
        assert!(keyed(&off, "]", false).unwrap().on);
        assert!(!keyed(&off, "0", false).unwrap().on);
    }

    #[test]
    fn typed_values_set_the_band_and_bad_text_is_refused() {
        let b = EqBand { on: false, ..EqBand::default() };
        assert_eq!(typed(&b, "freq", "400").unwrap().freq, 400.0);
        assert_eq!(typed(&b, "freq", "2.5k").unwrap().freq, 2500.0);
        assert_eq!(typed(&b, "freq", "99k").unwrap().freq, FREQ_RANGE.1);
        assert!(typed(&b, "freq", "400").unwrap().on, "a typed band comes on");
        assert_eq!(typed(&b, "gain", "-3,5").unwrap().gain_db, -3.5);
        assert_eq!(typed(&b, "q", "0.7").unwrap().q, 0.7);
        assert!(typed(&b, "freq", "").is_none());
        assert!(typed(&b, "freq", "4-0").is_none());
        assert!(typed(&b, "gain", "3k").is_none());
        assert!(typed(&b, "q", "0").is_none());
        let pass = EqBand { kind: EqKind::HighPass, ..b };
        assert!(typed(&pass, "gain", "3").is_none(), "a pass filter has no gain");
    }

    #[test]
    fn a_reopened_stream_is_followed_by_its_label() {
        let p = Panel::default();
        p.seen_labels.borrow_mut().insert("stream:1".into(), "Chromium".into());
        // Gone, nothing back yet: stay on it.
        assert_eq!(p.reopened("app:stream:1"), None);
        *p.apps.borrow_mut() = vec!["stream:2".into()];
        p.app_labels.borrow_mut().insert("stream:2".into(), "Chromium".into());
        assert_eq!(p.reopened("app:stream:1").as_deref(), Some("app:stream:2"));
        // Two candidates: no guess.
        p.apps.borrow_mut().push("stream:3".into());
        p.app_labels.borrow_mut().insert("stream:3".into(), "Chromium".into());
        assert_eq!(p.reopened("app:stream:1"), None);
    }

    #[test]
    fn the_response_renders_a_boost_above_the_zero_line() {
        let mut st = EqState::default();
        st.bands[3] = EqBand {
            on: true,
            kind: EqKind::Bell,
            freq: 1000.0,
            gain_db: 12.0,
            q: 1.0,
        };
        let c = Colors {
            band: [150; 3],
            curve: [240; 3],
            accent: [230, 140, 20],
            spectrum: [90; 3],
        };
        let (w, h) = (400, 200);
        let img = render(w, h, &st, 3, 1.0, &c);
        let px = img.as_slice();
        // The curve at 1 kHz sits at +12 dB.
        let x = (x_of(1000.0) * w as f32) as usize;
        let y = (y_of(12.0) * h as f32) as usize;
        let near = (y.saturating_sub(2)..y + 3)
            .map(|yy| px[yy * w + x].a)
            .max()
            .unwrap();
        assert!(near > 200, "curve at +12 dB over 1 kHz");
    }

    /// `cargo test --release -p patch-surfaces eq_render_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn eq_render_cost() {
        let mut st = EqState::default();
        for (i, b) in st.bands.iter_mut().enumerate() {
            b.on = true;
            b.gain_db = if i % 2 == 0 { 4.0 } else { -4.0 };
        }
        let c = Colors {
            band: [150; 3],
            curve: [240; 3],
            accent: [230, 140, 20],
            spectrum: [90; 3],
        };
        let sp: Vec<f32> = (0..64).map(|i| -30.0 - i as f32 * 0.5).collect();
        let t0 = std::time::Instant::now();
        for _ in 0..20 {
            let _ = render(1200, 480, &st, 2, 1.6, &c);
        }
        println!(
            "EQ response 1200×480, 8 bands: {:.2} ms per redraw (only when a band changes)",
            t0.elapsed().as_secs_f64() * 1000.0 / 20.0
        );
        let t0 = std::time::Instant::now();
        for _ in 0..200 {
            let _ = render_spectrum(600, 240, &sp, [90; 3]);
        }
        println!(
            "EQ spectrum layer 600×240: {:.3} ms per frame (15 fps while shown)",
            t0.elapsed().as_secs_f64() * 1000.0 / 200.0
        );
    }

    #[test]
    fn cached_trig_matches_the_exact_response() {
        let b = EqBand {
            on: true,
            kind: EqKind::HighShelf,
            freq: 3000.0,
            gain_db: 7.5,
            q: 0.71,
        };
        let c = coefficients(&b, 48_000.0);
        let trig = column_trig(300, 48_000.0);
        for (x, t) in trig.iter().enumerate().step_by(37) {
            let hz = hz_of((x as f32 + 0.5) / 300.0) as f64;
            assert!((response_at(&c, t) as f64 - response_db(&c, hz, 48_000.0)).abs() < 1e-3);
        }
    }
}

/// The order `p` steps through presets: the first built-in (Flat), then your
/// own and AutoEq presets, then the other built-ins. Built-ins first meant
/// passing every one of them, each changing the bands as it loaded, before
/// reaching a preset of yours.
fn cycle_order(list: &[(String, String, bool)]) -> Vec<String> {
    let first = list.iter().take(1).filter(|(_, _, builtin)| *builtin);
    let mine = list.iter().filter(|(_, _, builtin)| !builtin);
    let rest = list.iter().filter(|(_, _, builtin)| *builtin).skip(first.clone().count());
    first.chain(mine).chain(rest).map(|(_, n, _)| n.clone()).collect()
}

#[test]
fn your_presets_come_right_after_flat() {
    let p = |n: &str, b| (n.to_lowercase(), n.to_string(), b);
    let list = [p("Flat", true), p("Bass", true), p("Vocal", true), p("AirPods Max", false), p("Mine", false)];
    assert_eq!(cycle_order(&list), ["Flat", "AirPods Max", "Mine", "Bass", "Vocal"]);
    assert_eq!(cycle_order(&[p("Mine", false)]), ["Mine"]);
}
