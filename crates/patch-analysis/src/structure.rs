//! Cheap, coarse structural evidence derived from Level 1 energy and beat scans.
//! Scores are raw boundary salience, not probabilities or semantic labels.

use crate::tempo::BeatEvent;
use serde::{Deserialize, Serialize};

pub const ANALYZER_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructureEvidence {
    pub analyzer_version: u32,
    pub boundaries: Vec<BoundaryEvidence>,
    pub sections: Vec<CoarseSection>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoundaryEvidence {
    pub time_s: f64,
    /// Raw, uncalibrated salience combining relative energy and onset changes.
    pub raw_score: f32,
    pub energy_delta: f32,
    pub onset_density_delta: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoarseSection {
    pub start_s: f64,
    pub end_s: f64,
    pub mean_energy: f32,
    pub onset_density: f32,
}

/// Per-second energy in three bands (below 200 Hz, 200 Hz-2 kHz, above),
/// of interleaved stereo at `rate`: a drop that adds pads and leads over
/// the same kick and bass changes the balance more than the loudness.
pub fn band_energy(stereo: &[f32], rate: u32) -> Vec<[f32; 3]> {
    let pole = |hz: f32| 1.0 - (-std::f32::consts::TAU * hz / rate as f32).exp();
    let (a_lo, a_mid) = (pole(200.0), pole(2000.0));
    let (mut lo, mut mid) = (0.0f32, 0.0f32);
    stereo
        .chunks(rate as usize * 2)
        .map(|second| {
            let mut e = [0.0f64; 3];
            for f in second.chunks_exact(2) {
                let x = (f[0] + f[1]) * 0.5;
                lo += a_lo * (x - lo);
                mid += a_mid * (x - mid);
                let bands = [lo, mid - lo, x - mid];
                for (k, b) in bands.iter().enumerate() {
                    e[k] += f64::from(b * b);
                }
            }
            let n = (second.len() / 2).max(1) as f64;
            e.map(|v| (v / n).sqrt() as f32)
        })
        .collect()
}

/// Finds broad changes in adjacent four-second windows: loudness, onset
/// density and (with `bands`) the balance of lows, mids and highs; each
/// boundary then moves to the nearest downbeat when the grid has bars.
/// This intentionally favors speed and modest storage over semantic labels
/// such as verse/chorus.
pub fn analyze(
    energy: &[u8],
    bands: &[[f32; 3]],
    beats: &[BeatEvent],
    duration_s: f64,
) -> StructureEvidence {
    let seconds = duration_s.ceil().max(0.0) as usize;
    let mut onset = vec![0.0f32; seconds];
    let mut counts = vec![0u16; seconds];
    for beat in beats {
        let i = beat.time_s.floor() as usize;
        if i < seconds {
            counts[i] = counts[i].saturating_add(1);
            if beat.onset_offset_s.is_some() {
                onset[i] += 1.0;
            }
        }
    }
    for (o, n) in onset.iter_mut().zip(counts) {
        if n > 0 {
            *o /= n as f32;
        }
    }

    let mut candidates = Vec::new();
    if energy.len() >= 10 && seconds >= 10 {
        for t in 5..energy.len().min(seconds.saturating_sub(4)) {
            let e_before = mean_u8(energy, t.saturating_sub(4), t).max(2.0);
            let e_after = mean_u8(energy, t, (t + 4).min(energy.len())).max(2.0);
            let o_before = mean_f32(&onset, t.saturating_sub(4), t);
            let o_after = mean_f32(&onset, t, (t + 4).min(onset.len()));
            let de = ((e_after / e_before).ln()).abs();
            let do_ = ((o_after + 0.08) / (o_before + 0.08)).ln().abs();
            // Balance: how the share of each band changes (loudness apart).
            let db = if bands.len() >= t + 4 {
                let share = |a: usize, b: usize| {
                    let mut m = [0.0f32; 3];
                    for v in &bands[a..b] {
                        for k in 0..3 {
                            m[k] += v[k];
                        }
                    }
                    let total = m.iter().sum::<f32>().max(1e-6);
                    m.map(|v| v / total)
                };
                let (b0, b1) = (share(t.saturating_sub(4), t), share(t, t + 4));
                (0..3).map(|k| (b1[k] - b0[k]).abs()).sum::<f32>()
            } else {
                0.0
            };
            let score = de + 0.35 * do_ + BALANCE_WEIGHT * db;
            if score >= 0.22 {
                candidates.push(BoundaryEvidence {
                    time_s: t as f64,
                    raw_score: score,
                    energy_delta: de,
                    onset_density_delta: do_,
                });
            }
        }
    }
    let mut boundaries: Vec<BoundaryEvidence> = Vec::new();
    for c in candidates {
        if let Some(prev) = boundaries.last_mut().filter(|p| c.time_s - p.time_s < 8.0) {
            if c.raw_score > prev.raw_score {
                *prev = c;
            }
        } else {
            boundaries.push(c);
        }
    }
    // Sections change on bars: move each boundary to the nearest downbeat
    // (within a bar), when the grid knows its bars.
    let downbeats: Vec<f64> = beats
        .iter()
        .filter(|b| b.beat_in_bar == Some(0))
        .map(|b| b.time_s)
        .collect();
    if downbeats.len() >= 2 {
        let bar = (downbeats[downbeats.len() - 1] - downbeats[0]) / (downbeats.len() - 1) as f64;
        for b in &mut boundaries {
            if let Some(d) = downbeats
                .iter()
                .copied()
                .min_by(|x, y| (x - b.time_s).abs().total_cmp(&(y - b.time_s).abs()))
                .filter(|d| (d - b.time_s).abs() <= bar)
            {
                b.time_s = d;
            }
        }
        boundaries.dedup_by(|a, b| (a.time_s - b.time_s).abs() < 1e-6);
    }
    let mut cuts = Vec::with_capacity(boundaries.len() + 2);
    cuts.push(0.0);
    cuts.extend(boundaries.iter().map(|b| b.time_s));
    cuts.push(duration_s.max(0.0));
    let sections = cuts
        .windows(2)
        .filter(|&w| w[1] > w[0]).map(|w| {
                let start = w[0].floor() as usize;
                let end = w[1].ceil() as usize;
                CoarseSection {
                    start_s: w[0],
                    end_s: w[1],
                    mean_energy: mean_u8(energy, start, end),
                    onset_density: mean_f32(&onset, start, end),
                }
            })
        .collect();
    StructureEvidence {
        analyzer_version: ANALYZER_VERSION,
        boundaries,
        sections,
    }
}

/// Weight of the band-balance change (sum of share changes, 0-2) in a
/// boundary's score. `patch-bench --section-eval` (synthetic sections): 32 of
/// 40 boundaries without it (every drop that adds pads and leads missed),
/// 35 at 0.5, 40 at 1.0; `--section-real` (20 library tracks): 1.80
/// boundaries a minute without, 2.09 at 0.5, 2.47 at 1.0. 0.5 takes most
/// drops for few extra cuts until real section labels can settle it.
const BALANCE_WEIGHT: f32 = 0.5;

fn mean_u8(v: &[u8], a: usize, b: usize) -> f32 {
    let x = &v[a.min(v.len())..b.min(v.len()).max(a.min(v.len()))];
    if x.is_empty() {
        0.0
    } else {
        x.iter().map(|x| *x as f32).sum::<f32>() / x.len() as f32
    }
}
fn mean_f32(v: &[f32], a: usize, b: usize) -> f32 {
    let x = &v[a.min(v.len())..b.min(v.len()).max(a.min(v.len()))];
    if x.is_empty() {
        0.0
    } else {
        x.iter().sum::<f32>() / x.len() as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_large_coarse_energy_change_without_claiming_semantics() {
        let mut energy = vec![25; 64];
        energy[32..].fill(150);
        let out = analyze(&energy, &[], &[], 64.0);
        assert!(
            out.boundaries
                .iter()
                .any(|b| (b.time_s - 32.0).abs() <= 1.0)
        );
        assert!(out.sections.len() >= 2);
        assert!(out.boundaries.iter().all(|b| b.raw_score > 0.0));
    }
    #[test]
    fn steady_track_has_no_false_coarse_boundaries() {
        let out = analyze(&[70; 90], &[], &[], 90.0);
        assert!(out.boundaries.is_empty());
        assert_eq!(out.sections.len(), 1);
    }
}
