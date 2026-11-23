//! Model-neutral stem preparation and playback contracts. No audio or I/O.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub const CANONICAL: [&str; 4] = ["vocals", "drums", "bass", "other"];
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    #[default]
    Unavailable,
    Queued,
    Preparing,
    Ready,
    Failed,
    Stale,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StemChunk {
    pub start_frame: u64,
    pub frames: u64,
    pub cache_ref: String,
    pub checksum: String,
    pub bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StemArtifact {
    #[serde(default)]
    pub chunks: Vec<StemChunk>,
    pub id: String,
    pub cache_ref: String,
    pub codec: String,
    pub checksum: String,
    pub bytes: u64,
    pub frames: u64,
    pub start_frame: u64,
    pub peak: f32,
    /// Number of RMS activity values per second. Older cache records default
    /// to the original one-second summary.
    #[serde(default = "default_activity_hz")]
    pub activity_hz: u32,
    pub activity: Vec<f32>,
    /// The same, below 120 Hz (for drums: the kick), from stem pipeline 4.
    #[serde(default)]
    pub low_activity: Vec<f32>,
    pub waveform: Vec<f32>,
}

fn default_activity_hz() -> u32 {
    1
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StemSet {
    pub version: u32,
    pub id: String,
    pub source_hash: String,
    pub track_id: String,
    pub model_id: String,
    pub model_version: String,
    pub weights_digest: String,
    pub profile: String,
    pub pipeline_version: u32,
    pub backend: String,
    pub sample_rate: u32,
    pub frames: u64,
    pub duration: f64,
    pub layout: Vec<String>,
    pub stems: BTreeMap<String, StemArtifact>,
    pub created: u64,
}
impl StemSet {
    pub fn playback_ready(&self) -> bool {
        !self.layout.is_empty()
            && self.layout.len() <= 8
            && self.layout.iter().all(|id| {
                valid_id(id)
                    && self.stems.get(id).is_some_and(|s| {
                        s.start_frame == 0 && s.frames == self.frames && !s.cache_ref.is_empty()
                    })
            })
    }
    pub fn complete(&self) -> bool {
        CANONICAL.iter().all(|id| {
            self.stems
                .get(*id)
                .is_some_and(|s| s.start_frame == 0 && s.frames == self.frames)
        })
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StemContext {
    pub availability: Availability,
    pub set_id: Option<String>,
    pub available: Vec<String>,
    pub gains: BTreeMap<String, f32>,
    pub muted: Vec<String>,
    pub soloed: Vec<String>,
    pub activity: BTreeMap<String, Vec<f32>>,
    #[serde(default)]
    pub activity_hz: BTreeMap<String, u32>,
    pub human_owned: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "parameter", content = "value", rename_all = "snake_case")]
pub enum StemValue {
    Gain(f32),
    Mute(bool),
    Solo(bool),
    /// A knob, as the deck's stem knob and a controller's: −1 mutes the
    /// stem, 0 (with a small detent) is its own level, +1 doubles it.
    Knob(f32),
}
impl StemValue {
    pub fn validate(&self) -> Result<(), String> {
        if let Self::Gain(g) = self
            && (!g.is_finite() || !(0.0..=2.0).contains(g))
        {
            return Err("Stem gain must be finite and within 0–2".into());
        }
        if let Self::Knob(k) = self
            && (!k.is_finite() || !(-1.0..=1.0).contains(k))
        {
            return Err("Stem knob must be finite and within −1 … 1".into());
        }
        Ok(())
    }
    /// The gain it sets, if it sets one.
    pub fn gain(&self) -> Option<f32> {
        match *self {
            Self::Gain(g) => Some(g),
            Self::Knob(k) => Some(if k.abs() < 0.04 { 1.0 } else { 1.0 + k }),
            Self::Mute(_) | Self::Solo(_) => None,
        }
    }
}
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
