//! Tempo and beat grid.
//!
//! 1. Onset envelope: log-compressed STFT magnitudes (1024-sample Hann,
//!    hop 256 at 22.05 kHz → 86 frames/s), positive spectral flux, minus a
//!    0.5 s moving average, half-wave rectified.
//! 2. Tempo: autocorrelation of the envelope, scored as a comb over the
//!    first four multiples of the beat period (so the bar and half-bar
//!    reinforce the beat), times a weak broad prior around 120 BPM;
//!    searched 60–200 BPM in 0.05 steps.
//! 3. Folded into the DJ range 70–180 BPM.
//! 4. Grid phase: the offset in [0, period) whose comb collects the most
//!    onset energy over the whole track.
//! 5. Grid fit: the onset peak nearest each predicted beat (sub-frame), a
//!    least-squares line through them, outliers dropped: its slope is the
//!    tempo (hundredths of a BPM over a track), its intercept the grid.

use crate::stft;
use serde::{Deserialize, Serialize};

pub const WIN: usize = 1024;
/// The low band's onset envelope peaks this long after a kick's attack
/// starts (its 46 ms frames need the kick well inside them): kick grids
/// add it, so every grid puts the beat on the attack.
pub const KICK_ONSET_LAG_S: f64 = 0.014;
pub const HOP: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tempo {
    pub bpm: f64,
    /// First beat of the grid, seconds.
    pub offset: f64,
    /// Steadiness of the pulse (0…1): score-curve concentration. A raw
    /// score, not a calibrated probability; ≥ 0.5 reads as a steady beat.
    pub confidence: f32,
}

pub struct TempoScan {
    pub selected: Option<Tempo>,
    pub raw_scores: Vec<(f64, f64)>,
}

/// A competing tempo interpretation. `raw_score` is the detector's score,
/// not a probability and must not be displayed or consumed as calibrated
/// confidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TempoHypothesis {
    pub bpm: f64,
    pub beat_offset: f64,
    pub raw_score: f32,
    /// Competing bar origins for this tempo; these are measured scores, not
    /// calibrated probabilities.
    #[serde(default)]
    pub bar_phases: Vec<BarPhaseHypothesis>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BarPhaseHypothesis {
    pub downbeat: u8,
    pub raw_score: f32,
}

/// One event on the selected beat sequence. Beat count is always derived
/// from this sequence. The onset support is a raw local signal ratio, not a
/// calibrated probability that this is a beat.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeatEvent {
    /// Timestamp on the selected global beat grid.
    pub time_s: f64,
    pub beat_index: i64,
    pub interval_s: f32,
    pub bar_index: Option<i64>,
    pub beat_in_bar: Option<u8>,
    /// Nearest observed onset within the local beat window, relative to the
    /// grid timestamp. `None` means the signal did not support a local hit.
    pub onset_offset_s: Option<f32>,
    pub onset_support: f32,
}

/// Local rhythm measurements over a 16-beat window. These are observables,
/// not calibrated probabilities, so planning can distinguish a weak passage
/// from a reliable region elsewhere on the same track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RhythmRegion {
    pub start_s: f64,
    pub end_s: f64,
    pub beat_count: u16,
    pub observed_onset_coverage: f32,
    pub mean_onset_support: f32,
    pub onset_jitter_ms: f32,
    pub phase_spread_ms: f32,
    /// Regional slope fitted to supported beat onsets. Kept only on tracks
    /// whose fitted regions show a real tempo change (see `variable_tempo`);
    /// on constant-tempo tracks it is `None` and the global grid rules.
    #[serde(default)]
    pub local_bpm: Option<f32>,
    /// RMS onset residual around the regional slope; a raw fit diagnostic.
    #[serde(default)]
    pub tempo_fit_residual_ms: Option<f32>,
}

/// Versioned Level 1 rhythm evidence. Alternative hypotheses are retained
/// so later planning can account for half/double-time and bar-phase ambiguity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RhythmEvidence {
    pub analyzer_version: u32,
    /// Stored compactly (see `compact_beats`): a few KB per track.
    #[serde(with = "compact_beats")]
    pub beats: Vec<BeatEvent>,
    pub regions: Vec<RhythmRegion>,
    pub tempo_hypotheses: Vec<TempoHypothesis>,
    /// Candidate quarter-note meters inferred from the selected beat
    /// sequence. Scores are uncalibrated pattern-fit evidence.
    #[serde(default)]
    pub meter_hypotheses: Vec<MeterHypothesis>,
    /// How decisively the selected tempo beat its strongest independent
    /// rival on the score curve (0…1). Raw ambiguity evidence for
    /// evaluation and planning, never a gate on the grid's steadiness.
    #[serde(default)]
    pub selection_strength: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeterHypothesis {
    pub beats_per_bar: u8,
    pub beat_unit: u8,
    /// Beat-index remainder assigned to the bar's first beat.
    pub phase: u8,
    /// Raw repeating-accent fit plus downbeat contrast; not a probability.
    pub raw_score: f32,
    pub supporting_beats: u16,
}

pub const RHYTHM_EVIDENCE_VERSION: u32 = 10;

/// Retain plausible quarter-note meter/phase interpretations without
/// changing the legacy 4/4 fields used by current DJ planning.
fn meter_hypotheses(beats: &[BeatEvent]) -> Vec<MeterHypothesis> {
    let observed = beats
        .iter()
        .filter(|beat| beat.onset_support.is_finite())
        .map(|beat| (beat.beat_index, beat.onset_support.max(0.0) as f64))
        .collect::<Vec<_>>();
    let mut candidates = Vec::new();
    for meter in 2u8..=7 {
        if observed.len() < usize::from(meter) * 2 {
            continue;
        }
        for phase in 0..meter {
            let mut positions = vec![Vec::<f64>::new(); usize::from(meter)];
            for (index, support) in &observed {
                let position = (*index - i64::from(phase)).rem_euclid(i64::from(meter)) as usize;
                positions[position].push(*support);
            }
            if positions.iter().any(|position| position.len() < 2) {
                continue;
            }
            let means = positions
                .iter()
                .map(|position| position.iter().sum::<f64>() / position.len() as f64)
                .collect::<Vec<_>>();
            let global =
                observed.iter().map(|(_, value)| *value).sum::<f64>() / observed.len() as f64;
            let total_variance = observed
                .iter()
                .map(|(_, value)| (value - global).powi(2))
                .sum::<f64>();
            let residual = positions
                .iter()
                .zip(&means)
                .map(|(values, mean)| {
                    values
                        .iter()
                        .map(|value| (value - mean).powi(2))
                        .sum::<f64>()
                })
                .sum::<f64>();
            let repeating_fit = if total_variance > 1e-12 {
                (1.0 - residual / total_variance).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let other_mean = means.iter().skip(1).sum::<f64>() / f64::from(meter - 1);
            let downbeat_contrast = if global > 1e-9 {
                ((means[0] - other_mean) / global).clamp(-1.0, 1.0)
            } else {
                0.0
            };
            // A small shorter-cycle preference breaks exact multiples while
            // retaining them as explicit alternatives in the result.
            let score = repeating_fit + 0.2 * downbeat_contrast - 0.01 * f64::from(meter - 2);
            if score < 0.15 {
                continue;
            }
            candidates.push(MeterHypothesis {
                beats_per_bar: meter,
                beat_unit: 4,
                phase,
                raw_score: score as f32,
                supporting_beats: observed.len().min(u16::MAX as usize) as u16,
            });
        }
    }
    candidates.sort_by(|a, b| b.raw_score.total_cmp(&a.raw_score));
    candidates.truncate(12);
    candidates
}

/// A region's line through its onsets is a tempo measurement only when the
/// supported onsets sit this tightly on it.
const MAX_REGIONAL_FIT_RESIDUAL_MS: f64 = 15.0;
/// Fitted regional tempos must spread at least this much (p10…p90, as a
/// fraction of the global tempo) before a track counts as variable-tempo.
/// Below it, slope differences are measurement noise on a constant grid,
/// and following them would only move the beats off the music.
const VARIABLE_TEMPO_SPREAD: f64 = 0.01;
const VARIABLE_TEMPO_MIN_REGIONS: usize = 4;
/// A change between fitted regions smaller than this (as a fraction of the
/// global tempo) is no movement at all.
const VARIABLE_TEMPO_MIN_STEP: f64 = 0.0015;
/// Beats per tracked region, and how many must carry an onset to fit it.
const REGION_BEATS: usize = 16;
const REGION_MIN_SUPPORTED: usize = 12;
/// Farthest a region's tempo may sit from the global tempo.
const MAX_LOCAL_DEVIATION: f64 = 0.08;

/// One tracked region: its predicted or fitted beat times, and the fitted
/// tempo when the onsets supported a line.
struct TrackedRegion {
    times: Vec<f64>,
    bpm: Option<f64>,
    residual_ms: Option<f64>,
}

/// The strongest onset within `radius_s` of `time_s`, as (time, strength),
/// in the same time convention as the beat grid.
fn onset_near(env: &[f64], fr: f64, rate: u32, time_s: f64, radius_s: f64) -> Option<(f64, f64)> {
    if env.is_empty() {
        return None;
    }
    let frame =
        ((time_s * fr - (WIN as f64 / 2.0) / HOP as f64).round() as isize).max(0) as usize;
    let radius = (radius_s * fr).round() as usize;
    let start = frame.saturating_sub(radius).min(env.len() - 1);
    let end = (frame + radius).min(env.len() - 1);
    let peak = (start..=end).max_by(|a, b| env[*a].total_cmp(&env[*b]))?;
    Some((peak as f64 / fr + WIN as f64 / (2.0 * rate as f64), env[peak]))
}

/// Like `onset_near`, but only a real onset: the window's maximum must be an
/// interior local peak. A maximum on the window's edge is the slope of a
/// neighbouring hit or a decay tail, and fitting those locks the tracker
/// onto a consistently wrong phase.
fn onset_peak(env: &[f64], fr: f64, rate: u32, time_s: f64, radius_s: f64) -> Option<(f64, f64)> {
    if env.len() < 3 {
        return None;
    }
    let frame =
        ((time_s * fr - (WIN as f64 / 2.0) / HOP as f64).round() as isize).max(0) as usize;
    let radius = (radius_s * fr).round() as usize;
    let start = frame.saturating_sub(radius).min(env.len() - 1);
    let end = (frame + radius).min(env.len() - 1);
    let peak = (start..=end).max_by(|a, b| env[*a].total_cmp(&env[*b]))?;
    if !(peak > start && peak < end && env[peak] >= env[peak - 1] && env[peak] >= env[peak + 1]) {
        return None;
    }
    // Parabolic interpolation: the onset's sub-frame position.
    let (a, b, c) = (env[peak - 1], env[peak], env[peak + 1]);
    let curve = a - 2.0 * b + c;
    let delta = if curve < 0.0 {
        (0.5 * (a - c) / curve).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    Some((
        (peak as f64 + delta) / fr + WIN as f64 / (2.0 * rate as f64),
        b,
    ))
}

/// Least-squares line through a region's supported onsets, predicted from
/// `start` at `interval`. `None` unless enough onsets fit tightly.
/// Returns (first beat, interval, residual ms, summed onset strength).
fn fit_region(
    env: &[f64],
    fr: f64,
    rate: u32,
    floor: f64,
    start: f64,
    interval: f64,
) -> Option<RegionFit> {
    let radius = 0.12f64.min(interval * 0.2);
    let mut strength_sum = 0.0;
    let points: Vec<(f64, f64)> = (0..REGION_BEATS)
        .filter_map(|i| {
            let (t, strength) = onset_peak(env, fr, rate, start + i as f64 * interval, radius)?;
            (strength > floor).then(|| {
                strength_sum += strength;
                (i as f64, t)
            })
        })
        .collect();
    if points.len() < REGION_MIN_SUPPORTED {
        return None;
    }
    let n = points.len() as f64;
    let (xm, ym) = (
        points.iter().map(|p| p.0).sum::<f64>() / n,
        points.iter().map(|p| p.1).sum::<f64>() / n,
    );
    let xx: f64 = points.iter().map(|p| (p.0 - xm).powi(2)).sum();
    let slope = points.iter().map(|p| (p.0 - xm) * (p.1 - ym)).sum::<f64>() / xx;
    let intercept = ym - slope * xm;
    let residual_ms = (points
        .iter()
        .map(|p| (p.1 - (intercept + slope * p.0)).powi(2))
        .sum::<f64>()
        / n)
        .sqrt()
        * 1000.0;
    (slope > 0.0 && residual_ms <= MAX_REGIONAL_FIT_RESIDUAL_MS).then_some(RegionFit {
        first: intercept,
        interval: slope,
        residual_ms,
        strength: strength_sum,
    })
}

#[derive(Clone, Copy)]
struct RegionFit {
    first: f64,
    interval: f64,
    residual_ms: f64,
    strength: f64,
}

/// Follow the beat region by region. Tracking starts where the global grid
/// is best supported (the most certain phase), then walks outwards in both
/// directions: each region is predicted from its neighbour's fitted tempo
/// and phase, re-anchored to the onsets it actually finds, and when lock is
/// lost it re-acquires by trying nearby tempos at the predicted phase. Every
/// region is anchored to measured onsets, so errors never accumulate.
fn track_regions(
    env: &[f64],
    fr: f64,
    rate: u32,
    duration_s: f64,
    selected: Tempo,
) -> Vec<TrackedRegion> {
    let global = 60.0 / selected.bpm;
    // A tracked onset must stand clearly above the envelope's mean.
    let floor = env.iter().sum::<f64>() / env.len().max(1) as f64;
    let origin = selected.offset - (selected.offset / global).floor() * global;
    let span = REGION_BEATS as f64 * global;
    let count = ((duration_s - origin) / span).ceil().max(0.0) as usize;
    let Some((_, anchor_fit)) = (0..count.min(100_000 / REGION_BEATS))
        .filter_map(|k| {
            fit_region(env, fr, rate, floor, origin + k as f64 * span, global).map(|f| (k, f))
        })
        .max_by(|a, b| a.1.strength.total_cmp(&b.1.strength))
    else {
        return Vec::new();
    };
    let fit_at = |start: f64, interval: f64| {
        fit_region(env, fr, rate, floor, start, interval)
            .or_else(|| {
                // Lost lock: the music moved. Try nearby tempos at the
                // predicted phase, and keep the best-supported tight line.
                (-16..=16)
                    .map(|k| global * (1.0 + 0.005 * k as f64))
                    .filter(|candidate| (candidate / interval - 1.0).abs() > 0.002)
                    .filter_map(|candidate| fit_region(env, fr, rate, floor, start, candidate))
                    .max_by(|a, b| a.strength.total_cmp(&b.strength))
            })
            .filter(|fit| (fit.interval / global - 1.0).abs() <= MAX_LOCAL_DEVIATION)
    };
    let region = |first: f64, step: f64, fit: Option<RegionFit>| TrackedRegion {
        times: (0..REGION_BEATS)
            .map(|i| first + i as f64 * step)
            .filter(|t| (0.0..duration_s).contains(t))
            .collect(),
        bpm: fit.map(|f| 60.0 / f.interval),
        residual_ms: fit.map(|f| f.residual_ms),
    };
    let mut forward = vec![region(anchor_fit.first, anchor_fit.interval, Some(anchor_fit))];
    let (mut first, mut step) = (anchor_fit.first, anchor_fit.interval);
    loop {
        let start = first + REGION_BEATS as f64 * step;
        if start >= duration_s {
            break;
        }
        let fit = fit_at(start, step);
        (first, step) = fit.map_or((start, step), |f| (f.first, f.interval));
        forward.push(region(first, step, fit));
    }
    let mut backward = Vec::new();
    let (mut first, mut step) = (anchor_fit.first, anchor_fit.interval);
    while first > 0.0 {
        let start = first - REGION_BEATS as f64 * step;
        let fit = fit_at(start, step);
        (first, step) = fit.map_or((start, step), |f| (f.first, f.interval));
        backward.push(region(first, step, fit));
    }
    backward.reverse();
    backward.extend(forward);
    backward.retain(|r| !r.times.is_empty());
    backward
}

/// Whether tracked regional tempos show a real tempo change: they must
/// spread (p10…p90) by `VARIABLE_TEMPO_SPREAD`, and change consistently:
/// at most a third of their significant changes may reverse direction.
/// Music that really moves does so smoothly (a ramp) or in steps; fits that
/// jump back and forth are a tracker catching hats and claps on a
/// constant-tempo track.
fn variable_tempo(regions: &[TrackedRegion], bpm: f64) -> bool {
    if bpm.is_nan() || bpm <= 0.0 {
        return false;
    }
    let fitted: Vec<f64> = regions
        .iter()
        .filter_map(|region| region.bpm)
        .map(|local| local / bpm - 1.0)
        .collect();
    if fitted.len() < VARIABLE_TEMPO_MIN_REGIONS {
        return false;
    }
    // Direction of each significant change between fitted regions: a ramp
    // keeps one direction, a step is one change, noise flips back and forth.
    let moves: Vec<bool> = fitted
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|step| step.abs() > VARIABLE_TEMPO_MIN_STEP)
        .map(|step| step > 0.0)
        .collect();
    let reversals = moves.windows(2).filter(|w| w[0] != w[1]).count();
    if reversals * 3 > moves.len().saturating_sub(1) {
        return false;
    }
    let mut rel = fitted;
    rel.sort_by(f64::total_cmp);
    let at = |q: f64| rel[((rel.len() - 1) as f64 * q).round() as usize];
    at(0.9) - at(0.1) >= VARIABLE_TEMPO_SPREAD
}

/// A beat event at `time_s`, measured against the onsets around it.
fn beat_event(
    env: &[f64],
    fr: f64,
    rate: u32,
    mean: f64,
    time_s: f64,
    beat_index: i64,
    interval: f64,
    downbeat: Option<u8>,
) -> BeatEvent {
    let observed = onset_near(env, fr, rate, time_s, 0.12f64.min(interval * 0.2));
    let strength = observed.map_or(0.0, |o| o.1);
    let bar_position = downbeat.map(|phase| beat_index - phase as i64);
    BeatEvent {
        time_s,
        beat_index,
        interval_s: interval as f32,
        bar_index: bar_position.map(|position| position.div_euclid(4)),
        beat_in_bar: bar_position.map(|position| position.rem_euclid(4) as u8),
        onset_offset_s: observed
            .filter(|_| strength > mean * 0.2)
            .map(|o| compact_beats::offset(compact_beats::offset_q(o.0 - time_s))),
        onset_support: compact_beats::support(compact_beats::support_q(if mean > 0.0 {
            (strength / mean).min(100.0)
        } else {
            0.0
        })),
    }
}

/// Build compact beat events and preserve the strongest distinct tempo
/// interpretations from the same onset analysis used by the global scan.
pub fn rhythm_evidence(
    env: &[f64],
    fr: f64,
    rate: u32,
    duration_s: f64,
    selected: Option<Tempo>,
    downbeat: Option<u8>,
    bar_phases: Vec<BarPhaseHypothesis>,
    raw_scores: &[(f64, f64)],
) -> RhythmEvidence {
    let empty = || RhythmEvidence {
        analyzer_version: RHYTHM_EVIDENCE_VERSION,
        selection_strength: None,
        beats: Vec::new(),
        regions: Vec::new(),
        tempo_hypotheses: Vec::new(),
        meter_hypotheses: Vec::new(),
    };
    if env.len() < (fr * 8.0) as usize || env.iter().all(|x| *x == 0.0) {
        return empty();
    }
    let mut hypotheses = Vec::new();
    for (i, (candidate, raw)) in raw_scores.iter().copied().enumerate() {
        let previous = i
            .checked_sub(1)
            .map(|j| raw_scores[j].1)
            .unwrap_or(f64::MIN);
        let next = raw_scores.get(i + 1).map(|x| x.1).unwrap_or(f64::MIN);
        if raw <= 0.0 || raw < previous || raw < next {
            continue;
        }
        // Keep the detector's actual candidate instead of folding every
        // hypothesis into the DJ display range: half/double-time ambiguity
        // is useful evidence for later planning.
        if hypotheses
            .iter()
            .any(|h: &TempoHypothesis| (h.bpm - candidate).abs() < 0.35)
        {
            continue;
        }
        let Some(offset) = grid_offset_from_envelope(env, fr, rate, candidate) else {
            continue;
        };
        hypotheses.push(TempoHypothesis {
            bpm: candidate,
            beat_offset: offset,
            raw_score: raw as f32,
            bar_phases: Vec::new(),
        });
    }
    if let Some(selected) = selected
        && !hypotheses
            .iter()
            .any(|h| (h.bpm - selected.bpm).abs() < 0.35)
        {
            if hypotheses.len() >= 6 {
                hypotheses.sort_by(|a, b| b.raw_score.total_cmp(&a.raw_score));
                hypotheses.pop();
            }
            hypotheses.push(TempoHypothesis {
                bpm: selected.bpm,
                beat_offset: selected.offset,
                raw_score: score(env, fr, selected.bpm) as f32,
                bar_phases: Vec::new(),
            });
        }
    hypotheses.sort_by(|a, b| b.raw_score.total_cmp(&a.raw_score));
    hypotheses.truncate(6);
    if let Some(selected) = selected
        && let Some(i) = hypotheses
            .iter()
            .position(|h| (h.bpm - selected.bpm).abs() < 0.35)
        {
            hypotheses[i].bpm = selected.bpm;
            hypotheses[i].beat_offset = selected.offset;
            hypotheses[i].bar_phases = bar_phases.clone();
            hypotheses.sort_by(|a, b| b.raw_score.total_cmp(&a.raw_score));
        }

    if let Some(best) = hypotheses.first_mut()
        && best.bar_phases.is_empty() {
            best.bar_phases = bar_phases;
        }
    let mean = env.iter().sum::<f64>() / env.len().max(1) as f64;
    let mut beats = Vec::new();
    let mut regions = Vec::new();
    if let Some(selected) = selected.filter(|t| t.bpm.is_finite() && t.bpm > 0.0) {
        let global = 60.0 / selected.bpm;
        let tracked = track_regions(env, fr, rate, duration_s, selected);
        // Constant-tempo music (nearly all club music) keeps the exact global
        // grid: there, regional slopes are measurement noise. Only a clearly
        // variable track takes the tracked, onset-anchored beat times.
        let variable = variable_tempo(&tracked, selected.bpm);
        let floor = mean;
        let segments: Vec<(Vec<f64>, f64, Option<f64>, Option<f64>)> = if variable {
            tracked
                .into_iter()
                .map(|r| {
                    let step = r.bpm.map_or(global, |bpm| 60.0 / bpm);
                    (r.times, step, r.bpm, r.residual_ms)
                })
                .collect()
        } else {
            let first_index = (-selected.offset / global).ceil() as i64;
            let last_index = ((duration_s - selected.offset) / global).floor() as i64;
            let all: Vec<f64> = (first_index..=last_index)
                .map(|i| selected.offset + i as f64 * global)
                .collect();
            all.chunks(REGION_BEATS)
                .map(|chunk| {
                    let residual = fit_region(env, fr, rate, floor, chunk[0], global)
                        .map(|fit| fit.residual_ms);
                    (chunk.to_vec(), global, None, residual)
                })
                .collect()
        };
        let mut index = segments
            .first()
            .and_then(|s| s.0.first())
            .map_or(0, |t| ((t - selected.offset) / global).round() as i64);
        for (times, interval, local_bpm, residual) in segments {
            let start = beats.len();
            for time in times {
                beats.push(beat_event(env, fr, rate, mean, time, index, interval, downbeat));
                index += 1;
            }
            let covered = &beats[start..];
            if covered.len() < REGION_BEATS / 2 {
                continue;
            }
            let offsets: Vec<f64> = covered
                .iter()
                .filter_map(|beat| beat.onset_offset_s.map(f64::from))
                .collect();
            let mean_offset = offsets.iter().sum::<f64>() / offsets.len().max(1) as f64;
            let jitter = if offsets.is_empty() {
                0.0
            } else {
                (offsets
                    .iter()
                    .map(|offset| (offset - mean_offset).powi(2))
                    .sum::<f64>()
                    / offsets.len() as f64)
                    .sqrt()
            };
            let spread = offsets.iter().copied().fold(f64::MIN, f64::max)
                - offsets.iter().copied().fold(f64::MAX, f64::min);
            let last = covered.last().unwrap();
            regions.push(RhythmRegion {
                start_s: covered[0].time_s,
                end_s: last.time_s + f64::from(last.interval_s),
                beat_count: covered.len().min(u16::MAX as usize) as u16,
                observed_onset_coverage: offsets.len() as f32 / covered.len() as f32,
                mean_onset_support: covered.iter().map(|beat| beat.onset_support).sum::<f32>()
                    / covered.len() as f32,
                onset_jitter_ms: (jitter * 1000.0) as f32,
                phase_spread_ms: if offsets.is_empty() {
                    0.0
                } else {
                    (spread * 1000.0) as f32
                },
                local_bpm: local_bpm.map(|bpm| bpm as f32),
                tempo_fit_residual_ms: residual.map(|r| r as f32),
            });
        }
    }
    RhythmEvidence {
        analyzer_version: RHYTHM_EVIDENCE_VERSION,
        selection_strength: selected.and_then(|tempo| selection_strength(raw_scores, tempo.bpm)),
        meter_hypotheses: meter_hypotheses(&beats),
        beats,
        regions,
        tempo_hypotheses: hypotheses,
    }
}

/// Score-curve concentration times separation from the strongest peak
/// outside the selected tempo's basin (± 3 %), on the raw scan scores.
/// Octave relatives count as rivals: this measures ambiguity, not steadiness.
fn selection_strength(raw_scores: &[(f64, f64)], bpm: f64) -> Option<f32> {
    let (best_bpm, best) = raw_scores
        .iter()
        .copied()
        .filter(|(candidate, _)| {
            let r = candidate / bpm;
            [0.5, 1.0, 2.0].iter().any(|k| (r / k - 1.0).abs() <= 0.03)
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    if best.is_nan() || best <= 0.0 {
        return None;
    }
    let mean = raw_scores.iter().map(|x| x.1).sum::<f64>() / raw_scores.len() as f64;
    let runner_up = raw_scores
        .iter()
        .filter(|(candidate, _)| (candidate / best_bpm - 1.0).abs() > 0.03)
        .map(|(_, score)| *score)
        .fold(0.0f64, f64::max);
    let concentration = (1.0 - mean / best).clamp(0.0, 1.0);
    let separation = ((best - runner_up).max(0.0) / best).clamp(0.0, 1.0);
    Some((concentration * separation).sqrt() as f32)
}

/// Onset envelope of the low band only (bins below `max_hz`): the kick.
pub fn low_onset_envelope(mono: &[f32], rate: u32, max_hz: f64) -> Vec<f64> {
    let top = ((max_hz / (rate as f64 / WIN as f64)) as usize).max(2);
    let mut prev: Vec<f64> = vec![];
    let mut env = vec![];
    stft(mono, WIN, HOP, |_, mag| {
        let cur: Vec<f64> = mag[..top.min(mag.len())]
            .iter()
            .map(|m| (1.0 + 1000.0 * m).ln())
            .collect();
        env.push(if prev.is_empty() {
            0.0
        } else {
            cur.iter().zip(&prev).map(|(c, p)| (c - p).max(0.0)).sum()
        });
        prev = cur;
    });
    env
}

/// How strongly a grid of `period` frames sits on the envelope's onsets:
/// the best phase's mean envelope at grid points (± one frame), over the
/// envelope's mean. The true beat of 4/4 dance music has a kick at every
/// point; a 3:2 grid only at every other one.
/// `grid_strength` in 20-second windows, averaged. Each window finds its
/// own best phase, so a candidate a hair off the true tempo is not punished
/// for drifting off the kicks over a whole track: candidates compare on how
/// well their pulse sits on the music, not on how exactly they were scanned.
fn windowed_grid_strength(env: &[f64], fr: f64, period: f64) -> f64 {
    let window = ((20.0 * fr) as usize).max(period as usize * 8);
    let chunks: Vec<f64> = env
        .chunks(window)
        .filter(|chunk| chunk.len() >= window / 2)
        .map(|chunk| grid_strength(chunk, period))
        .collect();
    if chunks.is_empty() {
        grid_strength(env, period)
    } else {
        chunks.iter().sum::<f64>() / chunks.len() as f64
    }
}

pub fn grid_strength(env: &[f64], period: f64) -> f64 {
    let mean = env.iter().sum::<f64>() / env.len().max(1) as f64;
    if mean <= 0.0 || period < 2.0 {
        return 0.0;
    }
    let near = |t: f64| {
        let i = t.round() as usize;
        (i.saturating_sub(1)..=(i + 1).min(env.len() - 1))
            .map(|j| env[j])
            .fold(0.0, f64::max)
    };
    let mut best = 0.0f64;
    let mut o = 0.0;
    while o < period {
        let (mut t, mut s, mut n) = (o, 0.0, 0usize);
        while t < env.len() as f64 - 2.0 {
            s += near(t);
            n += 1;
            t += period;
        }
        if n > 0 {
            best = best.max(s / n as f64);
        }
        o += 0.5;
    }
    best / mean
}

/// Onset envelope and its frame rate.
pub fn onset_envelope(mono: &[f32], rate: u32) -> (Vec<f64>, f64) {
    let mut prev: Vec<f64> = vec![];
    let mut env = vec![];
    stft(mono, WIN, HOP, |_, mag| {
        let cur: Vec<f64> = mag.iter().map(|m| (1.0 + 1000.0 * m).ln()).collect();
        let flux = if prev.is_empty() {
            0.0
        } else {
            cur.iter().zip(&prev).map(|(c, p)| (c - p).max(0.0)).sum()
        };
        env.push(flux);
        prev = cur;
    });
    let fr = rate as f64 / HOP as f64;
    // Remove the slowly varying part (loudness changes), keep the onsets.
    let w = (fr * 0.5) as usize | 1;
    let mut out = vec![0.0; env.len()];
    let mut sum: f64 = env.iter().take(w / 2).sum();
    let mut count = w / 2;
    for i in 0..env.len() {
        if i + w / 2 < env.len() {
            sum += env[i + w / 2];
            count += 1;
        }
        if i > w / 2 {
            sum -= env[i - w / 2 - 1];
            count -= 1;
        }
        out[i] = (env[i] - sum / count.max(1) as f64).max(0.0);
    }
    (out, fr)
}

fn interp(x: &[f64], t: f64) -> f64 {
    let i = t.floor() as usize;
    let f = t - i as f64;
    match (x.get(i), x.get(i + 1)) {
        (Some(a), Some(b)) => a + (b - a) * f,
        (Some(a), None) => *a,
        _ => 0.0,
    }
}

/// Autocorrelation at a fractional lag (normalised by overlap).
fn acf(env: &[f64], lag: f64) -> f64 {
    let n = env.len() as f64 - lag.ceil();
    if n <= 1.0 {
        return 0.0;
    }
    let mut s = 0.0;
    let whole = lag.floor() as usize;
    let f = lag - whole as f64;
    for i in 0..(n as usize) {
        let b = env[i + whole] * (1.0 - f) + env.get(i + whole + 1).copied().unwrap_or(0.0) * f;
        s += env[i] * b;
    }
    s / n
}

fn score(env: &[f64], fr: f64, bpm: f64) -> f64 {
    let lag = 60.0 * fr / bpm;
    let comb: f64 = (1..=4).map(|k| acf(env, lag * k as f64) / k as f64).sum();
    // Broad prior: one octave either side of 120 BPM costs e^-0.5.
    let prior = (-0.5 * ((bpm / 120.0).log2()).powi(2)).exp();
    comb * (0.5 + 0.5 * prior)
}

pub fn detect(mono: &[f32], rate: u32) -> Option<Tempo> {
    let (env, fr) = onset_envelope(mono, rate);
    let low = low_onset_envelope(mono, rate, 150.0);
    detect_from_envelopes(&env, &low, fr, rate)
}

/// Tempo detector using an already computed onset envelope, allowing the
/// library scan to derive the selected grid and alternative hypotheses in
/// one spectral pass.
pub fn detect_from_envelopes(env: &[f64], low_env: &[f64], fr: f64, rate: u32) -> Option<Tempo> {
    scan_from_envelopes(env, low_env, fr, rate).selected
}

/// Global tempo search result, retaining the exact score curve so downstream
/// analysis can derive alternatives without repeating autocorrelation work.
pub fn scan_from_envelopes(env: &[f64], low_env: &[f64], fr: f64, rate: u32) -> TempoScan {
    if env.len() < (fr * 8.0) as usize || env.iter().all(|x| *x == 0.0) {
        return TempoScan {
            selected: None,
            raw_scores: Vec::new(),
        }; // under ~8 s, or no onsets
    }
    let mut best = (0.0, f64::MIN);
    let mut raw_scores = vec![];
    let mut bpm = 60.0;
    while bpm <= 200.0 {
        let s = score(env, fr, bpm);
        raw_scores.push((bpm, s));
        if s > best.1 {
            best = (bpm, s);
        }
        bpm += 0.05;
    }
    let fine = best;
    let mut bpm = fine.0;
    while bpm < 70.0 {
        bpm *= 2.0;
    }
    while bpm > 180.0 {
        bpm /= 2.0;
    }
    // Related readings, judged on the kick (low band) at each candidate's
    // own fitted tempo: a coarse scan step drifts a grid off the kicks over
    // a whole track and would hide the stronger candidate.
    let fitted = |b: f64| grid_fit_from_envelope(env, fr, rate, b).map_or(b, |(f, _)| f);
    let strength = |b: f64| windowed_grid_strength(low_env, fr, 60.0 * fr / b);
    bpm = fitted(bpm);
    // Octave: a half-time reading whose in-between beats carry kicks as
    // strongly as its own is double time. Only into the dance range (to
    // 160 BPM), so drum & bass and hip-hop keep their half-time reading.
    {
        let double = fitted(bpm * 2.0);
        if double <= 160.0 && strength(double) >= 0.9 * strength(bpm) {
            bpm = double;
        }
    }
    // 3:2: off-beat hats or bass can make a 1.5-beat period look like the
    // beat, so the scan reads 127 as 84.7. The faster reading wins when its
    // grid sits on the kicks more strongly. Only upwards: measured on 956
    // tagged DJ tracks, this fixes 25 of 28 such errors without breaking
    // any; the downward check broke more tracks than it fixed.
    {
        let up = fitted(bpm * 1.5);
        if up <= 180.0 && strength(up) > 1.05 * strength(bpm) {
            bpm = up;
        }
    }
    // Score-curve concentration: how clearly periodic the onsets are. This
    // is the grid's steadiness, the value every consumer gates on (≥ 0.5).
    // Ambiguity between competing pulse rates is kept apart, in the rhythm
    // evidence (`selection_strength`), so it can't silently demote a steady
    // 4/4 track whose half- and double-time peaks are naturally strong.
    let mean = raw_scores.iter().map(|x| x.1).sum::<f64>() / raw_scores.len() as f64;
    let confidence = if fine.1 > 0.0 {
        (1.0 - mean / fine.1).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };

    let selected = grid_fit_from_envelope(env, fr, rate, bpm).map(|(bpm, offset)| Tempo {
        bpm: (bpm * 100.0).round() / 100.0,
        offset,
        confidence,
    });
    TempoScan {
        selected,
        raw_scores,
    }
}

/// Find a grid phase at a supplied tempo. Reused when file metadata corrects
/// a detector's half/double-time choice, so phase matches the final BPM.
pub fn grid_offset(mono: &[f32], rate: u32, bpm: f64) -> Option<f64> {
    let (env, fr) = onset_envelope(mono, rate);
    grid_offset_from_envelope(&env, fr, rate, bpm)
}

/// Kick-led phase for DJ alignment. The broad-band onset grid may lock to
/// hats or claps ahead of the kick; those sound off when two decks overlap.
pub fn kick_grid_offset(mono: &[f32], rate: u32, bpm: f64) -> Option<f64> {
    let env = low_onset_envelope(mono, rate, 150.0);
    grid_offset_from_envelope(&env, rate as f64 / HOP as f64, rate, bpm)
}

/// A kick-led grid whose tempo is also fine-tuned from the long-term onset
/// slope. A few hundredths of a BPM error become an audible flam near the
/// outro, even when the two mathematical deck phases read zero.
pub fn kick_grid_fit(mono: &[f32], rate: u32, bpm: f64) -> Option<Tempo> {
    let env = low_onset_envelope(mono, rate, 150.0);
    let fr = rate as f64 / HOP as f64;
    let strength = grid_strength(&env, 60.0 * fr / bpm);
    if strength < 2.0 {
        return None;
    }
    // The whole-track comb can prefer a recurring hat/clap phase. Anchor
    // the phase to the opening minute's kick, then use the long track only
    // to measure how its tempo drifts from the coarse BPM estimate.
    let opening = &mono[..mono.len().min(rate as usize * 60)];
    let offset = kick_grid_offset(opening, rate, bpm)?;
    let period = 60.0 * fr / bpm;
    let phase = ((offset * rate as f64 - WIN as f64 / 2.0) / HOP as f64).rem_euclid(period);
    let (slope, first) = fit_grid(&env, period, phase).unwrap_or((period, phase));
    let bpm = 60.0 * fr / slope;
    let offset = (first * HOP as f64 + WIN as f64 / 2.0) / rate as f64 + KICK_ONSET_LAG_S;
    Some(Tempo {
        bpm,
        offset,
        confidence: (strength / 10.0).min(1.0) as f32,
    })
}

fn grid_offset_from_envelope(env: &[f64], fr: f64, rate: u32, bpm: f64) -> Option<f64> {
    grid_fit_from_envelope(env, fr, rate, bpm).map(|(_, offset)| offset)
}

fn grid_fit_from_envelope(env: &[f64], fr: f64, rate: u32, bpm: f64) -> Option<(f64, f64)> {
    if env.len() < (fr * 8.0) as usize
        || env.iter().all(|x| *x == 0.0)
        || !(40.0..=300.0).contains(&bpm)
    {
        return None;
    }
    let period = 60.0 * fr / bpm;
    let mut phase = (0.0, f64::MIN);
    let mut o = 0.0;
    while o < period {
        let mut t = o;
        let mut s = 0.0;
        while t < env.len() as f64 - 1.0 {
            s += interp(env, t);
            t += period;
        }
        if s > phase.1 {
            phase = (o, s);
        }
        o += 0.25;
    }
    // Fit the grid to the beats themselves: find the onset peak near each
    // predicted beat (sub-frame, parabolic), then a least-squares line
    // through them: slope = period, intercept = first beat. Over a whole
    // track this resolves the tempo to hundredths of a BPM.
    let (slope, first) = fit_grid(env, period, phase.0).unwrap_or((period, phase.0));
    // A flux frame sits between two STFT frames: centre of the later one.
    Some((
        60.0 * fr / slope,
        (first * HOP as f64 + WIN as f64 / 2.0) / rate as f64,
    ))
}

/// Refine (period, first beat) in frames by fitting the grid to the onset
/// peaks nearest each predicted beat.
fn fit_grid(env: &[f64], period: f64, phase: f64) -> Option<(f64, f64)> {
    let loudest = env.iter().cloned().fold(0.0, f64::max);
    let mut pts: Vec<(f64, f64)> = vec![];
    let mut k = 0usize;
    loop {
        let p = phase + k as f64 * period;
        if p >= env.len() as f64 - 2.0 {
            break;
        }
        let (lo, hi) = (
            (p - period * 0.2).max(1.0) as usize,
            ((p + period * 0.2) as usize).min(env.len() - 2),
        );
        if let Some(i) = (lo..=hi).max_by(|a, b| env[*a].total_cmp(&env[*b])) {
            // Only clear onsets pin the grid.
            if env[i] > loudest * 0.2 {
                let (a, b, c) = (env[i - 1], env[i], env[i + 1]);
                let d = a - 2.0 * b + c;
                let frac = if d.abs() > 1e-12 {
                    (0.5 * (a - c) / d).clamp(-0.5, 0.5)
                } else {
                    0.0
                };
                pts.push((k as f64, i as f64 + frac));
            }
        }
        k += 1;
    }
    let fit = |pts: &[(f64, f64)]| -> Option<(f64, f64)> {
        let n = pts.len() as f64;
        if n < 8.0 {
            return None;
        }
        let (sx, sy) = pts.iter().fold((0.0, 0.0), |(a, b), (x, y)| (a + x, b + y));
        let (mx, my) = (sx / n, sy / n);
        let (mut sxx, mut sxy) = (0.0, 0.0);
        for (x, y) in pts {
            sxx += (x - mx) * (x - mx);
            sxy += (x - mx) * (y - my);
        }
        let slope = sxy / sxx;
        Some((slope, my - slope * mx))
    };
    let (slope, icpt) = fit(&pts)?;
    // Drop outliers (fills, off-beat hits) and fit again.
    let kept: Vec<(f64, f64)> = pts
        .iter()
        .copied()
        .filter(|(x, y)| (y - (icpt + slope * x)).abs() < period * 0.08)
        .collect();
    let (slope, icpt) = fit(&kept).unwrap_or((slope, icpt));
    // Guard: the fit may only fine-tune, never change the tempo.
    ((slope / period - 1.0).abs() < 0.02).then_some((slope, icpt.rem_euclid(slope)))
}

/// Which beat of four starts the bar, and a 0…1 confidence. Bars tend to
/// start with the strongest low-end onset (kick with bass, crash): the
/// low-band (< 150 Hz) energy just after each grid beat is summed per beat
/// position mod 4; the strongest position is the downbeat.
pub fn downbeat_hypotheses(mono: &[f32], rate: u32, t: &Tempo) -> Vec<BarPhaseHypothesis> {
    // Low band: two cascaded one-pole low-passes at 150 Hz.
    let a = (-2.0 * std::f64::consts::PI * 150.0 / rate as f64).exp();
    let (mut y1, mut y2) = (0.0f64, 0.0f64);
    let low: Vec<f32> = mono
        .iter()
        .map(|&x| {
            y1 = (1.0 - a) * x as f64 + a * y1;
            y2 = (1.0 - a) * y1 + a * y2;
            y2 as f32
        })
        .collect();
    let per = 60.0 / t.bpm * rate as f64;
    let win = (0.06 * rate as f64) as usize;
    let mut sums = [0.0f64; 4];
    let mut counts = [0usize; 4];
    let mut k = 0usize;
    loop {
        let at = (t.offset * rate as f64 + k as f64 * per) as usize;
        if at + win >= low.len() {
            break;
        }
        let e: f64 = low[at..at + win].iter().map(|v| (*v as f64).powi(2)).sum();
        sums[k % 4] += e;
        counts[k % 4] += 1;
        k += 1;
    }
    if counts.iter().any(|c| *c < 4) {
        return Vec::new();
    }
    let mut phases: Vec<_> = (0..4)
        .map(|i| BarPhaseHypothesis {
            downbeat: i as u8,
            raw_score: (sums[i] / counts[i] as f64) as f32,
        })
        .collect();
    phases.sort_by(|a, b| b.raw_score.total_cmp(&a.raw_score));
    phases
}

pub fn downbeat(mono: &[f32], rate: u32, t: &Tempo) -> Option<(u8, f32)> {
    let phases = downbeat_hypotheses(mono, rate, t);
    downbeat_from_hypotheses(&phases)
}

pub fn downbeat_from_hypotheses(phases: &[BarPhaseHypothesis]) -> Option<(u8, f32)> {
    let top = phases.first()?;
    let second = phases.get(1).map_or(0.0, |p| p.raw_score);
    let confidence = if top.raw_score > 0.0 {
        ((top.raw_score - second) / top.raw_score * 3.0).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Some((top.downbeat, confidence))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A kick-like click track: decaying 60 Hz thump plus a noise transient.
    fn clicks(bpm: f64, secs: f64, rate: u32, offset: f64) -> Vec<f32> {
        let n = (secs * rate as f64) as usize;
        let mut x = vec![0.0f32; n];
        let period = 60.0 / bpm;
        let mut seed = 1u32;
        let mut t = offset;
        while t < secs {
            let start = (t * rate as f64) as usize;
            for i in 0..(rate as usize / 8) {
                if start + i >= n {
                    break;
                }
                let tt = i as f64 / rate as f64;
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let noise = (seed as f64 / u32::MAX as f64 - 0.5) * (-tt * 400.0).exp();
                let thump = (std::f64::consts::TAU * 60.0 * tt).sin() * (-tt * 25.0).exp();
                x[start + i] += (0.6 * thump + 0.4 * noise) as f32;
            }
            t += period;
        }
        x
    }

    fn accented_beats(pattern: &[f32], bars: usize) -> Vec<BeatEvent> {
        (0..pattern.len() * bars)
            .map(|index| BeatEvent {
                time_s: index as f64 * 0.5,
                beat_index: index as i64,
                interval_s: 0.5,
                bar_index: None,
                beat_in_bar: None,
                onset_offset_s: Some(0.0),
                onset_support: pattern[index % pattern.len()],
            })
            .collect()
    }

    fn accented_clicks(pattern: &[f32], bpm: f64, bars: usize, rate: u32) -> Vec<f32> {
        let interval = 60.0 / bpm;
        let duration_s = interval * pattern.len() as f64 * bars as f64 + 1.0;
        let mut audio = vec![0.0f32; (duration_s * rate as f64) as usize];
        let mut seed = 0x91e1_0da5u32;
        let mut beat = 0usize;
        let mut time = 0.3;
        while time < duration_s {
            let start = (time * rate as f64) as usize;
            let level = pattern[beat % pattern.len()];
            for i in 0..(rate as usize / 8) {
                if start + i >= audio.len() {
                    break;
                }
                let t = i as f64 / rate as f64;
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let noise = (seed as f64 / u32::MAX as f64 - 0.5) * (-t * 400.0).exp();
                let thump = (std::f64::consts::TAU * 60.0 * t).sin() * (-t * 25.0).exp();
                audio[start + i] += (f64::from(level) * (0.6 * thump + 0.4 * noise)) as f32;
            }
            beat += 1;
            time += interval;
        }
        audio
    }

    #[test]
    fn meter_evidence_preserves_odd_meter_and_phase_alternatives() {
        let triple = meter_hypotheses(&accented_beats(&[1.0, 0.12, 0.25], 12));
        assert_eq!(triple.first().unwrap().beats_per_bar, 3, "{triple:?}");
        assert_eq!(triple.first().unwrap().phase, 0);
        assert!(
            triple
                .iter()
                .any(|hypothesis| hypothesis.beats_per_bar == 6)
        );
        assert!(
            triple
                .iter()
                .all(|hypothesis| hypothesis.beat_unit == 4 && hypothesis.supporting_beats == 36)
        );

        let five = meter_hypotheses(&accented_beats(&[1.0, 0.12, 0.2, 0.12, 0.3], 10));
        assert_eq!(five.first().unwrap().beats_per_bar, 5, "{five:?}");
        assert_eq!(five.first().unwrap().phase, 0);
        assert!(meter_hypotheses(&accented_beats(&[1.0], 24)).is_empty());
    }

    #[test]
    fn complete_onset_pipeline_recovers_meter_cycles_and_ambiguity() {
        for (pattern, expected_meter, expected_phase, max_rank, bars) in [
            (&[1.0, 0.12, 0.25][..], 3u8, 0u8, 0usize, 16usize),
            (&[0.12, 0.25, 1.0][..], 3u8, 2u8, 0usize, 16usize),
            // A common four-beat accent pattern also supports a two-beat
            // interpretation; preserve that ambiguity instead of requiring
            // a false single winner.
            (&[1.0, 0.12, 0.65, 0.12][..], 4u8, 0u8, 2usize, 16usize),
            (&[1.0, 0.12, 0.2, 0.12, 0.3][..], 5u8, 0u8, 0usize, 12usize),
        ] {
            let rate = 22_050;
            let audio = accented_clicks(pattern, 120.0, bars, rate);
            let (env, frames_per_second) = onset_envelope(&audio, rate);
            let low = low_onset_envelope(&audio, rate, 150.0);
            let scan = scan_from_envelopes(&env, &low, frames_per_second, rate);
            let tempo = scan.selected.expect("detected beat pulse");
            assert!((tempo.bpm - 120.0).abs() < 0.2, "{} BPM", tempo.bpm);
            let downbeats = downbeat_hypotheses(&audio, rate, &tempo);
            let downbeat = downbeat_from_hypotheses(&downbeats).map(|phase| phase.0);
            let evidence = rhythm_evidence(
                &env,
                frames_per_second,
                rate,
                audio.len() as f64 / f64::from(rate),
                Some(tempo),
                downbeat,
                downbeats,
                &scan.raw_scores,
            );
            let (rank, meter) = evidence
                .meter_hypotheses
                .iter()
                .enumerate()
                .find(|(_, meter)| meter.beats_per_bar == expected_meter)
                .unwrap_or_else(|| {
                    panic!(
                        "{expected_meter}/4 candidates: {:?}",
                        evidence.meter_hypotheses
                    )
                });
            assert!(
                rank <= max_rank,
                "{expected_meter}/4 ranked {rank}: {:?}",
                evidence.meter_hypotheses
            );
            assert_eq!(meter.phase, expected_phase);
        }
    }

    #[test]
    fn finds_common_dj_tempos() {
        for bpm in [90.0, 124.0, 128.0, 140.0, 174.0] {
            let x = clicks(bpm, 40.0, 22_050, 0.3);
            let t = detect(&x, 22_050).expect("a beat");
            assert!((t.bpm - bpm).abs() < 0.1, "{bpm} → {}", t.bpm);
        }
    }

    #[test]
    fn competing_tempo_peaks_reduce_raw_selection_strength() {
        let rate = 22_050;
        let clean = clicks(120.0, 40.0, rate, 0.3);
        let mut ambiguous = clicks(120.0, 40.0, rate, 0.3);
        let second = clicks(93.0, 40.0, rate, 0.17);
        for (sample, other) in ambiguous.iter_mut().zip(second) {
            *sample += other;
        }
        let clean_score = detect(&clean, rate).unwrap().confidence;
        let ambiguous_score = detect(&ambiguous, rate).unwrap().confidence;
        assert!(
            ambiguous_score < clean_score,
            "ambiguous score {ambiguous_score} should be below clean score {clean_score}"
        );
    }

    #[test]
    fn measured_periodicity_wins_far_from_the_prior_center() {
        for bpm in [72.0, 84.0, 160.0] {
            let x = clicks(bpm, 40.0, 22_050, 0.3);
            let t = detect(&x, 22_050).expect("a beat");
            assert!((t.bpm - bpm).abs() < 0.1, "{bpm} → {}", t.bpm);
        }
    }

    #[test]
    fn grid_lands_on_the_beats() {
        let x = clicks(128.0, 40.0, 22_050, 0.3);
        let t = detect(&x, 22_050).unwrap();
        let period = 60.0 / t.bpm;
        let phase_err = ((t.offset - 0.3) / period).rem_euclid(1.0);
        let err_s = phase_err.min(1.0 - phase_err) * period;
        assert!(err_s < 0.015, "grid off by {:.1} ms", err_s * 1e3);
    }

    #[test]
    fn kick_grid_corrects_phase_and_long_track_tempo() {
        let rate = 22_050;
        let bpm = 124.0;
        let mut x = clicks(bpm, 100.0, rate, 0.3);
        // Bright offbeat hats should not become the beat phase.
        let mut t = 0.3 + 30.0 / bpm;
        while t < 100.0 {
            let start = (t * rate as f64) as usize;
            for i in 0..220 {
                if start + i < x.len() {
                    let time = i as f64 / rate as f64;
                    x[start + i] += (2.0
                        * (std::f64::consts::TAU * 4_000.0 * time).sin()
                        * (-time * 700.0).exp()) as f32;
                }
            }
            t += 60.0 / bpm;
        }
        let fit = kick_grid_fit(&x, rate, 124.05).unwrap();
        let period = 60.0 / fit.bpm;
        let phase_err = (fit.offset - 0.3).rem_euclid(period);
        assert!(
            phase_err.min(period - phase_err) < 0.005,
            "phase {}",
            fit.offset
        );
        assert!((fit.bpm - bpm).abs() < 0.02, "tempo {}", fit.bpm);
    }

    #[test]
    fn finds_the_downbeat() {
        // Every 4th beat, starting with the third one, carries a heavier kick.
        let rate = 22_050;
        let bpm = 124.0;
        let mut x = clicks(bpm, 40.0, rate, 0.3);
        let per = 60.0 / bpm;
        let mut k = 2usize;
        while 0.3 + k as f64 * per < 39.5 {
            let start = ((0.3 + k as f64 * per) * rate as f64) as usize;
            for i in 0..(rate as usize / 8) {
                let tt = i as f64 / rate as f64;
                x[start + i] +=
                    ((std::f64::consts::TAU * 55.0 * tt).sin() * (-tt * 12.0).exp() * 0.8) as f32;
            }
            k += 4;
        }
        let t = detect(&x, rate).unwrap();
        let (d, c) = downbeat(&x, rate, &t).unwrap();
        // The grid may start on any of the first beats; the accented one is
        // at 0.3 s + 2 beats.
        let first_grid_beat = ((0.3 - t.offset) / per).round() as i64;
        assert_eq!(
            (d as i64 - (first_grid_beat + 2)).rem_euclid(4),
            0,
            "downbeat {d}, confidence {c}"
        );
        assert!(c > 0.3, "confidence {c}");
    }

    #[test]
    fn silence_has_no_tempo() {
        let silent = vec![0.0; 22_050 * 20];
        assert!(detect(&silent, 22_050).is_none());
        let (env, fr) = onset_envelope(&silent, 22_050);
        assert!(
            rhythm_evidence(&env, fr, 22_050, 20.0, None, None, Vec::new(), &[])
                .tempo_hypotheses
                .is_empty()
        );
    }

    #[test]
    fn rhythm_evidence_retains_events_and_alternative_tempos() {
        let rate = 22_050;
        let x = clicks(124.0, 40.0, rate, 0.3);
        let (env, fr) = onset_envelope(&x, rate);
        let low = low_onset_envelope(&x, rate, 150.0);
        let scan = scan_from_envelopes(&env, &low, fr, rate);
        let t = scan.selected.unwrap();
        let d = downbeat(&x, rate, &t).map(|x| x.0);
        let phases = downbeat_hypotheses(&x, rate, &t);
        let evidence = rhythm_evidence(&env, fr, rate, 40.0, Some(t), d, phases, &scan.raw_scores);

        assert_eq!(evidence.analyzer_version, RHYTHM_EVIDENCE_VERSION);
        assert!((80..=84).contains(&evidence.beats.len()));
        assert!(evidence.beats.windows(2).all(|w| w[1].time_s > w[0].time_s));
        // A steady track keeps the exact global grid: no regional tempos,
        // constant spacing, yet each region's onset fit is still recorded.
        assert!(evidence.regions.iter().all(|r| r.local_bpm.is_none()));
        assert!(
            evidence
                .regions
                .iter()
                .any(|r| r.tempo_fit_residual_ms.is_some_and(|ms| ms < 15.0))
        );
        let period = 60.0 / t.bpm;
        assert!(evidence.beats.windows(2).all(|w| {
            ((w[1].time_s - w[0].time_s) - period).abs() < 1e-9
        }));
        assert!(evidence.beats.windows(2).all(|window| {
            (window[0].time_s + f64::from(window[0].interval_s) - window[1].time_s).abs() < 1e-5
        }));
        assert!(
            evidence
                .beats
                .iter()
                .all(|b| b.beat_in_bar.is_some_and(|p| (0..4).contains(&p)))
        );
        assert!(
            evidence
                .tempo_hypotheses
                .iter()
                .any(|h| (h.bpm - t.bpm).abs() < 0.1)
        );
        assert!(
            evidence
                .tempo_hypotheses
                .iter()
                .any(|h| (h.bpm * 2.0 - t.bpm).abs() < 1.0)
        );
        assert!(evidence.meter_hypotheses.is_empty());
        assert!(
            evidence
                .tempo_hypotheses
                .windows(2)
                .all(|w| w[0].raw_score >= w[1].raw_score)
        );
        let primary = evidence
            .tempo_hypotheses
            .iter()
            .find(|h| (h.bpm - t.bpm).abs() < 0.1)
            .unwrap();
        assert_eq!(primary.bar_phases.len(), 4);
        assert!(
            primary
                .bar_phases
                .windows(2)
                .all(|w| w[0].raw_score >= w[1].raw_score)
        );
        let unphased = rhythm_evidence(
            &env,
            fr,
            rate,
            40.0,
            Some(t),
            None,
            Vec::new(),
            &scan.raw_scores,
        );
        assert!(
            unphased
                .beats
                .iter()
                .all(|b| b.bar_index.is_none() && b.beat_in_bar.is_none())
        );

        let mut regional = clicks(124.0, 40.0, rate, 0.3);
        regional[(15 * rate as usize)..(24 * rate as usize)].fill(0.0);
        let (regional_env, regional_fr) = onset_envelope(&regional, rate);
        let regional_low = low_onset_envelope(&regional, rate, 150.0);
        let regional_scan = scan_from_envelopes(&regional_env, &regional_low, regional_fr, rate);
        let regional_tempo = regional_scan.selected.unwrap();
        let regional_phases = downbeat_hypotheses(&regional, rate, &regional_tempo);
        let regional_evidence = rhythm_evidence(
            &regional_env,
            regional_fr,
            rate,
            40.0,
            Some(regional_tempo),
            Some(0),
            regional_phases,
            &regional_scan.raw_scores,
        );
        assert!(
            regional_evidence
                .regions
                .iter()
                .any(|r| r.observed_onset_coverage < 0.3)
        );
        assert!(
            regional_evidence
                .regions
                .iter()
                .any(|r| r.observed_onset_coverage > 0.7)
        );
    }

    #[test]
    fn beats_store_compactly_and_round_trip_exactly() {
        let rate = 22_050;
        let x = clicks(124.0, 60.0, rate, 0.3);
        let (env, fr) = onset_envelope(&x, rate);
        let low = low_onset_envelope(&x, rate, 150.0);
        let scan = scan_from_envelopes(&env, &low, fr, rate);
        let t = scan.selected.unwrap();
        let evidence = rhythm_evidence(&env, fr, rate, 60.0, Some(t), Some(1), Vec::new(), &scan.raw_scores);
        let text = serde_json::to_string(&evidence).unwrap();
        assert!(text.contains("\"origin\""), "constant grid not stored compactly");
        let back: RhythmEvidence = serde_json::from_str(&text).unwrap();
        assert_eq!(back.beats, evidence.beats);
        // Far smaller than one JSON object per beat, which still reads.
        let verbose = serde_json::to_string(&evidence.beats).unwrap();
        let beats_json = serde_json::to_string(&serde_json::from_str::<serde_json::Value>(&text).unwrap()["beats"]).unwrap();
        assert!(beats_json.len() * 3 < verbose.len(), "{} vs {}", beats_json.len(), verbose.len());
        let old = text.replace(&beats_json, &verbose);
        let read: RhythmEvidence = serde_json::from_str(&old).unwrap();
        assert_eq!(read.beats, evidence.beats);
    }

    /// Clicks at explicit times (the same click as `clicks`).
    fn clicks_at(times: &[f64], secs: f64, rate: u32) -> Vec<f32> {
        let n = (secs * rate as f64) as usize;
        let mut x = vec![0.0f32; n];
        let mut seed = 1u32;
        for t in times {
            let start = (t * rate as f64) as usize;
            for i in 0..(rate as usize / 8) {
                if start + i >= n {
                    break;
                }
                let tt = i as f64 / rate as f64;
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let noise = (seed as f64 / u32::MAX as f64 - 0.5) * (-tt * 400.0).exp();
                let thump = (std::f64::consts::TAU * 60.0 * tt).sin() * (-tt * 25.0).exp();
                x[start + i] += (0.6 * thump + 0.4 * noise) as f32;
            }
        }
        x
    }

    #[test]
    fn tracked_beats_follow_a_gradual_tempo_ramp_without_drifting() {
        // A drummer speeding up from 118 to 126 BPM over 90 s.
        let (rate, secs) = (22_050, 90.0);
        let mut times = vec![0.4];
        while *times.last().unwrap() < secs - 0.5 {
            let t = *times.last().unwrap();
            let bpm = 118.0 + 8.0 * (t / secs);
            times.push(t + 60.0 / bpm);
        }
        let x = clicks_at(&times, secs, rate);
        let (env, fr) = onset_envelope(&x, rate);
        let low = low_onset_envelope(&x, rate, 150.0);
        let scan = scan_from_envelopes(&env, &low, fr, rate);
        let t = scan.selected.unwrap();
        let evidence = rhythm_evidence(&env, fr, rate, secs, Some(t), Some(0), Vec::new(), &scan.raw_scores);
        assert!(evidence.regions.iter().any(|r| r.local_bpm.is_some()), "ramp not tracked");
        // Every true hit has a tracked beat at the same small onset-detector
        // bias (the global grid shares it; the kick grid removes it), and that
        // error stays flat over the ramp: nothing accumulates.
        let errors: Vec<f64> = times
            .iter()
            .filter(|hit| **hit > 2.0 && **hit < secs - 2.0)
            .map(|hit| {
                evidence
                    .beats
                    .iter()
                    .map(|b| b.time_s - hit)
                    .min_by(|a, b| a.abs().total_cmp(&b.abs()))
                    .unwrap()
            })
            .collect();
        let (lo, hi) = errors
            .iter()
            .fold((f64::MAX, f64::MIN), |(lo, hi), e| (lo.min(*e), hi.max(*e)));
        // Within ± one envelope frame (11.6 ms) end to end, even for the
        // partial last region that can only be predicted.
        assert!(hi - lo < 0.015, "tracked error spread {:.1} ms", (hi - lo) * 1000.0);
        assert!(lo.abs().max(hi.abs()) < 0.020, "bias {:.1}…{:.1} ms", lo * 1000.0, hi * 1000.0);
    }

    #[test]
    fn a_kick_on_every_beat_reads_at_full_tempo_not_half() {
        let rate = 22_050;
        for bpm in [145.0, 150.0] {
            let t = detect(&clicks(bpm, 40.0, rate, 0.3), rate).unwrap();
            assert!((t.bpm - bpm).abs() < 0.5, "{bpm} read as {}", t.bpm);
        }
        // Drum & bass and hip-hop keep their half-time reading.
        let t = detect(&clicks(87.0, 40.0, rate, 0.3), rate).unwrap();
        assert!((t.bpm - 87.0).abs() < 0.5, "87 read as {}", t.bpm);
    }

    #[test]
    fn regional_tempo_fits_track_a_real_tempo_change() {
        let rate = 22_050;
        let mut x = clicks(120.0, 20.0, rate, 0.3);
        x.extend(clicks(126.0, 20.0, rate, 0.3));
        let (env, fr) = onset_envelope(&x, rate);
        let low = low_onset_envelope(&x, rate, 150.0);
        let scan = scan_from_envelopes(&env, &low, fr, rate);
        let selected = scan.selected.unwrap();
        let evidence = rhythm_evidence(
            &env,
            fr,
            rate,
            40.0,
            Some(selected),
            Some(0),
            Vec::new(),
            &scan.raw_scores,
        );
        let early = evidence
            .regions
            .iter()
            .filter(|region| region.end_s <= 18.0)
            .filter_map(|region| region.local_bpm)
            .collect::<Vec<_>>();
        let late = evidence
            .regions
            .iter()
            .filter(|region| region.start_s >= 22.0)
            .filter_map(|region| region.local_bpm)
            .collect::<Vec<_>>();
        assert!(
            !early.is_empty(),
            "no early regional tempo fit: {evidence:?}"
        );
        assert!(!late.is_empty(), "no late regional tempo fit: {evidence:?}");
        assert!(
            early.iter().all(|bpm| (*bpm - 120.0).abs() < 0.5),
            "{early:?}"
        );
        assert!(
            late.iter().all(|bpm| (*bpm - 126.0).abs() < 0.5),
            "{late:?}"
        );
        assert!(
            evidence.beats.len() > 80,
            "beat count must follow the faster passage"
        );
        assert!(evidence.beats.iter().any(|beat| {
            beat.time_s > 26.0 && (f64::from(beat.interval_s) - 60.0 / 126.0).abs() < 0.002
        }));
    }
}

/// Compact serialization of a beat sequence. Club tracks keep an exact
/// constant grid, so their beat times are the grid origin and step; only a
/// tracked variable-tempo sequence stores its times. Per-beat observations
/// are quantized when measured (10 µs offsets, 0.01 support), so the stored
/// form round-trips exactly. Reads the older verbose form too.
pub mod compact_beats {
    use super::BeatEvent;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    const NO_ONSET: i32 = i32::MIN;

    pub fn offset_q(seconds: f64) -> i32 {
        (seconds * 1e5).round() as i32
    }
    pub fn offset(q: i32) -> f32 {
        (f64::from(q) / 1e5) as f32
    }
    pub fn support_q(ratio: f64) -> u32 {
        (ratio.clamp(0.0, 100.0) * 100.0).round() as u32
    }
    pub fn support(q: u32) -> f32 {
        (f64::from(q) / 100.0) as f32
    }

    #[derive(Serialize, Deserialize)]
    struct Compact {
        /// Beat index of the first beat.
        first: i64,
        /// Constant grid: beat `i` sits at `origin + i * step`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
        /// Tracked sequence: explicit times and intervals.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        times: Option<Vec<f64>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        intervals: Option<Vec<f32>>,
        /// Beat index of a downbeat (bar phase), when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bar_origin: Option<i64>,
        offsets: Vec<i32>,
        support: Vec<u32>,
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Stored {
        Compact(Compact),
        Verbose(Vec<BeatEvent>),
    }

    fn compact(beats: &[BeatEvent]) -> Option<Compact> {
        let first = beats.first().map_or(0, |b| b.beat_index);
        if beats
            .iter()
            .enumerate()
            .any(|(k, b)| b.beat_index != first + k as i64)
        {
            return None;
        }
        let bar_origin = beats.first().and_then(|b| {
            Some(b.beat_index - (b.bar_index? * 4 + i64::from(b.beat_in_bar?)))
        });
        if beats.iter().any(|b| {
            let expected = bar_origin.map(|o| b.beat_index - o);
            b.bar_index != expected.map(|p| p.div_euclid(4))
                || b.beat_in_bar != expected.map(|p| p.rem_euclid(4) as u8)
        }) {
            return None;
        }
        let mut offsets = Vec::with_capacity(beats.len());
        let mut supports = Vec::with_capacity(beats.len());
        for b in beats {
            let q = b.onset_offset_s.map(|o| offset_q(f64::from(o)));
            if q.is_some_and(|q| q == NO_ONSET || Some(offset(q)) != b.onset_offset_s)
                || support(support_q(f64::from(b.onset_support))) != b.onset_support
            {
                return None;
            }
            offsets.push(q.unwrap_or(NO_ONSET));
            supports.push(support_q(f64::from(b.onset_support)));
        }
        // A constant grid when every time is exactly origin + index * step
        // (the formula the analysis used to place them).
        let constant = beats.windows(2).next().map(|w| {
            let step = w[1].time_s - w[0].time_s;
            (w[0].time_s - w[0].beat_index as f64 * step, step)
        });
        let constant = constant.filter(|(origin, step)| {
            beats.iter().all(|b| {
                origin + b.beat_index as f64 * step == b.time_s && b.interval_s == *step as f32
            })
        });
        Some(match constant {
            Some((origin, step)) => Compact {
                first,
                origin: Some(origin),
                step: Some(step),
                times: None,
                intervals: None,
                bar_origin,
                offsets,
                support: supports,
            },
            None => Compact {
                first,
                origin: None,
                step: None,
                times: Some(beats.iter().map(|b| b.time_s).collect()),
                intervals: Some(beats.iter().map(|b| b.interval_s).collect()),
                bar_origin,
                offsets,
                support: supports,
            },
        })
    }

    fn expand(c: Compact) -> Result<Vec<BeatEvent>, String> {
        let n = c.offsets.len();
        if c.support.len() != n
            || c.times.as_ref().is_some_and(|t| t.len() != n)
            || c.intervals.as_ref().is_some_and(|t| t.len() != n)
        {
            return Err("beat arrays differ in length".into());
        }
        let constant = c.origin.zip(c.step);
        if constant.is_none() && (c.times.is_none() || c.intervals.is_none()) && n > 0 {
            return Err("beat times missing".into());
        }
        Ok((0..n)
            .map(|k| {
                let beat_index = c.first + k as i64;
                let (time_s, interval_s) = match constant {
                    Some((origin, step)) => (origin + beat_index as f64 * step, step as f32),
                    None => (c.times.as_ref().unwrap()[k], c.intervals.as_ref().unwrap()[k]),
                };
                let bar = c.bar_origin.map(|o| beat_index - o);
                BeatEvent {
                    time_s,
                    beat_index,
                    interval_s,
                    bar_index: bar.map(|p| p.div_euclid(4)),
                    beat_in_bar: bar.map(|p| p.rem_euclid(4) as u8),
                    onset_offset_s: (c.offsets[k] != NO_ONSET).then(|| offset(c.offsets[k])),
                    onset_support: support(c.support[k]),
                }
            })
            .collect())
    }

    pub fn serialize<S: Serializer>(beats: &Vec<BeatEvent>, s: S) -> Result<S::Ok, S::Error> {
        match compact(beats) {
            Some(c) => c.serialize(s),
            None => beats.serialize(s),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<BeatEvent>, D::Error> {
        match Stored::deserialize(d)? {
            Stored::Compact(c) => expand(c).map_err(serde::de::Error::custom),
            Stored::Verbose(v) => Ok(v),
        }
    }
}
