//! The traits Derived Material depends on but does not own.
//!
//! Audio access and stretching are the engine's: it knows the graph rate, the
//! PipeWire buffers and Rubber Band. Here they are interfaces so this crate
//! stays free of engine dependencies and its tests can run on synthetic audio.
//! A transform the engine cannot perform is refused, never faked.

use crate::model::SourceRef;

/// Interleaved stereo `f32` at the graph rate; `len()` is frames × 2.
pub type Audio = Vec<f32>;

/// Decodes a track (or one stem of it) to the graph rate.
pub trait SourceAudio: Send + Sync {
    /// The whole source as interleaved stereo at `rate`. `source.stem` picks a
    /// stem when it is set; otherwise this is the full mix.
    fn decode(&self, source: &SourceRef, rate: u32) -> Result<Audio, String>;
}

/// Time and pitch, from the engine's Rubber Band. Without it the engine never
/// registers a `Stretch`, and `TimeStretch`/`PitchShift` are refused.
pub trait Stretch: Send + Sync {
    /// Play `audio` at `ratio`× its speed (1.0 = unchanged). The length
    /// changes by `1/ratio`; pitch is preserved.
    fn time_stretch(&self, audio: &[f32], rate: u32, ratio: f64) -> Result<Audio, String>;

    /// Shift pitch by `semitones` without changing tempo.
    fn pitch_shift(&self, audio: &[f32], rate: u32, semitones: f64) -> Result<Audio, String>;
}
