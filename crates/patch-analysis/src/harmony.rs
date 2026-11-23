//! Compact ordinary-track local chroma evidence from the key-analysis pass.
//!
//! These are pitch-class energy summaries, not chord labels or calibrated
//! probabilities. A frame's activity measures how much of its spectral window
//! had usable tonal peaks.

use serde::{Deserialize, Serialize};

pub const ANALYZER_VERSION: u32 = 1;
pub const WINDOW_SECONDS: f64 = 8.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarmonicProfile {
    pub analyzer_version: u32,
    pub window_seconds: f64,
    pub frames: Vec<HarmonicFrame>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HarmonicFrame {
    pub start_s: f64,
    pub end_s: f64,
    /// Normalized C-first pitch-class evidence. Zero means no stable peaks.
    pub chroma: [f32; 12],
    /// Fraction of STFT windows in this region containing tonal peaks.
    pub tonal_activity: f32,
}

impl HarmonicProfile {
    pub(crate) fn from_regions(duration_s: f64, regions: Vec<([f64; 12], usize, usize)>) -> Self {
        let frames = regions
            .into_iter()
            .enumerate()
            .map(|(index, (sum, active, count))| {
                let total = sum.iter().sum::<f64>();
                let chroma = if total > 1e-12 {
                    sum.map(|v| (v / total) as f32)
                } else {
                    [0.0; 12]
                };
                let start_s = index as f64 * WINDOW_SECONDS;
                HarmonicFrame {
                    start_s,
                    end_s: (start_s + WINDOW_SECONDS).min(duration_s),
                    chroma,
                    tonal_activity: if count > 0 {
                        (active as f32 / count as f32).clamp(0.0, 1.0)
                    } else {
                        0.0
                    },
                }
            })
            .filter(|frame| frame.end_s > frame.start_s)
            .collect();
        Self {
            analyzer_version: ANALYZER_VERSION,
            window_seconds: WINDOW_SECONDS,
            frames,
        }
    }

    pub fn validate(&self, duration_s: f64) -> Result<(), String> {
        if self.analyzer_version != ANALYZER_VERSION
            || (self.window_seconds - WINDOW_SECONDS).abs() > 1e-9
            || !duration_s.is_finite()
            || duration_s <= 0.0
        {
            return Err("invalid local harmony provenance or duration".into());
        }
        let mut previous_end = 0.0;
        for frame in &self.frames {
            if !frame.start_s.is_finite()
                || !frame.end_s.is_finite()
                || frame.start_s < previous_end
                || frame.end_s <= frame.start_s
                || frame.end_s > duration_s
                || !frame.tonal_activity.is_finite()
                || !(0.0..=1.0).contains(&frame.tonal_activity)
                || frame
                    .chroma
                    .iter()
                    .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
            {
                return Err("local harmony frames are unordered or out of range".into());
            }
            let sum = frame.chroma.iter().sum::<f32>();
            if sum > 0.0 && (sum - 1.0).abs() > 0.02 {
                return Err("local chroma bins must be normalized".into());
            }
            previous_end = frame.end_s;
        }
        Ok(())
    }

    /// Fraction of the requested interval described by the cached trajectory.
    pub fn coverage(&self, start_s: f64, end_s: f64) -> Option<f32> {
        if self.analyzer_version != ANALYZER_VERSION
            || !start_s.is_finite()
            || !end_s.is_finite()
            || end_s <= start_s
        {
            return None;
        }
        let covered = self
            .frames
            .iter()
            .map(|frame| (frame.end_s.min(end_s) - frame.start_s.max(start_s)).max(0.0))
            .sum::<f64>()
            .min(end_s - start_s);
        Some((covered / (end_s - start_s)) as f32)
    }

    /// Duration-weighted chroma for a source-time interval, if enough tonal
    /// evidence exists. This deliberately returns no label when evidence is
    /// sparse; callers can preserve Unknown instead of guessing a key.
    pub fn average(&self, start_s: f64, end_s: f64) -> Option<([f32; 12], f32)> {
        if self.analyzer_version != ANALYZER_VERSION
            || !start_s.is_finite()
            || !end_s.is_finite()
            || end_s <= start_s
        {
            return None;
        }
        let mut chroma = [0.0f64; 12];
        let (mut weighted_duration, mut active_duration) = (0.0, 0.0);
        for frame in &self.frames {
            let overlap = (frame.end_s.min(end_s) - frame.start_s.max(start_s)).max(0.0);
            if overlap <= 0.0 {
                continue;
            }
            for (sum, value) in chroma.iter_mut().zip(frame.chroma) {
                *sum += f64::from(value) * overlap;
            }
            weighted_duration += overlap;
            active_duration += overlap * f64::from(frame.tonal_activity);
        }
        let total = chroma.iter().sum::<f64>();
        if weighted_duration <= 0.0 || total <= 1e-9 {
            return None;
        }
        Some((
            chroma.map(|value| (value / total) as f32),
            (active_duration / weighted_duration) as f32,
        ))
    }
}
