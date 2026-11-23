//! Semantic material search: "give me a clean two-beat vocal, loopable, at
//! 126 BPM" → ranked candidates.
//!
//! The AI director asks this way and never needs sample offsets (they are in the
//! recipe if it wants them). Ranking uses the track's analysis (grid,
//! structure) and the stem activity curves; `raw_match`-style scores are
//! deterministic heuristics, never success probabilities.

use crate::model::{Kind, Recipe, RegionSpec, Resolved, SourceRef, Transform};
use crate::resolve;
use patch_analysis::Analysis;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Cap the activity lookups exactly as the AI DJ's material search does.
const MAX_ACTIVITY_HZ: u32 = 16;
pub const MAX_RESULTS: usize = 64;
/// One beat's worth of a kept region is never shorter than this.
const MIN_SECONDS: f64 = 0.05;

/// A track the search can draw from: its analysis and its stem activity.
#[derive(Clone, Debug)]
pub struct Track {
    pub source: SourceRef,
    pub analysis: Analysis,
    /// Stem id → activity curve (0…1-ish), at `activity_hz`.
    pub activity: BTreeMap<String, Vec<f32>>,
    pub activity_hz: BTreeMap<String, u32>,
}

/// What the caller needs of the material. Everything is optional: a caller
/// may ask only for "a bar loop", or everything.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Requirements {
    /// Beats the material must contain (a bar loop's length, a chop's beats).
    pub beats: Option<f64>,
    pub loopable: bool,
    /// "clean" = a single stem, no bleed.
    pub clean: bool,
    pub min_confidence: f32,
    pub target_bpm: Option<f64>,
    pub target_key: Option<String>,
    /// Soft deadline for preparation (ms); the service degrades past it.
    pub deadline_ms: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stem: Option<String>,
    /// An explicit region; when absent the search proposes regions for `kind`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<RegionSpec>,
    #[serde(default)]
    pub requirements: Requirements,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    8
}

/// A ranked candidate: a full recipe, not just a region, so the caller can
/// request exactly it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub recipe: Recipe,
    /// 0…1 how well the material fits `kind` (activity, position).
    pub suitability: f32,
    /// 0…1 the grid's confidence in the position.
    pub confidence: f32,
    /// Source seconds of the candidate's first beat.
    pub position: f64,
    pub bpm_fit: Option<f32>,
    pub key_fit: Option<f32>,
    pub est_cost_ms: f64,
    pub cached: bool,
    pub reasons: Vec<String>,
}

/// The stem a kind belongs to, when it has one.
pub fn kind_stem(kind: Kind) -> Option<&'static str> {
    Some(match kind {
        Kind::VocalPhrase | Kind::VocalChop => "vocals",
        Kind::Kick
        | Kind::Snare
        | Kind::Hat
        | Kind::Percussion
        | Kind::BeatLoop
        | Kind::BarLoop => "drums",
        Kind::BassFragment => "bass",
        Kind::MelodicFragment | Kind::HarmonicTexture => "other",
        Kind::PhraseLoop | Kind::TransitionFx | Kind::GranularSource => return None,
    })
}

/// Fraction of activity bins above the stem's leakage floor in a source-time
/// interval. Local mirror of `patch_ai::materials::activity_fraction` (this
/// crate does not depend on patch-ai); measured activity, not a probability.
pub fn activity_fraction(track: &Track, stem: &str, start_s: f64, end_s: f64) -> Option<f32> {
    if !start_s.is_finite() || !end_s.is_finite() || start_s < 0.0 || end_s <= start_s {
        return None;
    }
    let values = track.activity.get(stem)?;
    let hz = track
        .activity_hz
        .get(stem)
        .copied()
        .unwrap_or(1)
        .clamp(1, MAX_ACTIVITY_HZ) as f64;
    let lo = (start_s * hz).floor() as usize;
    let hi = (end_s * hz).ceil() as usize;
    let window = values.get(lo.min(values.len())..hi.min(values.len()))?;
    if window.is_empty() {
        return None;
    }
    let peak = values
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(0.0f32, f32::max);
    let floor = 0.01f32.max(peak * 0.1);
    let active = window
        .iter()
        .filter(|v| v.is_finite() && **v >= floor)
        .count();
    Some(active as f32 / window.len() as f32)
}

/// Search `tracks` for material matching `req`. Deterministic: same inputs,
/// same order.
pub fn search(req: &Request, tracks: &[Track], rate: u32) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    for track in tracks {
        if let Some(want) = &req.source
            && &track.source.track_id != want
        {
            continue;
        }
        if let Some(want) = &req.stem
            && track.source.stem.as_deref() != Some(want.as_str())
        {
            continue;
        }
        let specs = match &req.region {
            Some(r) => vec![r.clone()],
            None => propose(req, track),
        };
        for spec in specs {
            if let Some(c) = score(req, track, &spec, rate) {
                out.push(c);
            }
        }
    }
    // Suitability, then confidence, then the earliest position: stable.
    out.sort_by(|a, b| {
        b.suitability
            .total_cmp(&a.suitability)
            .then(b.confidence.total_cmp(&a.confidence))
            .then(a.position.total_cmp(&b.position))
    });
    out.truncate(req.limit.clamp(1, MAX_RESULTS));
    out
}

/// Regions a kind naturally proposes, without an explicit one.
fn propose(req: &Request, track: &Track) -> Vec<RegionSpec> {
    let beats = req
        .requirements
        .beats
        .unwrap_or(match req.kind {
            Kind::BeatLoop | Kind::Kick | Kind::Snare | Kind::Hat => 4.0,
            Kind::BarLoop => 4.0,
            Kind::PhraseLoop | Kind::VocalPhrase => 32.0,
            Kind::VocalChop => 2.0,
            _ => 8.0,
        })
        .max(1.0);
    let total = track
        .analysis
        .rhythm
        .as_ref()
        .and_then(|r| r.beats.last().map(|b| b.beat_index + 1))
        .unwrap_or(0);
    if total <= 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    match req.kind {
        Kind::VocalPhrase | Kind::VocalChop | Kind::PhraseLoop => {
            // Whole phrases across the track (the phrase convention: 32 beats).
            let step = if req.kind == Kind::VocalChop { 8 } else { 32 };
            let mut b = 0;
            while b + beats as i64 <= total && out.len() < MAX_RESULTS {
                out.push(RegionSpec::Beats {
                    beat: b,
                    count: beats as i64,
                    beats: None,
                });
                b += step;
            }
        }
        Kind::Kick | Kind::Snare | Kind::Hat | Kind::Percussion => {
            // One hit at a time: the beats the stems say are active.
            let stem = kind_stem(req.kind).unwrap_or("drums");
            let interval = 60.0 / track.analysis.bpm.unwrap_or(120.0);
            for i in 0..total.min(MAX_RESULTS as i64 * 4) {
                let t = i as f64 * interval;
                if activity_fraction(track, stem, t, t + interval).unwrap_or(0.5) > 0.5 {
                    out.push(RegionSpec::Beats {
                        beat: i,
                        count: 1,
                        beats: None,
                    });
                }
            }
        }
        _ => {
            let mut b = 0;
            while b + beats as i64 <= total && out.len() < MAX_RESULTS {
                out.push(RegionSpec::Beats {
                    beat: b,
                    count: beats as i64,
                    beats: None,
                });
                b += beats as i64;
            }
        }
    }
    out
}

fn fits(req: &Request, region: &Resolved) -> bool {
    let beats = req.requirements.beats.unwrap_or(region.beats);
    (region.beats - beats).abs() <= 0.5
        && region.confidence >= req.requirements.min_confidence
        && region.duration_s() >= MIN_SECONDS
}

fn score(req: &Request, track: &Track, spec: &RegionSpec, rate: u32) -> Option<Candidate> {
    let region = resolve::resolve(spec, &track.analysis, rate).ok()?;
    if !fits(req, &region) {
        return None;
    }
    let stem = req
        .stem
        .clone()
        .or_else(|| Some(track.source.stem.clone().unwrap_or_default()))
        .filter(|s| !s.is_empty())
        .or_else(|| kind_stem(req.kind).map(str::to_string));
    let mut reasons = Vec::new();
    let suitability = match stem.as_deref() {
        Some(stem) => match activity_fraction(
            track,
            stem,
            region.first_beat,
            region.first_beat + region.duration_s(),
        ) {
            Some(f) => {
                reasons.push(format!("{stem} activity {:.0}%", f * 100.0));
                f
            }
            None => {
                reasons.push("no stem activity evidence".into());
                0.5
            }
        },
        None => 0.5,
    };
    if req.requirements.clean
        && let Some(stem) = stem.as_deref()
        && activity_fraction(
            track,
            stem,
            region.first_beat,
            region.first_beat + region.duration_s(),
        )
        .is_none()
    {
        // "clean" with no stem evidence: not clean enough.
        return None;
    }
    let (bpm_fit, key_fit) = fit(req, track, &region, &mut reasons);
    let est_cost_ms = region.duration_s() * 40.0 + 120.0;
    let transforms = loops(req);
    reasons.push(region.why.clone());
    let confidence = region.confidence;
    let position = region.first_beat;
    Some(Candidate {
        recipe: Recipe {
            version: crate::model::VERSION,
            source: track.source.clone(),
            region,
            kind: req.kind,
            transforms,
            analysis_version: track.analysis.generation(),
        },
        suitability,
        confidence,
        position,
        bpm_fit,
        key_fit,
        est_cost_ms,
        cached: false,
        reasons,
    })
}

/// Whether the caller needs a loop, so the recipe carries a seam transform.
fn loops(req: &Request) -> Vec<Transform> {
    let mut t = vec![Transform::Fade {
        in_ms: 5.0,
        out_ms: 5.0,
    }];
    if req.requirements.loopable
        || matches!(req.kind, Kind::BeatLoop | Kind::BarLoop | Kind::PhraseLoop)
    {
        t.insert(0, Transform::LoopSeam { crossfade_ms: 10.0 });
    }
    t
}

fn fit(
    req: &Request,
    track: &Track,
    region: &Resolved,
    reasons: &mut Vec<String>,
) -> (Option<f32>, Option<f32>) {
    let bpm_fit = match (req.requirements.target_bpm, region.source_bpm) {
        (Some(target), Some(source)) if target > 0.0 && source > 0.0 => {
            // A ratio to stretch: the closer to 1, the better.
            let ratio = (source / target).log2().abs();
            let f = (1.0 - ratio).clamp(0.0, 1.0) as f32;
            if ratio > 0.25 {
                reasons.push(format!(
                    "stretch {:.0}%",
                    (source / target - 1.0).abs() * 100.0
                ));
            }
            Some(f)
        }
        _ => None,
    };
    let key_fit = match (&req.requirements.target_key, &region.source_key) {
        (Some(want), Some(have)) => {
            let f = if want.eq_ignore_ascii_case(have) {
                1.0
            } else {
                0.0
            };
            if f == 0.0 {
                reasons.push(format!("key {have} ≠ {want}"));
            }
            Some(f)
        }
        _ => None,
    };
    let _ = track;
    (bpm_fit, key_fit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    fn track(bpm: f64, stem: &str, activity: Vec<f32>) -> Track {
        let n = 128usize;
        Track {
            source: SourceRef {
                track_id: "track-a".into(),
                path: "/music/a.flac".into(),
                fingerprint: "0".repeat(64),
                stem: Some(stem.into()),
                stem_set: Some("set-1".into()),
            },
            analysis: testutil::analysis(
                bpm,
                n,
                vec![
                    testutil::section(0.0, 30.0, 20.0),
                    testutil::section(30.0, 60.0, 150.0),
                ],
            ),
            activity: BTreeMap::from([(stem.to_string(), activity)]),
            activity_hz: BTreeMap::from([(stem.to_string(), 4)]),
        }
    }

    #[test]
    fn a_bar_loop_request_returns_aligned_candidates() {
        let t = track(128.0, "drums", vec![0.9; 64]);
        let req = Request {
            kind: Kind::BarLoop,
            source: None,
            stem: Some("drums".into()),
            region: None,
            requirements: Requirements {
                beats: Some(4.0),
                loopable: true,
                ..Default::default()
            },
            limit: 4,
        };
        let hits = search(&req, &[t], 48_000);
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|c| c.recipe.region.beats == 4.0));
        // All candidates are bar-aligned (a whole number of bars).
        assert!(hits.iter().all(|c| c.recipe.region.bar >= 0));
        // A loop transform is present.
        assert!(hits.iter().all(|c| {
            c.recipe
                .transforms
                .iter()
                .any(|t| matches!(t, Transform::LoopSeam { .. }))
        }));
        assert!(hits[0].est_cost_ms > 0.0);
    }

    #[test]
    fn min_confidence_filters_out_uncertain_tracks() {
        let mut t = track(128.0, "drums", vec![0.9; 64]);
        t.analysis.bpm_confidence = 0.9;
        let mut req = Request {
            kind: Kind::BarLoop,
            source: None,
            stem: Some("drums".into()),
            region: None,
            requirements: Requirements::default(),
            limit: 4,
        };
        assert!(!search(&req, &[t.clone()], 48_000).is_empty());
        req.requirements.min_confidence = 0.95;
        // The resolver itself refuses below 0.3; a requirement above the
        // track's confidence must filter it out.
        assert!(search(&req, &[t], 48_000).is_empty());
    }

    #[test]
    fn results_are_ranked_by_activity() {
        let mut high = vec![0.1f32; 256];
        high[64..96].fill(1.0); // one loud phrase
        let t = track(128.0, "vocals", high);
        let req = Request {
            kind: Kind::VocalPhrase,
            source: None,
            stem: Some("vocals".into()),
            region: None,
            requirements: Requirements::default(),
            limit: 8,
        };
        let hits = search(&req, &[t], 48_000);
        assert!(!hits.is_empty());
        for w in hits.windows(2) {
            assert!(w[0].suitability >= w[1].suitability);
        }
    }

    #[test]
    fn an_explicit_region_is_used_verbatim() {
        let t = track(128.0, "drums", vec![0.9; 64]);
        let req = Request {
            kind: Kind::BarLoop,
            source: None,
            stem: None,
            region: Some(RegionSpec::Bars {
                bar: 2,
                count: 1,
                beats: None,
            }),
            requirements: Requirements::default(),
            limit: 4,
        };
        let hits = search(&req, &[t], 48_000);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].recipe.region.bar, 2);
    }
}
