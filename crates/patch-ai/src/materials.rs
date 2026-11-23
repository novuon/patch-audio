//! Material regions (the shared types).
use crate::TrackInfo;
use patch_core::stems::Availability;
use serde::{Deserialize, Serialize};

const MAX_ACTIVITY_HZ: u32 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialRole {
    Vocals,
    Drums,
    Bass,
    Other,
}

impl MaterialRole {
    pub fn stem_id(self) -> &'static str {
        match self {
            Self::Vocals => "vocals",
            Self::Drums => "drums",
            Self::Bass => "bass",
            Self::Other => "other",
        }
    }
}

/// A source-referenced region from a prepared stem. `raw_match` is an
/// activity/duration ranking feature, never a compatibility probability.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MaterialRegion {
    pub track_id: String,
    pub path: String,
    pub title: String,
    pub artist: String,
    pub stem_set_id: String,
    pub role: MaterialRole,
    pub start_s: f64,
    pub end_s: f64,
    pub active_fraction: f32,
    pub raw_match: f32,
    pub bpm: Option<f64>,
    pub key: Option<patch_analysis::key::Key>,
}

/// A material candidate ranked against a proposed performance tempo/key.
/// `raw_score` is a deterministic heuristic, not a success probability.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompatibleMaterial {
    pub region: MaterialRegion,
    /// A complete bar-aligned window, absent when the region lacks reliable
    /// current beat/bar evidence or is too short for the requested phrase.
    #[serde(default)]
    pub phrase: Option<AlignedMaterialRegion>,
    pub tempo_fit: f32,
    pub harmonic_fit: Option<f32>,
    pub raw_score: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AlignedMaterialRegion {
    pub start_s: f64,
    pub end_s: f64,
    pub start_beat_index: i64,
    pub beat_count: u16,
    /// Analyzer score for downbeat evidence, not a calibrated probability.
    pub raw_grid_support: f32,
}

/// Fraction of activity bins above the stem's leakage floor in a source-time
/// interval. Returns `None` when there is no current, non-human-owned evidence.
/// This is measured activity, not a vocal-presence probability.
pub fn activity_fraction(
    track: &TrackInfo,
    role: MaterialRole,
    start_s: f64,
    end_s: f64,
) -> Option<f32> {
    let id = role.stem_id();
    let audible = !track.stems.muted.iter().any(|stem| stem == id)
        && (track.stems.soloed.is_empty() || track.stems.soloed.iter().any(|stem| stem == id))
        && track.stems.gains.get(id).copied().unwrap_or(1.0) > 0.05;
    if !start_s.is_finite()
        || !end_s.is_finite()
        || start_s < 0.0
        || end_s <= start_s
        || track.stems.availability != Availability::Ready
        || !track.stems.available.iter().any(|stem| stem == id)
        || track.stems.human_owned.iter().any(|stem| stem == id)
        || !audible
    {
        return None;
    }
    let values = track.stems.activity.get(id)?;
    let hz = track
        .stems
        .activity_hz
        .get(id)
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
        .filter(|value| value.is_finite())
        .fold(0.0f32, f32::max);
    let floor = 0.01f32.max(peak * 0.1);
    let active = window
        .iter()
        .filter(|value| value.is_finite() && **value >= floor)
        .count();
    Some(active as f32 / window.len() as f32)
}
