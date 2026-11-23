//! Where a track sings: the lead vocal's level against the whole mix, per
//! second. Measured on the separator's vocals stem (htdemucs), not the mix:
//! a spectral guess on dense club music took synth leads for voices. Kept
//! with the analysis, but optional: it needs the stem separator, which a
//! Patch may not have, and decks and the AI DJ work without it.

use serde::{Deserialize, Serialize};

/// Bump when the measurement changes; older evidence is measured again.
pub const ANALYZER_VERSION: u32 = 1;

/// `level` is the vocal stem's level relative to the mix: 0 at 30 dB or
/// more below it (or silence), 255 as loud as the mix.
pub const FLOOR_DB: f32 = -30.0;
/// A second with a voice you hear: the vocal stem within 14 dB of the mix
/// (separation bleed of synths and hats stays below about −20 dB).
pub const AUDIBLE_DB: f32 = -14.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VocalEvidence {
    pub analyzer_version: u32,
    /// The separator that measured it ("htdemucs 955717e8").
    pub model: String,
    /// Per second from the start of the track.
    pub level: Vec<u8>,
    /// Share of the track's audible seconds with a voice you hear (0…1).
    pub presence: f32,
}

impl VocalEvidence {
    /// From per-second vocal-to-mix ratios (dB) and whether the mix was
    /// audible that second.
    pub fn from_ratios(model: &str, seconds: &[(f32, bool)]) -> Self {
        let level = seconds
            .iter()
            .map(|&(db, audible)| if audible { db_to_level(db) } else { 0 })
            .collect();
        let audible = seconds.iter().filter(|s| s.1).count();
        let sung = seconds.iter().filter(|s| s.1 && s.0 >= AUDIBLE_DB).count();
        Self {
            analyzer_version: ANALYZER_VERSION,
            model: model.into(),
            level,
            presence: if audible == 0 {
                0.0
            } else {
                sung as f32 / audible as f32
            },
        }
    }

    pub fn current(&self) -> bool {
        self.analyzer_version == ANALYZER_VERSION
    }

    /// Share of the seconds in `from..to` (seconds) with a voice you hear.
    pub fn sung_between(&self, from: f64, to: f64) -> f32 {
        let a = from.max(0.0).floor() as usize;
        let b = (to.max(0.0).ceil() as usize).min(self.level.len());
        if b <= a {
            return 0.0;
        }
        let audible = db_to_level(AUDIBLE_DB);
        self.level[a..b].iter().filter(|&&l| l >= audible).count() as f32 / (b - a) as f32
    }

    /// The vocal is heard at `t` seconds.
    pub fn sung_at(&self, t: f64) -> bool {
        t >= 0.0
            && self
                .level
                .get(t as usize)
                .is_some_and(|&l| l >= db_to_level(AUDIBLE_DB))
    }

    /// A word for the Library and the CLI.
    pub fn describe(&self) -> &'static str {
        match self.presence {
            p if p >= 0.25 => "vocal",
            p if p >= 0.10 => "some vocals",
            p if p >= 0.05 => "vocal shots",
            _ => "instrumental",
        }
    }
}

pub fn db_to_level(db: f32) -> u8 {
    (((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_counts_only_audible_seconds_with_a_voice() {
        let v = VocalEvidence::from_ratios(
            "test",
            &[(-6.0, true), (-20.0, true), (0.0, false), (-10.0, true)],
        );
        assert_eq!(v.level, vec![204, 85, 0, 170]);
        assert!((v.presence - 2.0 / 3.0).abs() < 1e-6);
        assert!(v.sung_at(0.5) && !v.sung_at(1.5) && !v.sung_at(9.0));
        assert!((v.sung_between(0.0, 4.0) - 0.5).abs() < 1e-6);
        assert_eq!(v.sung_between(3.0, 3.0), 0.0);
        assert_eq!(v.describe(), "vocal");
    }
}
