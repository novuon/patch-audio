//! Track selection's shared measures.
use crate::{Context, TrackInfo};

/// Tempo reach: beyond this ratio (half/double time included) only a cut
/// works.
pub const TEMPO_REACH: f64 = 0.08;
/// The energy step a build or release aims for (relative).
pub const ENERGY_STEP: f32 = 0.15;
/// How far from that target a track still fits at all (relative).
pub const ENERGY_REACH: f32 = 0.4;
/// Shorter than this is a clip, not a track to mix into.
pub const MIN_TRACK_S: f64 = 90.0;

/// Tempo ratio distance to `bpm`, half/double time included.
pub fn tempo_distance(bpm: f64, other: f64) -> f64 {
    [0.5, 1.0, 2.0]
        .iter()
        .map(|m| (bpm * m / other - 1.0).abs())
        .fold(f64::MAX, f64::min)
}

/// (score 0…1, reasons), or None when the track can't follow at all.
/// The factors behind a score, each 0…1 (what an AI view shows), plus the
/// energy change it brings.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Factors {
    pub tempo: f32,
    pub harmony: f32,
    pub energy: f32,
    pub rating: f32,
    pub energy_delta: f32,
    /// 0…1: how well its beat can run under another (its measured grid).
    #[serde(default)]
    pub grid: f32,
    /// Raw groove compatibility index; neutral when regional evidence is absent.
    #[serde(default = "neutral_groove")]
    pub groove: f32,
    /// Raw local pitch-class overlap across the proposed mix regions.
    #[serde(default = "neutral_groove")]
    pub regional_harmony: f32,
}

fn neutral_groove() -> f32 {
    0.5
}

impl Default for Factors {
    fn default() -> Self {
        Self {
            tempo: 0.0,
            harmony: 0.0,
            energy: 0.0,
            rating: 0.0,
            energy_delta: 0.0,
            grid: 0.0,
            groove: 0.5,
            regional_harmony: 0.5,
        }
    }
}

/// A track's fit after what plays: not in this build.
pub fn score_factors(_ctx: &Context, _t: &TrackInfo) -> Option<(f32, Vec<String>, Factors)> {
    None
}
