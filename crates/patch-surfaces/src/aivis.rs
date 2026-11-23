//! The AI DJ, drawn: an abstract musical-intelligence field rendered from
//! the authoritative `patch_core::aivis::VisualState` (ADR-007).
//!
//! Every mark maps to real state; nothing here decides anything:
//!
//! * PULSE FIELD — a polar lattice around the core. A pulse travels out on
//!   every heard beat (stronger on downbeats); the lattice twists a step at
//!   every phrase boundary and slowly within the phrase; its density follows
//!   the energy; rising energy ahead compresses it (tension), a drop
//!   releases it; a weak beat grid adds jitter (timing uncertainty).
//! * CORE — concentric rings. Their coherence is the AI's evidence-support
//!   index (misaligned when evidence is limited), their stretch points at the chosen
//!   candidate; hollow in Observe; fragmented after a human override,
//!   healing as it re-evaluates; a slow scan while it evaluates.
//! * CONSTELLATION — candidates in possibility space: angle = Camelot steps
//!   from what plays (harmony), distance = tempo fit, size = score, ring
//!   coherence = uncalibrated evidence support, three factor ticks (tempo, harmony, energy).
//!   Strong candidates pull the field toward them (gravity).
//! * TRAJECTORY — the TransitionPlan: shape by style (blend overlaps, cut
//!   kinks, filter tapers, loops repeat, effects echo), progress by the
//!   engine clock; before it starts, the bars left count down on the beat.
//! * MODE — threads: none in Observe, dotted in Suggest, solid to the
//!   chosen one in Prepare, and marching in Act.
//! * ENVIRONMENT — an outer field, only when real sensors report; absent
//!   otherwise (never implied).
//! * HISTORY — past decisions as short trails that decay.
//!
//! Timing is interpolated from the engine's stamped cycle (`clock`) and
//! delayed by the output latency, so pulses land when they are heard.

use patch_core::aivis::{Mode, Phase, VisualState};
use std::f32::consts::{PI, TAU};

/// Theme colours (from the Omarchy/Patch theme tokens).
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub bg: [u8; 3],
    pub fg: [u8; 3],
    pub dim: [u8; 3],
    pub faint: [u8; 3],
    pub accent: [u8; 3],
    pub warn: [u8; 3],
}

impl Palette {
    #[cfg(test)]
    pub fn mono() -> Palette {
        Palette {
            bg: [9, 9, 9],
            fg: [234, 234, 234],
            dim: [138, 138, 141],
            faint: [90, 90, 90],
            accent: [230, 142, 13],
            warn: [201, 162, 58],
        }
    }
}

/// Which view the frame is for (they share the state, not the layout).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Compact,
    Operator,
    Analysis,
}

/// Rendering detail (the quality strategy: visuals degrade, audio never).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Quality {
    Minimal,
    Reduced,
    Full,
}

/// The musical moment a frame shows (from the engine clock).
#[derive(Clone, Copy, Debug)]
pub struct Moment {
    /// Beats of the leading source, as heard now (None: no grid).
    pub beat: Option<f64>,
    /// Seconds for non-musical motion (information flow, scans): smooth
    /// but never musical truth.
    pub t: f64,
    pub reduced_motion: bool,
}

impl Moment {
    /// The heard beat of the lead source at `now_ns` (CLOCK_MONOTONIC).
    pub fn at(
        state: &VisualState,
        now_ns: u64,
        output_delay_ns: u64,
        t: f64,
        reduced_motion: bool,
    ) -> Moment {
        let beat = state.lead().and_then(|s| {
            let (b, bpm) = (s.beat?, s.bpm?);
            if state.clock.nsec == 0 {
                return Some(b);
            }
            let heard = now_ns.saturating_sub(output_delay_ns) as i128 - state.clock.nsec as i128;
            // A stale stamp (paused engine, sleep) is not extrapolated far.
            let dt = (heard as f64 / 1e9).clamp(-2.0, 2.0);
            Some(b + dt * bpm / 60.0)
        });
        Moment {
            beat,
            t,
            reduced_motion,
        }
    }
}

/// What a frame drew (for tests and diagnostics).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    pub nodes: usize,
    pub lattice_points: usize,
    pub threads: usize,
    pub trajectory: bool,
    pub environment: bool,
}

/// An RGBA canvas with additive, alpha-weighted plotting.
pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
    bg: [u8; 3],
}

impl Canvas {
    pub fn new(w: usize, h: usize, bg: [u8; 3]) -> Canvas {
        let mut px = vec![255u8; w * h * 4];
        for p in px.as_chunks_mut::<4>().0 {
            p[..3].copy_from_slice(&bg);
        }
        Canvas { w, h, px, bg }
    }

    #[inline]
    fn plot(&mut self, x: i32, y: i32, c: [u8; 3], a: f32) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h || a <= 0.0 {
            return;
        }
        let i = (y as usize * self.w + x as usize) * 4;
        let a = a.min(1.0);
        for k in 0..3 {
            let cur = self.px[i + k] as f32;
            self.px[i + k] = (cur + (c[k] as f32 - cur) * a) as u8;
        }
    }

    /// A soft dot of radius `r` (anti-aliased edge).
    pub fn dot(&mut self, x: f32, y: f32, r: f32, c: [u8; 3], a: f32) {
        let r = r.max(0.5);
        let (x0, x1, y0, y1) = (
            (x - r - 1.0) as i32,
            (x + r + 1.0) as i32,
            (y - r - 1.0) as i32,
            (y + r + 1.0) as i32,
        );
        for yy in y0..=y1 {
            for xx in x0..=x1 {
                let d = ((xx as f32 + 0.5 - x).powi(2) + (yy as f32 + 0.5 - y).powi(2)).sqrt();
                let cov = (r + 0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.plot(xx, yy, c, a * cov);
                }
            }
        }
    }

    /// An anti-aliased line (Wu), `width` ≥ 1 by parallel strokes.
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, c: [u8; 3], a: f32, width: f32) {
        let n = width.max(1.0).round() as i32;
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len = (dx * dx + dy * dy).sqrt().max(1e-3);
        let (nx, ny) = (-dy / len, dx / len);
        for k in 0..n {
            let o = k as f32 - (n - 1) as f32 / 2.0;
            self.wu(x0 + nx * o, y0 + ny * o, x1 + nx * o, y1 + ny * o, c, a);
        }
    }

    fn wu(&mut self, mut x0: f32, mut y0: f32, mut x1: f32, mut y1: f32, c: [u8; 3], a: f32) {
        let steep = (y1 - y0).abs() > (x1 - x0).abs();
        if steep {
            std::mem::swap(&mut x0, &mut y0);
            std::mem::swap(&mut x1, &mut y1);
        }
        if x0 > x1 {
            std::mem::swap(&mut x0, &mut x1);
            std::mem::swap(&mut y0, &mut y1);
        }
        let grad = if (x1 - x0).abs() < 1e-6 {
            0.0
        } else {
            (y1 - y0) / (x1 - x0)
        };
        let mut y = y0;
        let (xs, xe) = (x0.round() as i32, x1.round() as i32);
        for x in xs..=xe {
            let fy = y.floor();
            let frac = y - fy;
            if steep {
                self.plot(fy as i32, x, c, a * (1.0 - frac));
                self.plot(fy as i32 + 1, x, c, a * frac);
            } else {
                self.plot(x, fy as i32, c, a * (1.0 - frac));
                self.plot(x, fy as i32 + 1, c, a * frac);
            }
            y += grad;
        }
    }

    /// A dotted line: `on` of every `period` pixels drawn.
    pub fn dotted(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        c: [u8; 3],
        a: f32,
        period: f32,
        phase: f32,
    ) {
        let len = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
        let n = (len / period).max(1.0) as usize;
        for i in 0..n {
            let t = ((i as f32 + phase.rem_euclid(1.0)) * period / len).min(1.0);
            self.dot(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t, 0.9, c, a);
        }
    }

    pub fn into_rgba(self) -> Vec<u8> {
        let _ = self.bg;
        self.px
    }
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t) as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t) as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t) as u8,
    ]
}

/// Deterministic noise in −1…1 (for jitter and uncertainty, never decor).
fn hash(a: u32, b: u32) -> f32 {
    let mut h = a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 13;
    (h as f32 / u32::MAX as f32) * 2.0 - 1.0
}

/// Where a candidate sits: (angle, radius factor). Harmony sets the angle
/// (Camelot steps clockwise from straight up), tempo fit the distance.
fn candidate_place(c: &patch_core::aivis::Candidate, i: usize) -> (f32, f32) {
    // Rank fans candidates out a little so equal keys don't overlap.
    let spread = hash(i as u32, 7) * 0.1;
    let angle = -PI / 2.0 + c.key_steps as f32 * 0.5 + c.energy_delta * 1.6 + spread;
    let dist = 0.52
        + 0.4 * (1.0 - c.factors.tempo.clamp(0.0, 1.0))
        + 0.08 * (1.0 - c.score.clamp(0.0, 1.0));
    (angle, dist)
}

/// Render one frame of the field (the whole canvas: views add text).
pub fn render(
    w: usize,
    h: usize,
    s: &VisualState,
    m: Moment,
    pal: &Palette,
    view: View,
    q: Quality,
) -> (Vec<u8>, Stats) {
    let mut cv = Canvas::new(w.max(1), h.max(1), pal.bg);
    let mut st = Stats::default();
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let rad = (w.min(h) as f32) * if view == View::Compact { 0.46 } else { 0.44 };
    let lead = s.lead();
    let alive = !matches!(s.phase, Phase::Idle | Phase::Unavailable);

    // ── musical timing (heard) ─────────────────────────────────────────
    let beat = m.beat.filter(|_| lead.is_some());
    let (beat_frac, is_down, phrase_idx, phrase_pos) = match beat {
        Some(b) => {
            let f = b.rem_euclid(1.0) as f32;
            (
                f,
                b.rem_euclid(4.0) < 1.0,
                (b / 32.0).floor() as i32,
                (b / 32.0).rem_euclid(1.0) as f32,
            )
        }
        None => (0.5, false, 0, 0.0),
    };
    let pulse = if beat.is_some() && !m.reduced_motion {
        (-beat_frac * 6.0).exp() * if is_down { 1.0 } else { 0.55 }
    } else {
        0.0
    };
    let energy = lead.map(|l| l.energy).unwrap_or(0.0);
    let ahead = lead
        .map(|l| l.energy_ahead.iter().copied().sum::<f32>() / l.energy_ahead.len().max(1) as f32)
        .unwrap_or(energy);
    let tension = (ahead - energy).clamp(-0.5, 0.5);
    let ahead_curve: Vec<f32> = lead.map(|l| l.energy_ahead.clone()).unwrap_or_default();
    let timing_conf = lead.map(|l| l.beat_confidence).unwrap_or(0.0);
    let confidence = s.confidence.clamp(0.0, 1.0);

    // Candidates' places (visible, not rejected). Angles keep their order
    // (harmony, energy) but are pushed apart to stay legible.
    let mut placed: Vec<(usize, &patch_core::aivis::Candidate, f32, f32)> = s
        .candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| !c.rejected)
        .map(|(i, c)| {
            let (a, d) = candidate_place(c, i);
            (i, c, a, d)
        })
        .collect();
    placed.sort_by(|a, b| a.2.total_cmp(&b.2));
    const MIN_SEP: f32 = 0.62;
    for _ in 0..6 {
        for k in 1..placed.len() {
            let gap = placed[k].2 - placed[k - 1].2;
            if gap < MIN_SEP {
                let push = (MIN_SEP - gap) / 2.0;
                placed[k - 1].2 -= push;
                placed[k].2 += push;
            }
        }
    }
    let cands: Vec<(usize, &patch_core::aivis::Candidate, f32, f32)> = placed
        .into_iter()
        .map(|(i, c, a, d)| (i, c, cx + a.cos() * rad * d, cy + a.sin() * rad * d))
        .collect();
    let chosen = cands.iter().find(|(_, c, ..)| c.selected);

    let frame = Frame {
        ahead_curve: &ahead_curve,
        alive,
        beat,
        beat_frac,
        cands: &cands,
        chosen,
        confidence,
        cx,
        cy,
        energy,
        is_down,
        lead,
        m,
        pal,
        phrase_idx,
        phrase_pos,
        pulse,
        q,
        rad,
        s,
        tension,
        timing_conf,
        view,
    };
    draw_field(&mut cv, &mut st, &frame);
    draw_history(&mut cv, &mut st, &frame);
    draw_constellation(&mut cv, &mut st, &frame);
    draw_trajectory(&mut cv, &mut st, &frame);
    draw_core(&mut cv, &mut st, &frame);
    (cv.into_rgba(), st)
}

/// The environment (only with real sensors) and the pulse field.
fn draw_field(cv: &mut Canvas, st: &mut Stats, f: &Frame<'_>) {
    let Frame {
        s,
        m,
        pal,
        view,
        q,
        cx,
        cy,
        rad,
        lead,
        alive,
        beat,
        beat_frac,
        is_down,
        phrase_idx,
        phrase_pos,
        energy,
        tension,
        ahead_curve,
        timing_conf,
        cands,
        ..
    } = *f;
    // ── environment (only with real sensors) ───────────────────────────
    if let Some(env) = s.environment.as_ref().filter(|e| !e.sensors.is_empty()) {
        st.environment = true;
        let n = match q {
            Quality::Full => 240,
            Quality::Reduced => 140,
            Quality::Minimal => 60,
        };
        for k in 0..n {
            let a = k as f32 / n as f32 * TAU;
            let noise = hash(k as u32, (m.t * 3.0) as u32) * (1.0 - env.reliability) * 0.04;
            let r = rad * (1.02 + 0.03 * env.energy * (a * 5.0 + m.t as f32).sin() + noise);
            cv.dot(
                cx + a.cos() * r,
                cy + a.sin() * r,
                0.8 + env.energy,
                pal.dim,
                0.25 + 0.5 * env.energy * env.reliability,
            );
        }
    }

    // ── pulse field ────────────────────────────────────────────────────
    let (rings, spokes) = match (view, q) {
        (View::Compact, _) => (5, 36),
        (_, Quality::Full) => (11, 96),
        (_, Quality::Reduced) => (8, 64),
        (_, Quality::Minimal) => (5, 40),
    };
    if alive || lead.is_some() {
        // Twist: a step at each phrase boundary, a slow turn within it.
        let twist =
            phrase_idx as f32 * (PI / spokes as f32) * 3.0 + phrase_pos * (PI / spokes as f32);
        let visible_rings = ((rings as f32) * (0.35 + 0.65 * energy)).ceil() as usize;
        let thin = energy < 0.25; // breakdown: every other spoke
        for i in 0..visible_rings.min(rings) {
            let ri = i as f32 / rings as f32;
            // Tension compresses toward the core, release expands.
            let base = rad * (0.30 + 0.66 * ri) * (1.0 - tension * 0.25 * ri);
            // The pulse travels outward: later on outer rings.
            let local = if beat.is_some() && !m.reduced_motion {
                (-(beat_frac - ri * 0.35).max(0.0) * 7.0).exp()
                    * if (beat_frac - ri * 0.35) < 0.0 {
                        0.0
                    } else {
                        1.0
                    }
            } else {
                0.0
            };
            let amp = if is_down { 1.0 } else { 0.5 };
            for k in 0..spokes {
                if thin && k % 2 == 1 {
                    continue;
                }
                // Spiral arms: outer rings trail, so the lattice reads as
                // a flowing fabric, not a target.
                let a0 = k as f32 / spokes as f32 * TAU + twist * (1.0 + ri) + ri * ri * 1.4;
                // The energy ahead, wrapped clockwise from the top, bulges
                // the field where the music is heading.
                let contour = {
                    let n = ahead_curve.len();
                    if n == 0 {
                        0.0
                    } else {
                        let u = ((a0 + PI / 2.0).rem_euclid(TAU) / TAU) * n as f32;
                        let (i0, f) = (u.floor() as usize % n, u.fract());
                        ahead_curve[i0] * (1.0 - f) + ahead_curve[(i0 + 1) % n] * f - energy
                    }
                };
                let jitter = hash(
                    i as u32 * 131 + k as u32,
                    beat.map(|b| (b * 4.0) as u32).unwrap_or(0),
                ) * (1.0 - timing_conf)
                    * 0.035
                    * rad;
                let mut r = base * (1.0 + contour * 2.4 * ri) + local * amp * rad * 0.035 + jitter;
                let (mut x, mut y) = (cx + a0.cos() * r, cy + a0.sin() * r);
                // Gravity: strong candidates pull the field.
                for (_, c, nx, ny) in cands {
                    let (dx, dy) = (nx - x, ny - y);
                    let d2 = dx * dx + dy * dy + rad * rad * 0.02;
                    let g = c.score
                        * c.score
                        * (if c.selected { 2.2 } else { 1.0 })
                        * rad
                        * rad
                        * 0.012
                        / d2;
                    x += dx * g.min(0.5);
                    y += dy * g.min(0.5);
                }
                r = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
                let _ = r;
                let bright = 0.26 + 0.3 * local + 0.3 * energy + contour.max(0.0);
                let col = mix(pal.faint, pal.fg, local * 0.6);
                if q == Quality::Minimal || view == View::Compact {
                    cv.dot(x, y, 0.8 + local * 0.6, col, bright);
                } else {
                    // A filing in the field: points outward, leans with the
                    // twist, grows with the pulse and the energy ahead.
                    let dir = (y - cy).atan2(x - cx) + 0.5 + tension * 0.8;
                    let l =
                        rad * (0.008 + 0.014 * local + 0.02 * contour.max(0.0) + 0.006 * energy);
                    cv.line(
                        x - dir.cos() * l,
                        y - dir.sin() * l,
                        x + dir.cos() * l,
                        y + dir.sin() * l,
                        col,
                        bright,
                        1.0,
                    );
                }
                st.lattice_points += 1;
            }
        }
    }

}

/// History trails, decaying over ten minutes.
fn draw_history(cv: &mut Canvas, _st: &mut Stats, f: &Frame<'_>) {
    let Frame {
        s,
        pal,
        view,
        cx,
        cy,
        rad,
        ..
    } = *f;
    // ── history trails (decay over ten minutes) ────────────────────────
    if view != View::Compact {
        for (i, d) in s.history.iter().enumerate().take(5) {
            let age = (d.age_s / 600.0).clamp(0.0, 1.0) as f32;
            if age >= 1.0 {
                continue;
            }
            let a = PI / 2.0 + (i as f32 - 2.0) * 0.18;
            let (x0, y0) = (cx + a.cos() * rad * 0.2, cy + a.sin() * rad * 0.2);
            let (x1, y1) = (
                cx + a.cos() * rad * (0.45 + 0.1 * i as f32),
                cy + a.sin() * rad * (0.45 + 0.1 * i as f32),
            );
            cv.line(x0, y0, x1, y1, pal.dim, 0.35 * (1.0 - age), 1.0);
        }
    }

}

/// The constellation: candidates' threads and nodes.
fn draw_constellation(cv: &mut Canvas, st: &mut Stats, f: &Frame<'_>) {
    let Frame {
        s,
        pal,
        cx,
        cy,
        rad,
        alive,
        beat,
        cands,
        ..
    } = *f;
    // ── constellation: threads and nodes ───────────────────────────────
    let show_candidates = s.mode != Mode::Observe && alive;
    if show_candidates {
        for (i, c, nx, ny) in cands {
            st.nodes += 1;
            let conf = c.confidence.clamp(0.0, 1.0);
            // Thread from the core: dotted (suggest), solid (chosen),
            // marching (act, chosen); wobble = low confidence.
            let solid = c.selected && matches!(s.mode, Mode::Prepare | Mode::Act);
            let march = if s.mode == Mode::Act && c.selected {
                beat.map(|b| b.rem_euclid(1.0) as f32).unwrap_or(0.0)
            } else {
                0.0
            };
            let (sx, sy) = (cx + (nx - cx) * 0.16, cy + (ny - cy) * 0.16);
            if solid {
                cv.line(
                    sx,
                    sy,
                    *nx,
                    *ny,
                    pal.fg,
                    0.55 + 0.35 * conf,
                    1.0 + conf * 1.5,
                );
                if s.mode == Mode::Act {
                    cv.dotted(sx, sy, *nx, *ny, pal.accent, 0.9, 12.0, march);
                }
            } else {
                let wob = (1.0 - conf) * rad * 0.04;
                let segs = 24;
                let mut px = sx;
                let mut py = sy;
                for k in 1..=segs {
                    let t = k as f32 / segs as f32;
                    let (lx, ly) = (sx + (nx - sx) * t, sy + (ny - sy) * t);
                    let (dx, dy) = (ny - sy, sx - nx);
                    let n = (dx * dx + dy * dy).sqrt().max(1.0);
                    let off = hash(*i as u32 * 17 + k as u32, 3) * wob * (t * (1.0 - t) * 4.0);
                    let (qx, qy) = (lx + dx / n * off, ly + dy / n * off);
                    if k % 2 == 0 || c.selected {
                        cv.line(px, py, qx, qy, pal.dim, 0.3 + 0.4 * c.score, 1.0);
                    }
                    px = qx;
                    py = qy;
                }
            }
            st.threads += 1;
            // Node: size = score; rings: coherent when confident.
            let size = rad * (0.018 + 0.03 * c.score);
            let col = if c.selected { pal.fg } else { pal.dim };
            cv.dot(*nx, *ny, size * 0.45, col, 0.9);
            let rings_n = 1 + (conf * 3.0) as usize;
            for k in 0..rings_n {
                let rr = size * (0.9 + 0.45 * k as f32);
                let off = (1.0 - conf) * 3.0;
                let n = 40;
                for j in 0..n {
                    let a = j as f32 / n as f32 * TAU;
                    let d = hash(j as u32 + k as u32 * 97, *i as u32) * off;
                    cv.dot(
                        nx + a.cos() * (rr + d),
                        ny + a.sin() * (rr + d),
                        0.6,
                        col,
                        0.5,
                    );
                }
            }
            // Factor ticks: tempo, harmony, energy (outward, 0…1 long).
            for (j, f) in [c.factors.tempo, c.factors.harmony, c.factors.energy]
                .into_iter()
                .enumerate()
            {
                let a = (ny - cy).atan2(nx - cx) - 0.5 + j as f32 * 0.5;
                let (x0, y0) = (nx + a.cos() * size * 1.5, ny + a.sin() * size * 1.5);
                let l = size * (0.4 + 2.2 * f.clamp(0.0, 1.0));
                cv.line(
                    x0,
                    y0,
                    x0 + a.cos() * l,
                    y0 + a.sin() * l,
                    if c.selected { pal.fg } else { pal.faint },
                    0.8,
                    1.0,
                );
            }
            if c.locked {
                // Lock: square brackets.
                let b = size * 2.4;
                for (sx2, sy2) in [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                    let (x, y) = (nx + sx2 * b, ny + sy2 * b);
                    cv.line(x, y, x - sx2 * b * 0.4, y, pal.accent, 0.9, 1.0);
                    cv.line(x, y, x, y - sy2 * b * 0.4, pal.accent, 0.9, 1.0);
                }
            }
        }
    }

}

/// The trajectory: the planned transition.
fn draw_trajectory(cv: &mut Canvas, st: &mut Stats, f: &Frame<'_>) {
    let Frame {
        s,
        pal,
        cx,
        cy,
        rad,
        beat,
        pulse,
        cands,
        chosen,
        ..
    } = *f;
    // ── trajectory (the TransitionPlan) ────────────────────────────────
    if let (Some(tr), Some((_, _, tx, ty))) = (s.transition.as_ref(), chosen.or(cands.first())) {
        st.trajectory = true;
        let (tx, ty) = (*tx, *ty);
        let total_beats = (tr.bars.max(0.25) * 4.0) as f64;
        let into = beat.map(|b| b - tr.start_beat).unwrap_or(-1.0);
        let progress = if tr.started {
            (into / total_beats).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let (dx, dy) = (tx - cx, ty - cy);
        let (nx, ny) = (-dy, dx);
        let n = (nx * nx + ny * ny).sqrt().max(1.0);
        let (nx, ny) = (nx / n, ny / n);
        let steps = 64;
        let point = |t: f32, lane: f32| -> (f32, f32) {
            // Blend: two lanes that overlap in the middle; cut: a kink.
            let bend = match tr.style.as_str() {
                "cut" => {
                    if t > 0.85 {
                        0.0
                    } else {
                        rad * 0.05
                    }
                }
                "filterout" => rad * 0.04 * (1.0 - t),
                _ => rad * 0.06 * (t * PI).sin(),
            };
            let (x, y) = (cx + dx * t, cy + dy * t);
            (x + nx * (bend + lane), y + ny * (bend + lane))
        };
        let lanes: &[f32] = if tr.style == "blend" {
            &[-3.0, 3.0]
        } else {
            &[0.0]
        };
        for &lane in lanes {
            let mut prev = point(0.12, lane);
            for k in 1..=steps {
                let t = 0.12 + 0.88 * k as f32 / steps as f32;
                let cur = point(t, lane * (1.0 - (t * PI).sin()));
                let done = t <= 0.12 + 0.88 * progress;
                let width = if tr.style == "filterout" {
                    1.0 + 2.0 * (1.0 - t)
                } else {
                    1.6
                };
                let col = if done && tr.started {
                    pal.accent
                } else if tr.ready {
                    pal.fg
                } else {
                    pal.dim
                };
                cv.line(
                    prev.0,
                    prev.1,
                    cur.0,
                    cur.1,
                    col,
                    if tr.ready { 0.9 } else { 0.5 },
                    width,
                );
                prev = cur;
            }
        }
        // Loops: repeated segment ticks; effects: fading echoes past the end.
        if tr.loops {
            for k in 0..4 {
                let t = 0.2 + k as f32 * 0.06;
                let (x, y) = point(t, 0.0);
                cv.line(
                    x - nx * 6.0,
                    y - ny * 6.0,
                    x + nx * 6.0,
                    y + ny * 6.0,
                    pal.fg,
                    0.7,
                    1.0,
                );
            }
        }
        if tr.effects {
            for k in 1..4 {
                let (x, y) = point(1.0, 0.0);
                cv.dot(
                    x + dx * 0.05 * k as f32,
                    y + dy * 0.05 * k as f32,
                    2.0,
                    pal.dim,
                    0.5 / k as f32,
                );
            }
        }
        // Carrier (executing), or the bars left counting down (ready).
        if tr.started {
            let (x, y) = point(0.12 + 0.88 * progress, 0.0);
            cv.dot(x, y, 3.5 + 2.0 * pulse, pal.accent, 1.0);
        } else if into < 0.0 {
            let bars_left = ((-into) / 4.0).ceil().min(8.0) as usize;
            for k in 0..bars_left {
                let t = 0.12 - 0.02 * (k as f32 + 1.0);
                let (x, y) = point(t.max(0.0), 0.0);
                cv.dot(
                    x,
                    y,
                    1.8 + if k == 0 { pulse * 2.0 } else { 0.0 },
                    pal.fg,
                    0.8,
                );
            }
        }
    }

}

/// The core, and the evaluation scan.
fn draw_core(cv: &mut Canvas, _st: &mut Stats, f: &Frame<'_>) {
    let Frame {
        s,
        m,
        pal,
        view,
        cx,
        cy,
        rad,
        alive,
        pulse,
        confidence,
        chosen,
        ..
    } = *f;
    // ── core ───────────────────────────────────────────────────────────
    let core_r = rad * if view == View::Compact { 0.26 } else { 0.16 };
    let bias = chosen.map(|(_, _, x, y)| (y - cy).atan2(x - cx));
    let stretch = match s.phase {
        Phase::Preparing => 0.12,
        Phase::Ready => 0.22,
        Phase::Executing => 0.3,
        _ => 0.0,
    };
    let fragments = s
        .override_age_s
        .map(|a| (1.0 - (a / 8.0) as f32).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    let core_rings = match s.phase {
        Phase::Idle => 1,
        Phase::Unavailable => 2,
        _ => 4,
    };
    let hollow = s.mode == Mode::Observe;
    for k in 0..core_rings {
        let rr = core_r * (0.45 + 0.18 * k as f32) * (1.0 + pulse * 0.08);
        // Misalignment: low confidence (or no candidates yet) shifts rings.
        let mis = (1.0 - confidence) * 0.35 * (k as f32);
        let rot = if m.reduced_motion {
            0.0
        } else {
            (m.t as f32)
                * 0.05
                * (k as f32 + 1.0)
                * if matches!(s.phase, Phase::Evaluating | Phase::Comparing) {
                    3.0
                } else {
                    1.0
                }
        };
        let n = if view == View::Compact { 48 } else { 120 };
        for j in 0..n {
            let a = j as f32 / n as f32 * TAU;
            // Fragmentation after a human override: gaps that close.
            if fragments > 0.0 && ((a * 3.0 + k as f32).sin() > 1.0 - fragments * 1.6) {
                continue;
            }
            let mut r = rr * (1.0 + mis * (a * 2.0 + rot).sin() * 0.3);
            if let Some(b) = bias {
                r *= 1.0 + stretch * (a - b).cos().max(0.0).powi(2);
            }
            let col = if matches!(s.phase, Phase::Executing | Phase::Ready) {
                mix(pal.fg, pal.accent, 0.35)
            } else if confidence < 0.45 && alive && !hollow {
                // Unsure: the core says so in the theme's warning colour.
                mix(pal.fg, pal.warn, 0.55)
            } else {
                pal.fg
            };
            let alpha = if hollow {
                0.35
            } else {
                0.55 + 0.35 * confidence
            } * if s.phase == Phase::Unavailable {
                0.4
            } else {
                1.0
            };
            cv.dot(cx + a.cos() * r, cy + a.sin() * r, 0.9, col, alpha);
        }
    }
    if !hollow && alive {
        cv.dot(cx, cy, core_r * 0.12 * (1.0 + pulse * 0.6), pal.fg, 0.9);
    }
    // Evaluating: a scan (latency made visible, not a spinner).
    if matches!(s.phase, Phase::Evaluating) && !m.reduced_motion {
        let a = (m.t as f32 * 1.7).rem_euclid(TAU);
        cv.line(
            cx,
            cy,
            cx + a.cos() * core_r * 1.3,
            cy + a.sin() * core_r * 1.3,
            pal.dim,
            0.6,
            1.0,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> VisualState {
        let path = format!("{}/fixtures/aivis/{name}.json", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("{path}")))
            .unwrap()
    }

    fn frame(name: &str, view: View) -> (Vec<u8>, Stats) {
        let s = fixture(name);
        let m = Moment::at(&s, s.clock.nsec + 250_000_000, 0, 1.0, false);
        render(320, 240, &s, m, &Palette::mono(), view, Quality::Full)
    }

    fn digest(px: &[u8]) -> u64 {
        px.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
            (h ^ *b as u64).wrapping_mul(0x0100_0000_01b3)
        })
    }

    /// Pixel digests of every fixture in every view and quality at four
    /// moments: run before and after a change to the drawing and `cmp` the
    /// files to show it draws exactly as before.
    /// `PATCH_AIVIS_DUMP=<file> cargo test --release -p patch-surfaces --lib
    /// render_dump -- --ignored`
    #[test]
    #[ignore = "render digest dump: PATCH_AIVIS_DUMP"]
    fn render_dump() {
        use std::io::Write;
        let mut out = std::fs::File::create(std::env::var("PATCH_AIVIS_DUMP").unwrap()).unwrap();
        let dir = format!("{}/fixtures/aivis", env!("CARGO_MANIFEST_DIR"));
        let mut names: Vec<String> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(String::from)).collect();
        names.sort();
        for name in names {
            let s = fixture(&name);
            for view in [View::Compact, View::Operator, View::Analysis] {
                for q in [Quality::Minimal, Quality::Reduced, Quality::Full] {
                    for (dt, reduced) in [(0u64, false), (130_000_000, false), (777_000_000, false), (250_000_000, true)] {
                        let m = Moment::at(&s, s.clock.nsec + dt, 0, 1.0, reduced);
                        let (px, st) = render(320, 240, &s, m, &Palette::mono(), view, q);
                        writeln!(out, "{name} {view:?} {q:?} {dt} {reduced} {:016x} {:?}", digest(&px), st).unwrap();
                    }
                }
            }
        }
    }

    #[test]
    fn fixtures_render_their_state() {
        let (_, obs) = frame("observing", View::Operator);
        assert_eq!(obs.nodes, 0, "observe: no candidates shown");
        let (_, three) = frame("three_candidates", View::Operator);
        assert_eq!(three.nodes, 3);
        assert!(!three.trajectory);
        let (_, ready) = frame("transition_ready", View::Operator);
        assert!(ready.trajectory, "a prepared plan draws its trajectory");
        let (_, active) = frame("transition_active", View::Operator);
        assert!(active.trajectory);
        let (_, crowd) = frame("crowd_energy_high", View::Operator);
        assert!(crowd.environment, "sensors present: the field is drawn");
        assert!(!three.environment, "no sensors: no environment field");
        let (_, off) = frame("ai_unavailable", View::Operator);
        assert_eq!(off.nodes, 0, "unavailable: no prediction branches");
        assert!(off.lattice_points > 0, "…but the music still shows");
    }

    #[test]
    fn rendering_is_deterministic_and_state_driven() {
        let (a, _) = frame("three_candidates", View::Operator);
        let (b, _) = frame("three_candidates", View::Operator);
        assert_eq!(digest(&a), digest(&b), "same state, same pixels");
        let (low, _) = frame("low_confidence", View::Operator);
        assert_ne!(digest(&a), digest(&low), "confidence changes the geometry");
        let (ov, _) = frame("human_override", View::Operator);
        assert_ne!(digest(&a), digest(&ov));
    }

    #[test]
    fn the_pulse_follows_the_heard_beat() {
        let s = fixture("three_candidates");
        let on_beat = Moment::at(&s, s.clock.nsec, 0, 0.0, false);
        let later = Moment::at(&s, s.clock.nsec + 250_000_000, 0, 0.0, false);
        let bpm = s.lead().unwrap().bpm.unwrap();
        assert!(
            (later.beat.unwrap() - on_beat.beat.unwrap() - 0.25 * bpm / 60.0).abs() < 1e-9,
            "interpolated from the stamp"
        );
        // Delayed by the output latency: what's heard lags the engine.
        let heard = Moment::at(&s, s.clock.nsec + 250_000_000, 250_000_000, 0.0, false);
        assert!((heard.beat.unwrap() - on_beat.beat.unwrap()).abs() < 1e-9);
    }

    #[test]
    fn every_view_and_quality_renders() {
        for v in [
            View::Compact,
            View::Operator,
            View::Analysis,
        ] {
            for q in [Quality::Minimal, Quality::Reduced, Quality::Full] {
                let s = fixture("transition_active");
                let (px, _) = render(
                    200,
                    120,
                    &s,
                    Moment::at(&s, s.clock.nsec, 0, 0.0, true),
                    &Palette::mono(),
                    v,
                    q,
                );
                assert_eq!(px.len(), 200 * 120 * 4);
            }
        }
    }

    /// Every fixture as a PPM image: `PATCH_AIVIS_SHOTS=dir cargo test -p
    /// patch-surfaces aivis_shots -- --ignored`.
    #[test]
    #[ignore]
    fn aivis_shots() {
        let dir = std::env::var("PATCH_AIVIS_SHOTS").unwrap_or_else(|_| "/tmp/aivis".into());
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "observing",
            "three_candidates",
            "low_confidence",
            "transition_ready",
            "transition_active",
            "crowd_energy_high",
            "ai_unavailable",
            "human_override",
        ] {
            let s = fixture(name);
            let (w, h) = (720, 480);
            let m = Moment::at(&s, s.clock.nsec + 60_000_000, 0, 1.0, false);
            let (px, _) = render(w, h, &s, m, &Palette::mono(), View::Operator, Quality::Full);
            let mut out = format!("P6 {w} {h} 255\n").into_bytes();
            for p in px.as_chunks::<4>().0 {
                out.extend_from_slice(&p[..3]);
            }
            std::fs::write(format!("{dir}/{name}.ppm"), out).unwrap();
        }
    }

    /// Frame cost per view and quality (release): `cargo test --release -p
    /// patch-surfaces aivis_cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn aivis_cost() {
        let s = fixture("transition_active");
        for (v, w, h) in [
            (View::Compact, 320, 200),
            (View::Operator, 1280, 720),
            (View::Operator, 1920, 1080),
        ] {
            for q in [Quality::Full, Quality::Reduced, Quality::Minimal] {
                let mut times = vec![];
                for i in 0..60 {
                    let m =
                        Moment::at(&s, s.clock.nsec + i * 16_666_667, 0, i as f64 / 60.0, false);
                    let t0 = std::time::Instant::now();
                    let _ = render(w, h, &s, m, &Palette::mono(), v, q);
                    times.push(t0.elapsed().as_secs_f64() * 1000.0);
                }
                times.sort_by(f64::total_cmp);
                println!(
                    "{v:?} {w}×{h} {q:?}: median {:.2} ms, p95 {:.2} ms, max {:.2} ms",
                    times[30], times[57], times[59]
                );
            }
        }
    }
}

/// What every drawing pass of [`render`] reads: the state and moment, the
/// geometry, the heard timing and the placed candidates.
#[derive(Clone, Copy)]
struct Frame<'a> {
    s: &'a VisualState,
    m: Moment,
    pal: &'a Palette,
    view: View,
    q: Quality,
    cx: f32,
    cy: f32,
    rad: f32,
    lead: Option<&'a patch_core::aivis::Source>,
    alive: bool,
    beat: Option<f64>,
    beat_frac: f32,
    is_down: bool,
    phrase_idx: i32,
    phrase_pos: f32,
    pulse: f32,
    energy: f32,
    tension: f32,
    ahead_curve: &'a [f32],
    timing_conf: f32,
    confidence: f32,
    /// Visible candidates with their places (index, candidate, x, y).
    cands: &'a [(usize, &'a patch_core::aivis::Candidate, f32, f32)],
    chosen: Option<&'a (usize, &'a patch_core::aivis::Candidate, f32, f32)>,
}
