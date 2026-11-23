//! How mixable a track's beat is: where each kick really lands against one
//! straight grid.
//!
//! A beat grid is a line (a tempo and a first beat). Machine-made dance
//! music sits on it within a millisecond from the first bar to the last;
//! a live drummer, an edit with a tempo change or a track with long
//! beatless stretches does not. Two decks can only be layered for a long
//! time (a 32-bar blend, a bassline under another track's melody, a
//! mash-up) when both beats are straight: a 10 ms wander is an audible
//! flam. So per beat we find the kick, fit one line through all of them,
//! and measure what's left:
//!
//! * **jitter**: how far single kicks scatter around the local grid (ms),
//! * **drift**: how far the beat wanders from one straight grid over the
//!   track (ms, the worst 16-beat stretch),
//! * **coverage**: the share of beats with a clear kick,
//! * **beatless**: the stretches without one (breakdowns, intros),
//! * **mixability**: 0…1 from those three.
//!
//! Two resolutions. The **coarse** pass works on the analysis' onset
//! envelope (11.6 ms frames, sub-frame peak interpolation): it comes with
//! every analysis. The **precise** pass reads the low band at the full
//! sample rate and finds each kick's attack to a fraction of a
//! millisecond; it costs a decode, so the AI DJ asks for it only for
//! tracks it intends to layer. Both put a beat where the kick's attack
//! starts (the coarse pass corrects its envelope's lag), so a precise deck
//! and a coarse deck meet.

use crate::tempo;
use serde::{Deserialize, Serialize};

/// Bump when the measurement changes: older grids are measured again (in
/// the background; until then they are still used).
pub const METHOD: u32 = 12;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GridQuality {
    /// Which measurement made it ([`METHOD`]); 0 or 1: an older one.
    #[serde(default)]
    pub method: u32,
    /// Measured at the full sample rate (else on the half-rate mono).
    pub precise: bool,
    /// The straight grid through the kicks.
    pub bpm: f64,
    /// First beat of that grid, seconds.
    pub offset: f64,
    /// Which grid beat (0…3) starts a bar, with that grid; and how sure.
    pub downbeat: Option<u8>,
    pub downbeat_confidence: f32,
    /// Scatter of single kicks around the local grid, ms (robust RMS).
    pub jitter_ms: f32,
    /// Worst wander of the beat from the straight grid, ms.
    pub drift_ms: f32,
    /// Share of beats with a clear kick, 0…1.
    pub coverage: f32,
    /// Stretches of 8 beats or more without a kick, seconds [from, to].
    pub beatless: Vec<[f32; 2]>,
    /// 0…1: how long this beat can run under another one.
    pub mixability: f32,
    /// Raw kick salience relative to other regular elements; not a
    /// calibrated probability (a bassline on the off-beat can rival it).
    #[serde(default = "sure")]
    pub phase_confidence: f32,
    /// Where the kick's click attack sits relative to its body, ms
    /// (negative: the click leads). Within ±`MAX_CLICK_SHIFT_MS`, `offset`
    /// already includes it: the beat is on the click.
    #[serde(default)]
    pub click_offset_ms: Option<f32>,
}

fn sure() -> f32 {
    1.0
}

impl GridQuality {

    /// A straight machine beat: long layering is safe.
    pub fn straight(&self) -> bool {
        self.drift_ms <= 4.0 && self.jitter_ms <= 3.0 && self.phase_confidence >= 0.25
    }

    /// In a few words, for the DJ.
    pub fn describe(&self) -> String {
        let kind = if self.straight() {
            "straight grid"
        } else if self.mixability >= 0.5 {
            "slightly loose grid"
        } else {
            "loose grid"
        };
        format!(
            "{kind} ({}: ±{:.1} ms, drift {:.1} ms, kick on {:.0} % of beats) · mixability {:.0} %",
            if self.precise { "precise" } else { "coarse" },
            self.jitter_ms,
            self.drift_ms,
            self.coverage * 100.0,
            self.mixability * 100.0
        )
    }
}

/// 0…1 from the measurements. Jitter up to 2 ms and drift up to 3 ms are
/// inaudible under a blend; 12 ms and 25 ms are a flam at any length.
pub fn mixability(jitter_ms: f32, drift_ms: f32, coverage: f32) -> f32 {
    let j = (1.0 - (jitter_ms - 2.0) / 10.0).clamp(0.0, 1.0);
    let d = (1.0 - (drift_ms - 3.0) / 22.0).clamp(0.0, 1.0);
    let c = ((coverage - 0.35) / 0.5).clamp(0.0, 1.0);
    // Geometric-like: one bad property can't be averaged away.
    (j * 0.35 + d * 0.4 + c * 0.25) * j.max(0.2).min(d.max(0.2)).powf(0.3)
}

/// Kick times per grid beat: `(beat index, time in units)`, only where a
/// kick stands out. `find(k)` returns the onset near predicted beat k.
fn fit_line(pts: &[(f64, f64)]) -> Option<(f64, f64)> {
    let n = pts.len() as f64;
    if n < 16.0 {
        return None;
    }
    let (sx, sy) = pts.iter().fold((0.0, 0.0), |(a, b), (x, y)| (a + x, b + y));
    let (mx, my) = (sx / n, sy / n);
    let (mut sxx, mut sxy) = (0.0, 0.0);
    for (x, y) in pts {
        sxx += (x - mx) * (x - mx);
        sxy += (x - mx) * (y - my);
    }
    (sxx > 0.0).then(|| {
        let slope = sxy / sxx;
        (slope, my - slope * mx)
    })
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    let m = v.len() / 2;
    if v.len() % 2 == 1 {
        v[m]
    } else {
        (v[m - 1] + v[m]) / 2.0
    }
}

/// A kick found near grid beat `k`: its time (seconds) and strength.
#[derive(Clone, Copy)]
struct Hit {
    k: f64,
    t: f64,
    strength: f64,
}

struct Fit {
    period: f64,
    first: f64,
    jitter_ms: f32,
    drift_ms: f32,
    coverage: f32,
    gaps: Vec<(usize, usize)>,
}

/// One straight grid through the kicks, and how they sit around it.
///
/// The line is the one most strong hits agree with (within `tol` seconds):
/// pairs of hits far apart propose lines, the best-supported one wins, a
/// least-squares fit through its supporters refines it. So a breakdown's
/// bass notes or a section without a kick can't tilt it (a plain fit
/// through everything found turned 126.00 BPM into 126.05).
///
/// Then only real kicks count (as strong as the track's typical kick), and
/// only where they are steady: drift is how far a steady 16-beat stretch
/// sits off the line; jitter the scatter within stretches.
fn straight_fit(hits: &[Hit], beats: usize, period: f64, tol: f64) -> Option<Fit> {
    let loud = {
        let mut v: Vec<f64> = hits.iter().map(|h| h.strength).collect();
        v.sort_by(f64::total_cmp);
        v.get(v.len() * 9 / 10).copied().unwrap_or(0.0)
    };
    let strong: Vec<Hit> = hits
        .iter()
        .copied()
        .filter(|h| h.strength > loud * 0.25)
        .collect();
    let dbg = std::env::var_os("PATCH_GRID_DEBUG").is_some();
    if dbg {
        eprintln!(
            "fit: {} hits, {} strong; first strengths {:?}",
            hits.len(),
            strong.len(),
            hits.iter()
                .take(12)
                .map(|h| format!(
                    "{}:{:.3}@{:+.1}",
                    h.k,
                    h.strength,
                    (h.t - hits[0].t - (h.k - hits[0].k) * period) * 1e3
                ))
                .collect::<Vec<_>>()
        );
    }
    if strong.len() < 16 {
        return None;
    }
    // Consensus: lines through pairs from the first and last 40 %.
    let n = strong.len();
    let (early, late) = (&strong[..n * 2 / 5], &strong[n * 3 / 5..]);
    let support = |slope: f64, icpt: f64| -> f64 {
        strong
            .iter()
            .filter(|h| (h.t - (icpt + slope * h.k)).abs() < tol)
            .map(|h| h.strength.min(loud))
            .sum()
    };
    let mut best = (0.0f64, period, strong[0].t - strong[0].k * period);
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    for _ in 0..600 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let a = early[(seed % early.len().max(1) as u64) as usize];
        let b = late[((seed >> 32) % late.len().max(1) as u64) as usize];
        if b.k <= a.k {
            continue;
        }
        let slope = (b.t - a.t) / (b.k - a.k);
        if (slope / period - 1.0).abs() > 0.02 {
            continue;
        }
        let icpt = a.t - slope * a.k;
        let s = support(slope, icpt);
        if s > best.0 {
            best = (s, slope, icpt);
        }
    }
    let (mut slope, mut icpt) = (best.1, best.2);
    for _ in 0..3 {
        let on: Vec<(f64, f64)> = strong
            .iter()
            .filter(|h| (h.t - (icpt + slope * h.k)).abs() < tol)
            .map(|h| (h.k, h.t))
            .collect();
        let Some((s, i)) = fit_line(&on) else { break };
        (slope, icpt) = (s, i);
    }
    if (slope / period - 1.0).abs() > 0.02 {
        return None;
    }
    // Real kicks: the typical kick's strength, near the grid.
    let typical = {
        let mut v: Vec<f64> = strong
            .iter()
            .filter(|h| (h.t - (icpt + slope * h.k)).abs() < tol)
            .map(|h| h.strength)
            .collect();
        median(&mut v)
    };
    let kicks: Vec<(usize, f64)> = strong
        .iter()
        .filter(|h| {
            h.strength >= typical * 0.4 && (h.t - (icpt + slope * h.k)).abs() < period * 0.08
        })
        .map(|h| (h.k as usize, (h.t - (icpt + slope * h.k)) * 1e3))
        .collect();
    if let Some(f) = std::env::var_os("PATCH_GRID_DUMP") {
        let _ = std::fs::write(
            f,
            kicks
                .iter()
                .map(|(k, r)| format!("{k} {r:.2}\n"))
                .collect::<String>(),
        );
    }
    // Where the kick really is, per stretch of 17 beats: the median of its
    // hits, when at least 10 agree within 8 ms (else no steady kick there).
    let steady_median = |k: usize| -> Option<f64> {
        let mut w: Vec<f64> = kicks
            .iter()
            .filter(|(j, _)| j.abs_diff(k) <= 8)
            .map(|(_, r)| *r)
            .collect();
        let m = median(&mut w);
        if w.iter().filter(|x| (*x - m).abs() < 8.0).count() >= 10 {
            return Some(m);
        }
        // A kick with two attacks ~10 ms apart (a layered kick, a clap on
        // it): the nearest one alternates beat by beat and the median falls
        // between them, where neither is. The larger of the two then, when
        // it holds most of the window on its own.
        let (count, at) = w
            .iter()
            .map(|c| {
                let near: Vec<f64> = w.iter().copied().filter(|x| (x - c).abs() < 3.0).collect();
                (near.len(), near.iter().sum::<f64>() / near.len() as f64)
            })
            .max_by(|a, b| a.0.cmp(&b.0).then(b.1.abs().total_cmp(&a.1.abs())))?;
        (count >= 7 && count * 5 >= w.len() * 2).then_some(at)
    };
    let local: Vec<Option<f64>> = kicks.iter().map(|(k, _)| steady_median(*k)).collect();
    // Drift is a wander: the kick's local position moves gradually (under
    // a millisecond per beat even for a big sway). Another element taking
    // over (a riser, a swell before the kick) jumps there in a beat. So
    // stretches connect while their position changes by under 4 ms from
    // one kick to the next; those connected to a stretch on the line are
    // the kick, and only they count.
    let mut seg = vec![usize::MAX; kicks.len()];
    let mut segs: Vec<(bool, f64)> = vec![]; // (touches the line, worst |m|)
    let mut prev: Option<f64> = None;
    for (i, m) in local.iter().enumerate() {
        let Some(m) = *m else {
            continue;
        };
        if prev.is_none_or(|p| (m - p).abs() >= 4.0) {
            segs.push((false, 0.0));
        }
        let last = segs.len() - 1;
        seg[i] = last;
        segs[last].0 |= m.abs() < 3.0;
        segs[last].1 = segs[last].1.max(m.abs());
        prev = Some(m);
    }
    let drift = segs
        .iter()
        .filter(|s| s.0)
        .map(|s| s.1)
        .fold(0.0f64, f64::max);
    let mut dev = vec![];
    let mut agree = vec![false; beats];
    let mut present = vec![false; beats];
    for (i, (k, r)) in kicks.iter().enumerate() {
        let Some(m) = local[i] else { continue };
        if seg[i] == usize::MAX || !segs[seg[i]].0 || *k >= beats {
            continue;
        }
        present[*k] = true;
        if (r - m).abs() < 8.0 {
            dev.push((r - m).abs());
        }
        // On the grid: this beat's kick where the kick is right now.
        if (r - m).abs() < (tol * 1e3).max(1.5) {
            agree[*k] = true;
        }
    }
    if dbg {
        eprintln!(
            "fit: {} kicks, {} steady deviations, {} segments ({} on the line)",
            kicks.len(),
            dev.len(),
            segs.len(),
            segs.iter().filter(|s| s.0).count()
        );
    }
    if dev.len() < 16 {
        return None;
    }
    let jitter = median(&mut dev) * 1.4826;
    let mut gaps = vec![];
    let mut run: Option<usize> = None;
    for (k, h) in present.iter().chain(std::iter::once(&true)).enumerate() {
        match (h, run) {
            (false, None) => run = Some(k),
            (true, Some(s)) => {
                if k - s >= 8 {
                    gaps.push((s, k));
                }
                run = None;
            }
            _ => {}
        }
    }
    // Coverage: of the beats outside beatless stretches (breakdowns are
    // part of a track, not a flaw), the share with the kick on the grid.
    let in_gaps: usize = gaps.iter().map(|(a, b)| b - a).sum();
    let with_beat = beats.saturating_sub(in_gaps).max(1);
    let coverage = agree.iter().filter(|a| **a).count() as f32 / with_beat as f32;
    // A stretch ends where the kick is steady again, which takes ten kicks
    // in the window: 1-4 beats after the groove really returns. The drop
    // after a breakdown is placed on that end, so move it back over the
    // kicks leading into it (one missing beat allowed, at most 8 beats).
    // Blind test 2026-10-19: the drop on the returning kick beat the late
    // one (6 pairs of reported breakdown entries).
    let hit: std::collections::HashSet<usize> = kicks.iter().map(|(k, _)| *k).collect();
    for (s, e) in &mut gaps {
        if *e >= beats {
            continue;
        }
        let end = *e;
        while end - *e < 8 {
            let step = if hit.contains(&(*e - 1)) {
                1
            } else if end - *e < 7 && *e >= 2 && hit.contains(&(*e - 2)) {
                2
            } else {
                break;
            };
            if *e - step < *s + 8 {
                break;
            }
            *e -= step;
        }
    }
    Some(Fit {
        period: slope,
        first: icpt,
        jitter_ms: jitter as f32,
        drift_ms: drift as f32,
        coverage,
        gaps,
    })
}

/// A downbeat vote from the structure: the bass returns (a drop, the body
/// after an intro) on the first beat of a bar. Per grid beat, the low
/// band's energy over the next 8 beats against the previous 8; the beat
/// positions (mod 4) of big rises vote. Returns (beat of four, confidence).
fn downbeat_from_rises(low_per_beat: &[f64]) -> Option<(u8, f32)> {
    let n = low_per_beat.len();
    if n < 64 {
        return None;
    }
    let mut votes = [0.0f64; 4];
    let mut total = 0.0;
    for k in 8..n - 8 {
        let before: f64 = low_per_beat[k - 8..k].iter().sum::<f64>() / 8.0;
        let after: f64 = low_per_beat[k..k + 8].iter().sum::<f64>() / 8.0;
        if after > before * 2.5 && after > 0.0 {
            // Only the sharpest beat of a rise counts (not its neighbours).
            let step = low_per_beat[k] - low_per_beat[k - 1];
            let sharpest = (k.saturating_sub(2)..(k + 3).min(n))
                .filter(|&j| j >= 1)
                .all(|j| low_per_beat[j] - low_per_beat[j - 1] <= step);
            if sharpest {
                let w = (after / before.max(1e-9)).ln();
                votes[k % 4] += w;
                total += w;
            }
        }
    }
    if total <= 0.0 {
        return None;
    }
    let (best, top) = votes.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
    Some((best as u8, (*top / total) as f32))
}

/// The kick band: below this (four poles, zero phase).
const KICK_BAND_HZ: f64 = 300.0;

/// The low band's amplitude envelope, squared (energy), without ripple and
/// without delay, at `rate / decim`.
///
/// Below 300 Hz: two one-pole low-passes run forward and then backward
/// (zero phase: no lag, whatever a kick's pitch). Its energy as the
/// squared magnitude of the analytic signal (via FFT): a sustained 73 Hz
/// bass is a flat line, not a ripple at 146 Hz that looks like a string of
/// attacks.
fn low_envelope(mono: &[f32], rate: u32, decim: usize) -> Vec<f64> {
    // 300 Hz: a kick's attack starts well above its body (an 808 sweeps
    // 160 → 50 Hz); at 150 Hz its first 20 ms were filtered away.
    let a = (-std::f64::consts::TAU * KICK_BAND_HZ / rate as f64).exp();
    let mut y: Vec<f64> = mono.iter().map(|&x| x as f64).collect();
    let pass = |y: &mut Vec<f64>, backward: bool| {
        let mut s = 0.0f64;
        let n = y.len();
        for k in 0..n {
            let i = if backward { n - 1 - k } else { k };
            s = (1.0 - a) * y[i] + a * s;
            y[i] = s;
        }
    };
    for backward in [false, false, true, true] {
        pass(&mut y, backward);
    }
    // Decimate: the band is far below the new Nyquist (rate / 2 / decim).
    let low: Vec<f64> = y.iter().step_by(decim).copied().collect();
    let n = low.len().next_power_of_two().max(2);
    let mut re = low.clone();
    re.resize(n, 0.0);
    let mut im = vec![0.0; n];
    patch_meter::spectrum::fft(&mut re, &mut im);
    // Analytic signal: positive frequencies doubled, negative ones gone.
    for k in 1..n / 2 {
        re[k] *= 2.0;
        im[k] *= 2.0;
    }
    for k in n / 2 + 1..n {
        re[k] = 0.0;
        im[k] = 0.0;
    }
    // Inverse FFT via the conjugate.
    for v in im.iter_mut() {
        *v = -*v;
    }
    patch_meter::spectrum::fft(&mut re, &mut im);
    let scale = 1.0 / n as f64;
    (0..low.len())
        .map(|i| (re[i] * scale).powi(2) + (im[i] * scale).powi(2))
        .collect()
}

/// High-frequency attacks (above ~2 kHz): how sharply that band's energy
/// rises, per 0.5 ms.
struct Clicks {
    rise: Vec<f32>,
    per_s: f64,
}

impl Clicks {
    /// The sharpest rise within ±3 ms of `t`.
    fn near(&self, t: f64) -> f64 {
        let (a, b) = (
            ((t - 0.003) * self.per_s).max(0.0) as usize,
            ((t + 0.003) * self.per_s) as usize + 1,
        );
        self.rise
            .get(a..b.min(self.rise.len()))
            .map_or(0.0, |w| w.iter().cloned().fold(0.0, f32::max) as f64)
    }
}

fn click_rise(mono: &[f32], rate: u32) -> Clicks {
    let r = rate as f64;
    let a = (-std::f64::consts::TAU * 2000.0 / r).exp();
    let sm = (-1.0 / (0.0005 * r)).exp();
    let (mut lp, mut e) = (0.0f64, 0.0f64);
    let step = ((0.0005 * r) as usize).max(1);
    let lag = ((0.002 * r) as usize).max(1);
    let energy: Vec<f32> = mono
        .iter()
        .map(|&x| {
            lp = (1.0 - a) * x as f64 + a * lp;
            let hp = x as f64 - lp;
            e = (1.0 - sm) * hp * hp + sm * e;
            e as f32
        })
        .collect();
    let rise = (0..energy.len() / step)
        .map(|k| {
            let i = k * step;
            if i < lag {
                0.0
            } else {
                (energy[i] - energy[i - lag]).max(0.0)
            }
        })
        .collect();
    Clicks {
        rise,
        per_s: r / step as f64,
    }
}

/// Every kick in the track, found without any grid.
///
/// A kick is the low band's (below 150 Hz) energy jumping by a lot, fast:
/// within a few milliseconds. A sidechained bassline swells back more
/// slowly after the kick and off-beat hats carry little low energy:
/// neither makes such a jump. (On produced tracks the older detector sat on
/// the bassline's return, 40 ms late, or on the hats.)
///
/// Its time: where the envelope first climbs 10 % of the way from the dip
/// before it to its peak, the start a listener hears, for clicky, soft and
/// 808 kicks alike. Returns (time s, strength: the 4 ms jump) per kick, the
/// envelope, and its rate.
fn kick_onsets(mono: &[f32], rate: u32) -> (Vec<(f64, f64)>, Vec<f32>, f64) {
    let decim = (rate as usize / 2756).max(1);
    let env = low_envelope(mono, rate, decim);
    let r = rate as f64 / decim as f64;
    let click = click_rise(mono, rate);
    let n = env.len();
    let ms = |v: f64| ((v * 0.001 * r).round() as usize).max(1);
    let (ahead, behind) = (ms(3.0), ms(1.0));
    let jump_at = |i: usize| env[i + ahead] - env[i - behind];
    // Local maxima of the 4 ms jump within ±10 ms.
    let half = ms(10.0);
    let mut cands = vec![];
    let mut dq: std::collections::VecDeque<usize> = Default::default();
    let (first, last) = (behind, n.saturating_sub(ahead + 1));
    let mut next = first;
    for c in first..last {
        while next < last && next <= c + half {
            while dq.back().is_some_and(|&j| jump_at(j) <= jump_at(next)) {
                dq.pop_back();
            }
            dq.push_back(next);
            next += 1;
        }
        while dq.front().is_some_and(|&j| j + half < c) {
            dq.pop_front();
        }
        if dq.front() == Some(&c) && jump_at(c) > 0.0 {
            cands.push(c);
        }
    }
    let mut out: Vec<(f64, f64, f64)> = Vec::with_capacity(cands.len());
    for i in cands {
        let lo = i.saturating_sub(ms(15.0));
        let hi = (i + ms(25.0)).min(n - 1);
        let (dip_at, dip) = (lo..=i)
            .map(|j| (j, env[j]))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        let peak = env[i..=hi].iter().cloned().fold(0.0, f64::max);
        if peak <= dip {
            continue;
        }
        let level = dip + (peak - dip) * KICK_LEVEL;
        let Some(j) = (dip_at..=hi).find(|&j| env[j] >= level) else {
            continue;
        };
        let (p, q) = (env[j.saturating_sub(1)], env[j]);
        let frac = if q > p {
            ((level - p) / (q - p)).clamp(0.0, 1.0)
        } else {
            1.0
        };
        // Sharpness: how much of the whole rise (over 20 ms) happens in the
        // 4 ms jump. A kick ~1; two low tones beating, or a swell, far less.
        let slow = env[i + ahead]
            - env[i.saturating_sub(ms(17.0))..=i]
                .iter()
                .cloned()
                .fold(f64::MAX, f64::min);
        let jump = jump_at(i);
        let sharp = if slow > 0.0 {
            (jump / slow).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let t = (j as f64 - 1.0 + frac) / r - KICK_ONSET_BIAS_S;
        out.push((t, jump * sharp * sharp, click.near(t)));
    }
    // A kick's beater clicks: high-frequency attack right at the low jump.
    // A sub or bass stab after the kick has none. The low jump counts more
    // with a click (claps and hats alone make no low jump).
    let mut clicks: Vec<f64> = out.iter().map(|o| o.2).filter(|c| *c > 0.0).collect();
    clicks.sort_by(f64::total_cmp);
    let typical_click = clicks
        .get(clicks.len() / 2)
        .copied()
        .unwrap_or(0.0)
        .max(1e-12);
    let mut out: Vec<(f64, f64)> = out
        .into_iter()
        .map(|(t, s, c)| (t, s * (0.35 + (c / typical_click).min(2.0))))
        .collect();
    // Jumps of one kick (its swell, its pitch sweep) trace back to the same
    // start: one onset per 10 ms, the strongest.
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f64, f64)> = Vec::with_capacity(out.len());
    for o in out {
        match merged.last_mut() {
            Some(last) if o.0 - last.0 < 0.010 => {
                if o.1 > last.1 {
                    *last = o;
                }
            }
            _ => merged.push(o),
        }
    }
    let mut out = merged;
    // Keep the clear ones: well above the track's own small jumps.
    let mut v: Vec<f64> = out.iter().map(|o| o.1).collect();
    v.sort_by(f64::total_cmp);
    let top = v.get(v.len() * 95 / 100).copied().unwrap_or(0.0);
    out.retain(|o| o.1 > top * 0.05);
    (out, env.iter().map(|v| *v as f32).collect(), r)
}

/// Where the beat's attack sits in `mono`, as a phase of `period` from time
/// zero (seconds): the high-band attack folded over the beats `keep` accepts,
/// its strongest point within 50 ms of the phase `near` (elsewhere: hats).
/// None without a clear one, or with fewer than `min_beats` beats to fold. For judging how two
/// tracks' kicks meet in a mix, by the same moment the grids mean.
pub fn attack_phase(
    mono: &[f32],
    rate: u32,
    period: f64,
    near: f64,
    keep: &dyn Fn(f64) -> bool,
    min_beats: usize,
) -> Option<f64> {
    let clicks = click_rise(mono, rate);
    let beats = (mono.len() as f64 / rate as f64 / period) as usize;
    fold_attack(
        &clicks,
        period,
        near,
        (0..beats).filter(|k| keep(*k as f64 * period)),
        min_beats,
    )
}

fn fold_attack(
    clicks: &Clicks,
    period: f64,
    near: f64,
    beats: impl Iterator<Item = usize>,
    min_beats: usize,
) -> Option<f64> {
    let cr = clicks.per_s;
    let n = (period * cr).round() as usize;
    let mut fold = vec![0.0f64; n];
    let mut used = 0;
    for k in beats {
        let t = k as f64 * period;
        let s = (t * cr).round() as usize;
        let Some(w) = clicks.rise.get(s..s + n) else {
            break;
        };
        let top = w.iter().cloned().fold(0.0, f32::max) as f64;
        if top > 0.0 {
            for (i, v) in w.iter().enumerate() {
                fold[i] += *v as f64 / top;
            }
            used += 1;
        }
    }
    let mut sorted = fold.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted.get(n / 2).copied()?;
    let reach = (0.050 * cr) as i64;
    let centre = (near.rem_euclid(period) * cr).round() as i64;
    let (at, top) = (-reach..=reach)
        .map(|d| (centre + d).rem_euclid(n as i64) as usize)
        .fold(
            (0, f64::MIN),
            |m, i| if fold[i] > m.1 { (i, fold[i]) } else { m },
        );
    (used >= min_beats && top > median * 4.0).then(|| at as f64 / cr)
}

/// The kicks in `mono` (times in seconds, clear ones only), as the grid
/// measurement finds them: for judging how two tracks' kicks meet in a mix.
pub fn kick_times(mono: &[f32], rate: u32) -> Vec<f64> {
    let (onsets, _, _) = kick_onsets(mono, rate);
    let mut v: Vec<f64> = onsets.iter().map(|o| o.1).collect();
    v.sort_by(f64::total_cmp);
    let strong = v.get(v.len() * 3 / 4).copied().unwrap_or(0.0) * 0.3;
    onsets
        .into_iter()
        .filter(|o| o.1 >= strong)
        .map(|o| o.0)
        .collect()
}

/// Farthest a kick's click may sit from its body and still carry the beat;
/// beyond it the "click" is another sound.
pub const MAX_CLICK_SHIFT_MS: f32 = 20.0;

/// Where a part's attacks sit against a grid, in ms: the click attack folded
/// over the part, wrapped to ±half a beat (positive: the part's beat is later
/// than the grid). None without four clear attacks (a breakdown, a riser).
///
/// `from_s` is where the part starts in the track: a fold reads the phase of
/// a slice against *its own* start, so a part cut from the middle of a track
/// has to say where it was cut from, or it measures nothing (the peak lands
/// anywhere and the reading is noise). The tempo is known, so a part needs no
/// fit either: the whole-track measurement wants 16 steady deviations and
/// refuses a part of a few bars even where the same audio measures straight
/// over a window.
pub fn part_phase(
    mono: &[f32],
    rate: u32,
    bpm: f64,
    offset: f64,
    from_s: f64,
) -> Option<f64> {
    let period = 60.0 / bpm;
    if !period.is_finite() || period <= 0.0 || !from_s.is_finite() {
        return None;
    }
    let near = (offset - from_s).rem_euclid(period);
    attack_phase(mono, rate, period, near, &|_| true, 4).map(|p| {
        let d = (p - near).rem_euclid(period);
        (if d > period / 2.0 { d - period } else { d }) * 1e3
    })
}

/// How a window's beats hold, measured part by part: the window is cut into
/// parts of `part_s` seconds and each part's attack is folded against the
/// grid, so a part too short for the whole-track fit (it wants 16 steady
/// deviations) still counts. A window's figures are what a mix actually
/// lays over; the track-wide ones can be dragged down by parts that never
/// get mixed (measured: a track at mixability 15 % whole-track reads 0.83
/// over the window a mix lands in).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowGrid {
    /// The window's own beat against the grid, ms (median of its parts).
    pub phase_ms: f64,
    /// The parts' worst wander from that line, half the range, ms.
    pub drift_ms: f32,
    /// Scatter of the parts' beats, ms (robust, the same reading as the
    /// whole-track `jitter_ms`).
    pub jitter_ms: f32,
    /// Share of the window's parts with a clear attack, 0…1.
    pub coverage: f32,
    /// 0…1 from those three, the same formula the track uses.
    pub mixability: f32,
    /// Parts measured (fewer than two is nothing to say).
    pub parts: usize,
}

/// Measure a window of audio, part by part (see [`WindowGrid`]). `from_s` is
/// where the window starts in the track — each part is folded against the
/// grid from that origin, so a part knows where it was cut from.
pub fn window_grid(
    mono: &[f32],
    rate: u32,
    bpm: f64,
    offset: f64,
    part_s: f64,
    from_s: f64,
) -> Option<WindowGrid> {
    if !bpm.is_finite() || !(40.0..=300.0).contains(&bpm) || !offset.is_finite() || part_s <= 0.0 {
        return None;
    }
    let step = ((part_s * f64::from(rate)) as usize).max(1);
    let phases: Vec<Option<f64>> = mono
        .chunks(step)
        .enumerate()
        .map(|(j, c)| {
            (c.len() >= rate as usize / 2 && c.len() as f64 >= 60.0 / bpm * 4.0 * f64::from(rate))
                .then(|| part_phase(c, rate, bpm, offset, from_s + j as f64 * part_s))
                .flatten()
        })
        .collect();
    let parts = phases.len();
    let mut have: Vec<f64> = phases.iter().flatten().copied().collect();
    if parts < 2 || have.len() < 2 {
        return None;
    }
    have.sort_by(f64::total_cmp);
    let phase = have[have.len() / 2];
    let drift = ((have[have.len() - 1] - have[0]) / 2.0) as f32;
    let mut dev: Vec<f64> = have.iter().map(|p| (p - phase).abs()).collect();
    let jitter = (median(&mut dev) * 1.4826) as f32;
    let coverage = have.len() as f32 / parts as f32;
    Some(WindowGrid {
        phase_ms: phase,
        drift_ms: drift,
        jitter_ms: jitter,
        coverage,
        mixability: mixability(jitter, drift, coverage),
        parts,
    })
}

/// Where a kick is heard: its envelope halfway from the dip before it to
/// its peak, where its energy (the punch) arrives. At 10 % the first tick
/// of a kick counted: one track's kick has a tick 12 ms before its body,
/// another none, and aligned so their bodies flammed.
const KICK_LEVEL: f64 = 0.5;

/// Where the 10 % crossing sits relative to a kick's true start (the
/// analytic envelope rises a little ahead of a sharp onset), calibrated on
/// synthetic kicks.
const KICK_ONSET_BIAS_S: f64 = 0.0;

/// Tempo and phase of the kick. A kick lands on the same spot of every
/// beat; a bassline's notes (as strong in the low band, 20–45 ms around
/// the kick) and a breakdown's hits don't. So per candidate tempo, the
/// attacks' positions within the beat go into 1 ms slots: the fullest slot
/// (with its neighbours) is the kick, and the tempo that makes it fullest
/// is the track's. Searched within ±1.5 % of `bpm`, then finer; then a
/// least-squares line through the attacks on that slot.
fn kick_grid(onsets: &[(f64, f64)], bpm: f64) -> Option<(f64, f64, f32)> {
    if onsets.len() < 32 {
        return None;
    }
    let mut str_sorted: Vec<f64> = onsets.iter().map(|o| o.1).collect();
    let typical = median(&mut str_sorted).max(1e-12);
    // A strong, sharp jump counts more (the kick, not the most regular
    // small element), capped so one huge hit doesn't decide alone.
    let weight: Vec<f64> = onsets.iter().map(|o| (o.1 / typical).min(10.0)).collect();
    let score = |b: f64, bin_s: f64| -> (f64, f64) {
        let period = 60.0 / b;
        let bins = (period / bin_s).round().max(8.0) as usize;
        let mut h = vec![0.0f64; bins];
        for ((t, _), w) in onsets.iter().zip(&weight) {
            let i = ((t / period).fract() * bins as f64) as usize % bins;
            h[i] += w;
        }
        let (mut best, mut at) = (0.0, 0);
        for i in 0..bins {
            let v = h[(i + bins - 1) % bins] + h[i] + h[(i + 1) % bins];
            if v > best {
                (best, at) = (v, i);
            }
        }
        (best, (at as f64 + 0.5) / bins as f64 * period)
    };
    // Coarse to fine; each stage searches around the one before and keeps
    // its own best (scores of different slot widths don't compare).
    let mut best = (bpm, 0.0f64, 0.0f64);
    for (range, step, bin) in [
        (bpm * 0.015, 0.002, 0.001),
        (0.004, 0.0002, 0.0005),
        (0.0004, 0.00002, 0.0005),
    ] {
        let centre = best.0;
        let mut stage = (centre, f64::MIN, 0.0);
        let mut b = centre - range;
        while b <= centre + range {
            let (v, phase) = score(b, bin);
            if v > stage.1 {
                stage = (b, v, phase);
            }
            b += step;
        }
        best = stage;
    }
    let (mut period, mut offset) = (60.0 / best.0, best.2);
    // The kick is the first thing on the beat: a bass stab or a sub right
    // after it can jump harder in this band (on one track 31 ms after the
    // kick, 3.5× as strong). What makes the kick is being on nearly every
    // beat: of the slots up to a fifth of a beat before the strongest, the
    // earliest with onsets on at least 60 % as many beats (counted, not
    // weighed). One track: the kick, then 41 and 73 ms later two harder
    // low hits.
    {
        let bin = 0.0005;
        let bins = (period / bin).round() as usize;
        let slot = |t: f64| ((t / period).fract() * bins as f64) as usize % bins;
        let mut h = vec![0.0f64; bins];
        for ((t, _), w) in onsets.iter().zip(&weight) {
            h[slot(*t)] += w;
        }
        let sum3 = |v: &Vec<f64>, i: isize| {
            (-1..=1)
                .map(|d| v[(i + d).rem_euclid(bins as isize) as usize])
                .sum::<f64>()
        };
        let guess = (offset / bin).floor() as isize;
        let top_i = (guess - 4..=guess + 4)
            .max_by(|a, b| sum3(&h, *a).total_cmp(&sum3(&h, *b)))
            .unwrap_or(guess);
        // Count only real hits: a fifth of the strongest slot's typical one.
        let near_top = |t: f64| {
            (slot(t) as isize - top_i)
                .rem_euclid(bins as isize)
                .min((top_i - slot(t) as isize).rem_euclid(bins as isize))
                <= 1
        };
        let mut top_strengths: Vec<f64> = onsets
            .iter()
            .filter(|(t, _)| near_top(*t))
            .map(|o| o.1)
            .collect();
        let floor = median(&mut top_strengths) * 0.2;
        let mut n = vec![0.0f64; bins];
        for (t, v) in onsets {
            if *v >= floor {
                n[slot(*t)] += 1.0;
            }
        }
        let top_count = sum3(&n, top_i);
        // Up to a fifth of a beat back: short of a 16th note (a quarter),
        // far short of the off-beat.
        let reach = (period * 0.2 / bin) as isize;
        let local_peak =
            |i: isize| sum3(&n, i) >= sum3(&n, i - 1) && sum3(&n, i) >= sum3(&n, i + 1);
        if std::env::var_os("PATCH_GRID_DEBUG").is_some() {
            let peaks: Vec<String> = (top_i - reach..=top_i)
                .filter(|&i| sum3(&n, i) >= top_count * 0.3 && local_peak(i))
                .map(|i| {
                    format!(
                        "{:+.1}ms:{:.0}%",
                        (i - top_i) as f64 * bin * 1e3,
                        sum3(&n, i) / top_count * 100.0
                    )
                })
                .collect();
            eprintln!("slots before the strongest (by count): {peaks:?}");
        }
        let early = (top_i - reach..top_i)
            .find(|&i| sum3(&n, i) >= top_count * 0.6 && local_peak(i))
            .unwrap_or(top_i);
        offset = ((early as f64 + 0.5) * bin).rem_euclid(period);
        if std::env::var_os("PATCH_GRID_DEBUG").is_some() {
            eprintln!(
                "strongest slot at {:.4} s, kick slot {:+.1} ms: offset {offset:.4}",
                (top_i as f64 + 0.5) * bin,
                (early - top_i) as f64 * bin * 1e3
            );
        }
    }
    // The line through the attacks on the kick's slot, tightening.
    for tol in [0.006, 0.003, 0.002] {
        let on: Vec<(f64, f64)> = onsets
            .iter()
            .filter_map(|(t, _)| {
                let k = ((t - offset) / period).round();
                ((t - (offset + k * period)).abs() < tol).then_some((k, *t))
            })
            .collect();
        let Some((slope, icpt)) = fit_line(&on) else {
            break;
        };
        if (slope * best.0 / 60.0 - 1.0).abs() > 0.001 {
            break;
        }
        (period, offset) = (slope, icpt.rem_euclid(slope));
        if std::env::var_os("PATCH_GRID_DEBUG").is_some() {
            eprintln!(
                "  line through {} attacks within {:.0} ms: offset {offset:.4}",
                on.len(),
                tol * 1e3
            );
        }
    }
    // How sure the phase is: the kick's slot against the fullest one
    // elsewhere in the beat (60 ms or more away). A bassline on the
    // off-beat as regular as the kick: unsure.
    let phase_confidence = {
        let bin = 0.0005;
        let bins = (period / bin).round() as usize;
        let mut h = vec![0.0f64; bins];
        for ((t, _), w) in onsets.iter().zip(&weight) {
            h[((t / period).fract() * bins as f64) as usize % bins] += w;
        }
        let score = |i: isize| {
            (-1..=1)
                .map(|d| h[(i + d).rem_euclid(bins as isize) as usize])
                .sum::<f64>()
        };
        let at = (offset / bin).floor() as isize;
        let kick = (at - 4..=at + 4).map(score).fold(0.0, f64::max);
        // Rivals at least 60 ms away (what follows a kick within that, a
        // clap or a bass stab, belongs to it).
        let near = (0.060 / bin) as isize;
        let away = (near..bins as isize - near)
            .map(|d| score(at + d))
            .fold(0.0, f64::max);
        if kick > 0.0 {
            (1.0 - away / kick).clamp(0.0, 1.0) as f32
        } else {
            0.0
        }
    };
    Some((60.0 / period, offset, phase_confidence))
}

/// Measure the grid: kicks found without a grid, the tempo they agree on,
/// then per beat how each kick sits. `precise` tightens the tolerances
/// (use it on the full-rate audio). `bpm` is the tempo hint (the analysis'
/// detection, which settles the octave).
fn measure_at(mono: &[f32], rate: u32, bpm: f64, precise: bool) -> Option<GridQuality> {
    measure_from(mono, rate, bpm, precise, None)
}

/// [`measure_at`], the kicks looked for from `start` (seconds, and how
/// clear the kick was there) instead of where the onsets put the first
/// beat: the kick's phase is decided, only measured again.
fn measure_from(
    mono: &[f32],
    rate: u32,
    bpm: f64,
    precise: bool,
    start: Option<(f64, f32)>,
) -> Option<GridQuality> {
    let (onsets, energy, env_rate) = kick_onsets(mono, rate);
    let dbg = std::env::var_os("PATCH_GRID_DEBUG").is_some();
    if dbg {
        let mut v: Vec<f64> = onsets.iter().map(|o| o.1).collect();
        v.sort_by(f64::total_cmp);
        eprintln!(
            "onsets {} (strength p50 {:.4} p90 {:.4}) first {:?}",
            onsets.len(),
            v.get(v.len() / 2).unwrap_or(&0.0),
            v.get(v.len() * 9 / 10).unwrap_or(&0.0),
            onsets
                .iter()
                .take(6)
                .map(|o| (format!("{:.4}", o.0), format!("{:.3}", o.1)))
                .collect::<Vec<_>>()
        );
    }
    let (bpm0, offset0, phase_confidence) = kick_grid(&onsets, bpm)?;
    let (offset0, phase_confidence) = match start {
        Some((at, clear)) => (at, phase_confidence.max(clear)),
        None => (offset0, phase_confidence),
    };
    if dbg {
        for k in [1usize, 2, 3] {
            let t = if std::env::var_os("PATCH_GRID_AT").is_some() {
                0.37 + k as f64 * 60.0 / bpm0
            } else {
                offset0 + k as f64 * 60.0 / bpm0
            };
            eprintln!(
                "beat {k} at {t:.4}: {:?}",
                onsets
                    .iter()
                    .filter(|o| (o.0 - t).abs() < 0.03)
                    .map(|o| (format!("{:+.2}ms", (o.0 - t) * 1e3), format!("{:.4}", o.1)))
                    .collect::<Vec<_>>()
            );
        }
        eprintln!("kick grid {bpm0:.4} BPM, first beat {offset0:.4} s");
    }
    let period = 60.0 / bpm0;
    let dur = mono.len() as f64 / rate as f64;
    let beats = ((dur - offset0) / period).floor().max(0.0) as usize;
    // Per beat: the attack nearest the kick grid, within 10 % of a beat
    // (not the strongest: that's often the bassline).
    let mut hits = vec![];
    let mut j = 0;
    for k in 0..beats {
        let at = offset0 + k as f64 * period;
        while j < onsets.len() && onsets[j].0 < at - period * 0.1 {
            j += 1;
        }
        let near = onsets[j..]
            .iter()
            .take_while(|o| o.0 <= at + period * 0.1)
            .min_by(|a, b| (a.0 - at).abs().total_cmp(&(b.0 - at).abs()));
        if let Some(&(t, v)) = near {
            hits.push(Hit {
                k: k as f64,
                t,
                strength: v,
            });
        }
    }
    if let Some(f) = std::env::var_os("PATCH_GRID_HITS") {
        let _ = std::fs::write(
            f,
            hits.iter()
                .map(|h| {
                    format!(
                        "{} {:.3} {:.4}\n",
                        h.k,
                        (h.t - (offset0 + h.k * period)) * 1e3,
                        h.strength
                    )
                })
                .collect::<String>(),
        );
    }
    // Several low elements taking turns can leave no steady stretch to
    // measure: then the kick grid as found, marked unsure (the AI keeps to
    // cuts and short layers with it) rather than no grid at all.
    let fit =
        straight_fit(&hits, beats, period, if precise { 0.0015 } else { 0.003 }).unwrap_or(Fit {
            period,
            first: offset0,
            jitter_ms: 8.0,
            drift_ms: 20.0,
            coverage: 0.3,
            gaps: vec![],
        });

    let bpm = 60.0 / fit.period;
    // (A last "punch alignment" step moved grids onto a bass stab right
    // after the kick, +15 ms on one track: reported off beat. Gone: the
    // kick's own energy crossing decides.)
    let offset = fit.first.rem_euclid(fit.period);
    // Single onsets of a soft kick scatter (heard: tens of ms off on one
    // track); the kick averaged over every beat doesn't. Its rise decides.
    let clicks = click_rise(mono, rate);
    // The phase itself, checked over the whole beat: where an attack and
    // the low end's rise meet, averaged over every beat (single onsets put
    // one grid 107 ms before its kick, on a bass note).
    let (mut moved, clear) = kick_phase(&energy, env_rate, &clicks, offset, fit.period);
    // The onsets' grid was on the off-beat (an open hat louder than the
    // kick): the fit above measured the hats and found no steady line, so a
    // straight club track was marked unsure (290 of 2350 tracks, every
    // AI mix with them a cut or a short layer). Measure again on the kick.
    if start.is_none()
        && let Some(m) = moved
        && m.abs() > fit.period * 0.25
        && let Some(q) = measure_from(
            mono,
            rate,
            bpm,
            precise,
            Some(((offset + m).rem_euclid(fit.period), clear)),
        )
        && q.coverage > fit.coverage
    {
        return Some(q);
    }
    // Measured again from the kick: a second half-beat move would put it
    // back on the element the first pass rejected.
    if start.is_some() && moved.is_some_and(|m| m.abs() > fit.period * 0.25) {
        moved = None;
    }
    let offset = match moved {
        Some(moved) => {
            if dbg {
                eprintln!("kick phase: moved {:+.1} ms", moved * 1e3);
            }
            (offset + moved).rem_euclid(fit.period)
        }
        None => offset,
    };
    let shift =
        kick_template(&energy, env_rate, &clicks, offset, fit.period).unwrap_or(KICK_ATTACK_LEAD_S);
    if dbg {
        eprintln!("kick template: {:+.1} ms", shift * 1e3);
    }
    let offset = (offset + shift).rem_euclid(fit.period);
    if std::env::var_os("PATCH_GRID_DEBUG").is_some() {
        // The track's own kicks against its grid: where most of them sit.
        let res: Vec<f64> = onsets
            .iter()
            .map(|(t, _)| {
                let x = (t - offset) / fit.period;
                (x - x.round()) * fit.period * 1e3
            })
            .collect();
        let best = res
            .iter()
            .map(|c| (*c, res.iter().filter(|x| (*x - c).abs() <= 2.0).count()))
            .max_by_key(|x| x.1);
        let second = best.and_then(|(b, _)| {
            res.iter()
                .filter(|x| (*x - b).abs() > 6.0)
                .map(|c| (*c, res.iter().filter(|x| (*x - c).abs() <= 2.0).count()))
                .max_by_key(|x| x.1)
        });
        eprintln!(
            "kicks vs grid: densest at {:+.1} ms ({} kicks), next at {:+.1} ms ({})",
            best.map_or(0.0, |b| b.0),
            best.map_or(0, |b| b.1),
            second.map_or(0.0, |b| b.0),
            second.map_or(0, |b| b.1)
        );
    }
    // Low-band energy per beat: the structure's downbeat vote.
    let r = env_rate;
    let per_beat: Vec<f64> = (0..beats)
        .map(|k| {
            let a = ((offset + k as f64 * fit.period) * r) as usize;
            let b = (((offset + (k + 1) as f64 * fit.period) * r) as usize).min(energy.len());
            energy[a.min(b)..b].iter().map(|x| *x as f64).sum::<f64>()
        })
        .collect();
    let mut q = finish(mono, rate, bpm, offset, fit, precise, &per_beat);
    // Sure of the phase when either says so: the onsets' count (no rival
    // as regular), or the kick's own sound (no rival kick-like element).
    let phase_confidence = phase_confidence.max(clear);
    q.phase_confidence = phase_confidence;
    // An unsure phase: the kick might be the element half a beat away. A
    // long blend on the wrong one is a trainwreck, so it counts against
    // mixability (short layers or a cut instead).
    // (A rival at half the kick's strength is harmless: tech house's
    // off-beat bass. Measured: the tracks whose grid sat half a beat off
    // scored 0-21 %.)
    q.mixability *= (phase_confidence / 0.35).clamp(0.3, 1.0);
    Some(q)
}

/// Where in the beat the kick is, folded over every beat: the phase where
/// a high-band attack and a rise of the low band meet (a kick has both; an
/// off-beat hat no low rise, a bass note no attack). The shift from
/// `offset` (seconds) when that phase is clearly elsewhere, else None.
fn kick_phase(
    energy: &[f32],
    r: f64,
    clicks: &Clicks,
    offset: f64,
    period: f64,
) -> (Option<f64>, f32) {
    const BINS: usize = 400; // ~1.2 ms at 128 BPM
    let beats = ((energy.len() as f64 / r - offset) / period).floor() as i64;
    let rise_s = 0.004;
    let mut low = vec![0.0f64; BINS];
    let mut high = vec![0.0f64; BINS];
    let mut used = 0;
    for k in 0..beats.max(0) {
        let t0 = offset + k as f64 * period;
        // Per beat, each band normalised by its own largest rise: every
        // section counts alike.
        let mut l = [0.0f64; BINS];
        let mut h = [0.0f64; BINS];
        for b in 0..BINS {
            let t = t0 + period * b as f64 / BINS as f64;
            let (i, j) = ((t * r) as usize, ((t + rise_s) * r) as usize);
            let (Some(a), Some(z)) = (energy.get(i), energy.get(j)) else {
                continue;
            };
            l[b] = (*z - *a).max(0.0) as f64;
            h[b] = clicks.near(t + 0.002);
        }
        let (lm, hm) = (
            l.iter().cloned().fold(0.0, f64::max),
            h.iter().cloned().fold(0.0, f64::max),
        );
        if lm <= 0.0 || hm <= 0.0 {
            continue;
        }
        for b in 0..BINS {
            low[b] += l[b] / lm;
            high[b] += h[b] / hm;
        }
        used += 1;
    }
    if used < 32 {
        return (None, 0.0);
    }
    // The low end may rise up to 15 ms after the attack (a soft body).
    let reach = (0.015 / period * BINS as f64).ceil() as usize;
    let score: Vec<f64> = (0..BINS)
        .map(|b| high[b] * (0..=reach).map(|d| low[(b + d) % BINS]).fold(0.0, f64::max))
        .collect();
    let (best, top) =
        score.iter().enumerate().fold(
            (0, f64::MIN),
            |m, (i, v)| if *v > m.1 { (i, *v) } else { m },
        );
    // The grid's own phase: its best within ±25 ms (the template refines).
    let near = (0.025 / period * BINS as f64).ceil() as i64;
    let here = (-near..=near)
        .map(|d| score[d.rem_euclid(BINS as i64) as usize])
        .fold(0.0, f64::max);
    let d = best as f64 / BINS as f64;
    let shift = if d > 0.5 { d - 1.0 } else { d } * period;
    if std::env::var_os("PATCH_GRID_DEBUG").is_some() {
        eprintln!(
            "kick phase: best {:+.1} ms ({:.2}× the grid's own)",
            shift * 1e3,
            top / here.max(1e-12)
        );
    }
    let moved = (shift.abs() > 0.025 && top > here * 1.5).then_some(shift);
    // How clearly the kick stands where the grid ends up: its kick-like
    // score (an attack with a low rise) against the best one 60 ms or more
    // away. An off-beat bass or hat has one of the two, not both: unlike
    // the onset count (as regular off the beat as on it in tech house),
    // this doesn't mistake them for a rival kick.
    let at = if moved.is_some() { best as i64 } else { 0 };
    let own = (-near..=near)
        .map(|d| score[(at + d).rem_euclid(BINS as i64) as usize])
        .fold(0.0, f64::max);
    let gap = (0.060 / period * BINS as f64).ceil() as i64;
    let rival = (gap..BINS as i64 - gap)
        .map(|d| score[(at + d).rem_euclid(BINS as i64) as usize])
        .fold(0.0, f64::max);
    let clear = if own > 0.0 {
        (1.0 - rival / own).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    if std::env::var_os("PATCH_GRID_DEBUG").is_some() {
        eprintln!(
            "kick phase: clear {:.0} % (rival {:.2}× the kick)",
            clear * 100.0,
            rival / own.max(1e-12)
        );
    }
    (moved, clear)
}

/// Where the average kick rises, relative to the grid (seconds): the low
/// band's energy folded over every beat that has a kick, crossing the same
/// level single onsets use. None when there is no clear rise near the grid.
fn kick_template(energy: &[f32], r: f64, clicks: &Clicks, offset: f64, period: f64) -> Option<f64> {
    let ms = |v: f64| (v * 0.001 * r).round() as i64;
    let (before, after) = (ms(60.0), ms(80.0));
    let width = (before + after + 1) as usize;
    let beats = ((energy.len() as f64 / r - offset) / period).floor() as i64;
    // Per beat: its window and how much its low end jumps (a breakdown's
    // beats barely do: left out).
    let mut rows: Vec<(usize, f64)> = vec![];
    for k in 1..beats.max(1) - 1 {
        let c = ((offset + k as f64 * period) * r).round() as i64;
        let (a, b) = (c - before, c + after);
        if a < 0 || b as usize >= energy.len() {
            continue;
        }
        let w = &energy[a as usize..=b as usize];
        let pre = w[..before as usize]
            .iter()
            .cloned()
            .fold(f32::MAX, f32::min) as f64;
        let peak = w[before as usize..].iter().cloned().fold(0.0, f32::max) as f64;
        rows.push((a as usize, peak - pre));
    }
    let mut jumps: Vec<f64> = rows.iter().map(|r| r.1).collect();
    jumps.sort_by(f64::total_cmp);
    let typical = *jumps.get(jumps.len() * 3 / 4)?;
    if typical <= 0.0 {
        return None;
    }
    let mut t = vec![0.0f64; width];
    let mut used = 0;
    for &(a, jump) in &rows {
        if jump >= typical * 0.4 {
            // Each beat by its own size: loud and quiet sections alike.
            for (i, v) in energy[a..a + width].iter().enumerate() {
                t[i] += *v as f64 / jump;
            }
            used += 1;
        }
    }
    if used < 16 {
        return None;
    }
    let zero = before as usize;
    let near = ms(25.0) as usize;
    let (peak_at, peak) = t[zero - near..]
        .iter()
        .enumerate()
        .take(near + ms(60.0) as usize)
        .fold((0, f64::MIN), |m, (i, v)| {
            if *v > m.1 { (i + zero - near, *v) } else { m }
        });
    let dip = t[..peak_at].iter().cloned().fold(f64::MAX, f64::min);
    // The body's rise; none where a sidechained bass is louder than the
    // kick (its low end dips at the beat): then the attack is looked for
    // around the grid itself.
    let body = || {
        if peak <= dip * 1.2 {
            return None;
        }
        let level = dip + (peak - dip) * KICK_LEVEL;
        let j = (1..=peak_at).rev().find(|&j| t[j - 1] < level)?;
        let frac = (level - t[j - 1]) / (t[j] - t[j - 1]);
        Some((j as f64 - 1.0 + frac - zero as f64) / r)
    };
    let low = body().unwrap_or(0.0);
    // What a listener times a kick by is its attack: the click at its
    // start. A soft kick's body swells long after it (heard: one track's
    // click 34 ms before its body, the mix audibly flammed). The strongest
    // high-band attack from 50 ms before the body's rise to 10 ms after it,
    // on the same beats.
    let click_at = {
        let cr = clicks.per_s;
        let (from, to) = (low - 0.050, low + 0.010);
        let n = ((to - from) * cr).round() as usize + 1;
        let mut ct = vec![0.0f64; n];
        let mut used = 0;
        for &(a, jump) in &rows {
            if jump < typical * 0.4 {
                continue;
            }
            let beat = (a as i64 + before) as f64 / r;
            let s = ((beat + from) * cr).round() as usize;
            let Some(w) = clicks.rise.get(s..s + n) else {
                continue;
            };
            let top = w.iter().cloned().fold(0.0, f32::max) as f64;
            if top > 0.0 {
                for (i, v) in w.iter().enumerate() {
                    ct[i] += *v as f64 / top;
                }
                used += 1;
            }
        }
        let mut sorted = ct.clone();
        sorted.sort_by(f64::total_cmp);
        let median = sorted[n / 2];
        let (at, top) =
            ct.iter().enumerate().fold(
                (0, f64::MIN),
                |m, (i, v)| if *v > m.1 { (i, *v) } else { m },
            );
        if std::env::var_os("PATCH_GRID_DEBUG").is_some() {
            eprintln!(
                "kick body rises at {:+.1} ms, strongest attack at {:+.1} ms ({:.1}× the window's median, {used} beats)",
                low * 1e3,
                (from + at as f64 / cr) * 1e3,
                top / median.max(1e-12)
            );
        }
        // The rise is measured over 2 ms: its peak is ~1 ms into the click.
        // Clear: 80 % of tech house tracks, 8× up to 200×. Below that the
        // window's strongest attack scattered (hats, a faint body).
        (used >= 16 && top > median * 8.0).then(|| from + at as f64 / cr - 0.001)
    };
    Some(click_at.unwrap_or(KICK_ATTACK_LEAD_S))
}

/// Where a clear kick attack sits relative to the single-onset kick grid:
/// the median over two collections of 100 tech house tracks (−10.1 and
/// −8.5 ms; 80 % of each has a clear attack). Kicks without one get this,
/// so all grids mean the same moment.
const KICK_ATTACK_LEAD_S: f64 = -0.010;

/// The standard measurement, on the analysis' own half-rate mono. The
/// offset hint is not needed: the kicks decide the phase.
pub fn measure(mono: &[f32], rate: u32, bpm: f64, _offset: f64) -> Option<GridQuality> {
    measure_at(mono, rate, bpm, false)
}

/// Which beat of the bar (counted from `offset`) the track's sections start
/// on, when that is clear. Club tracks change section (drops, breaks, new
/// loops) on bar 1: the 12 strongest changes of four-beat energy, one per
/// neighbourhood, vote for a beat of the bar; clear when the winner has
/// three votes more than the runner-up. `patch-bench --downbeat-check`:
/// where clear, it agreed with the grid on 56 of 58 tracks whose downbeat
/// was measured confidently (≥ 0.75).
fn section_phase(mono: &[f32], rate: u32, beat_s: f64, offset: f64) -> Option<u8> {
    let r = f64::from(rate);
    let beats = ((mono.len() as f64 / r - offset) / beat_s).floor().max(0.0) as usize;
    if beats < 64 {
        return None;
    }
    let e: Vec<f64> = (0..beats)
        .map(|k| {
            let i0 = (((offset + k as f64 * beat_s) * r).max(0.0) as usize).min(mono.len());
            let i1 = (((offset + (k + 1) as f64 * beat_s) * r) as usize).min(mono.len());
            let seg = &mono[i0..i1.max(i0)];
            (seg.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / seg.len().max(1) as f64 + 1e-9).ln()
        })
        .collect();
    let mut changes: Vec<(f64, usize)> = (4..beats - 4)
        .map(|k| ((e[k..k + 4].iter().sum::<f64>() - e[k - 4..k].iter().sum::<f64>()).abs(), k))
        .collect();
    changes.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut picked: Vec<usize> = Vec::with_capacity(12);
    for (_, k) in changes {
        if picked.iter().all(|p| p.abs_diff(k) > 4) {
            picked.push(k);
            if picked.len() == 12 {
                break;
            }
        }
    }
    if picked.len() < 8 {
        return None;
    }
    let mut votes = [0usize; 4];
    for k in &picked {
        votes[k % 4] += 1;
    }
    let best = (0..4).max_by_key(|p| votes[*p])?;
    let mut sorted = votes;
    sorted.sort_unstable();
    (sorted[3] >= sorted[2] + 3).then_some(best as u8)
}

/// Downbeat and the rest, shared by both passes.
fn finish(
    mono: &[f32],
    rate: u32,
    bpm: f64,
    offset: f64,
    fit: Fit,
    precise: bool,
    per_beat: &[f64],
) -> GridQuality {
    let db = downbeat(mono, rate, bpm, offset, per_beat);
    let beat_s = 60.0 / bpm;
    // The beat goes where its kick is heard: the click attack, when the kick
    // has a clear one near its body. Two tracks' clicks are what the ear
    // lines up (blind listening and rehearsal flam agree); a kick without a
    // clear click keeps the body. Measured on 100 rehearsed transitions:
    // mean flam 6.8 → 4.9 ms, flam rejections 13 → 6.
    let click_offset_ms = attack_phase(mono, rate, beat_s, offset, &|_| true, 8).map(|at| {
        let d = (at - offset).rem_euclid(beat_s);
        ((if d > beat_s / 2.0 { d - beat_s } else { d }) * 1e3) as f32
    });
    let offset = offset
        + click_offset_ms
            .filter(|ms| ms.abs() <= MAX_CLICK_SHIFT_MS)
            .map_or(0.0, |ms| f64::from(ms) / 1e3);
    // The track's own sections vote on the bar: where they are clear, they
    // confirm the downbeat, or replace one the kicks measured weakly.
    let db = match (db, section_phase(mono, rate, beat_s, offset)) {
        (Some((d, c)), Some(p)) if d % 4 == p => Some((d, c.max(0.85))),
        (Some((_, c)), Some(p)) if c < 0.75 => Some((p, 0.8)),
        (None, Some(p)) => Some((p, 0.8)),
        (db, _) => db,
    };
    GridQuality {
        method: METHOD,
        precise,
        bpm,
        offset,
        downbeat: db.map(|d| d.0),
        downbeat_confidence: db.map(|d| d.1).unwrap_or(0.0),
        jitter_ms: fit.jitter_ms,
        drift_ms: fit.drift_ms,
        coverage: fit.coverage.min(1.0),
        beatless: fit
            .gaps
            .iter()
            .map(|(a, b)| {
                [
                    (offset + *a as f64 * beat_s) as f32,
                    (offset + *b as f64 * beat_s) as f32,
                ]
            })
            .collect(),
        mixability: mixability(fit.jitter_ms, fit.drift_ms, fit.coverage),
        phase_confidence: 1.0,
        click_offset_ms,
    }
}

/// Per beat, the energy of three bands (low < 150 Hz, mid 150 Hz–2 kHz,
/// high > 4 kHz), log scale.
fn band_beats(mono: &[f32], rate: u32, bpm: f64, offset: f64) -> Vec<[f64; 3]> {
    let r = rate as f64;
    let pole = |hz: f64| (-std::f64::consts::TAU * hz / r).exp();
    let (a150, a2k, a4k) = (pole(150.0), pole(2000.0), pole(4000.0));
    let (mut l1, mut l2, mut m, mut h) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let beat = 60.0 / bpm * r;
    let mut out = vec![];
    let mut acc = [0.0f64; 3];
    let mut next = offset * r + beat;
    let start = (offset * r).max(0.0) as usize;
    for (i, &x) in mono.iter().enumerate() {
        let x = x as f64;
        l1 = (1.0 - a150) * x + a150 * l1;
        l2 = (1.0 - a150) * l1 + a150 * l2;
        m = (1.0 - a2k) * x + a2k * m;
        h = (1.0 - a4k) * x + a4k * h;
        if i < start {
            continue;
        }
        let (low, mid, high) = (l2, m - l2, x - h);
        acc[0] += low * low;
        acc[1] += mid * mid;
        acc[2] += high * high;
        if i as f64 >= next {
            out.push(acc.map(|e| (e + 1e-9).ln()));
            acc = [0.0; 3];
            next += beat;
        }
    }
    out
}

/// Downbeat votes from the arrangement: elements enter, leave and change
/// on the first beat of a bar (and most of all on a phrase). Per beat, how
/// much each band changes from the bar before to the bar after; the
/// clearest changes vote for their position within the bar.
fn votes_from_changes(bands: &[[f64; 3]]) -> [f64; 4] {
    let n = bands.len();
    let mut votes = [0.0f64; 4];
    if n < 24 {
        return votes;
    }
    let mean = |a: usize, b: usize, band: usize| {
        bands[a..b].iter().map(|v| v[band]).sum::<f64>() / (b - a) as f64
    };
    let change: Vec<f64> = (0..n)
        .map(|k| {
            if k < 4 || k + 4 > n {
                0.0
            } else {
                (0..3)
                    .map(|b| (mean(k, k + 4, b) - mean(k - 4, k, b)).abs())
                    .sum()
            }
        })
        .collect();
    let mut sorted = change.clone();
    sorted.sort_by(f64::total_cmp);
    let typical = sorted[n / 2];
    for k in 4..n.saturating_sub(4) {
        let c = change[k];
        // A clear change, and the peak of its neighbourhood (the bars
        // around a change blur into it).
        if c > typical * 2.0 && (k.saturating_sub(2)..(k + 3).min(n)).all(|j| change[j] <= c) {
            votes[k % 4] += c - typical;
        }
    }
    votes
}

/// Which grid beat (0…3) starts a bar, and how sure: the arrangement's
/// changes, the bass coming back, and the heavier kick on the one, each
/// voting (weighted by how much it can be trusted).
fn downbeat(mono: &[f32], rate: u32, bpm: f64, offset: f64, per_beat: &[f64]) -> Option<(u8, f32)> {
    let unit = |v: [f64; 4]| {
        let s: f64 = v.iter().sum();
        if s > 0.0 { v.map(|x| x / s) } else { [0.0; 4] }
    };
    let mut total = [0.0f64; 4];
    let changes = unit(votes_from_changes(&band_beats(mono, rate, bpm, offset)));
    let rises = downbeat_from_rises(per_beat).map(|(d, c)| {
        let mut v = [0.0; 4];
        v[d as usize] = c as f64;
        v
    });
    let kick = tempo::downbeat(
        mono,
        rate,
        &tempo::Tempo {
            bpm,
            offset,
            confidence: 1.0,
        },
    )
    .map(|(d, c)| {
        let mut v = [0.0; 4];
        v[d as usize] = c as f64;
        v
    });
    // Measured on 100 tech house tracks: the arrangement's changes and the
    // bass returns agree with each other on 82; the heavier kick matches
    // either only half the time (barely better than chance) and where the
    // kick comes in isn't consistent within a track (a filtered or quiet
    // first kick is found a beat late). So the two structural cues lead.
    for (v, w) in [(Some(changes), 1.0), (rises, 0.8), (kick, 0.15)] {
        if let Some(v) = v {
            for i in 0..4 {
                total[i] += v[i] * w;
            }
        }
    }
    if std::env::var_os("PATCH_GRID_DEBUG").is_some() {
        let f = |v: [f64; 4]| v.map(|x| (x * 100.0).round() as i64);
        eprintln!(
            "downbeat votes: changes {:?} rises {:?} kick {:?}",
            f(changes),
            rises.map(f),
            kick.map(f)
        );
    }
    let sum: f64 = total.iter().sum();
    if sum <= 0.0 {
        return None;
    }
    let (best, top) = total.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
    let second = total
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != best)
        .map(|(_, v)| *v)
        .fold(0.0, f64::max);
    Some((best as u8, ((top - second) / top).clamp(0.0, 1.0) as f32))
}

/// The precise pass: the same at the full rate (44.1 kHz), tighter.
pub fn precise(mono: &[f32], rate: u32, coarse: &GridQuality) -> Option<GridQuality> {
    measure_at(mono, rate, coarse.bpm, true)
}

#[cfg(test)]
#[path = "grid_tests.rs"]
mod tests;
