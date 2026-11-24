//! Display metering: ballistics and loudness integration on top of the
//! instantaneous peaks a provider reports. Kept in the UI so a detached engine
//! only has to ship raw peaks, and so every surface meters consistently.

use patch_model::audio::{MeterFrame, SILENCE_DB};

/// System-meter floor for dBFS text and signal-presence cutoffs.
pub const FLOOR_DB: f32 = -60.0;

/// Spectrum display range: 0 dBFS (top) to −72 dBFS (bottom). Band levels
/// are energy per band, calibrated so a full-scale sine reads 0 dBFS.
pub const SPECTRUM_FLOOR_DB: f32 = -72.0;

fn spectrum_pos(db: f32) -> f32 {
    ((db - SPECTRUM_FLOOR_DB) / -SPECTRUM_FLOOR_DB).clamp(0.0, 1.0)
}

/// Loudness display range for the history, LUFS.
/// Loudness history scale: −48 … 0 LUFS. Loud modern masters (−9 … −6
/// LUFS momentary) sit below the top instead of pinning it; the EBU R128
/// target (−23) and streaming targets (−14) fall inside with room around.
pub const LUFS_FLOOR: f32 = -48.0;

pub fn lufs_pos(lufs: f32) -> f32 {
    ((lufs - LUFS_FLOOR) / -LUFS_FLOOR).clamp(0.0, 1.0)
}

fn db_to_power(db: f32) -> f32 {
    if db <= SILENCE_DB {
        0.0
    } else {
        10f32.powf(db / 10.0)
    }
}

fn power_to_db(p: f32) -> f32 {
    if p <= 1e-12 {
        SILENCE_DB
    } else {
        10.0 * p.log10()
    }
}

/// Digital peak meter ballistics (IEC 60268-18): instant attack, return
/// 20 dB in 1.7 s, and a peak hold that waits 1.5 s before falling 20 dB/s.
/// Every level meter in Patch uses these, so they all read alike. Stepped at
/// the display rate with the real frame time, so motion is smooth without
/// any animation in the view (an animated bar lags and jitters).
#[derive(Clone, Copy, Debug)]
pub struct Ballistic {
    pub level: f32,
    pub peak: f32,
    hold: f32,
}

impl Default for Ballistic {
    fn default() -> Self {
        Ballistic {
            level: SILENCE_DB,
            peak: SILENCE_DB,
            hold: 0.0,
        }
    }
}

impl Ballistic {
    pub const RELEASE_DB_PER_S: f32 = 20.0 / 1.7;
    pub const HOLD_S: f32 = 1.5;
    pub const PEAK_FALL_DB_PER_S: f32 = 20.0;

    pub fn step(&mut self, input_db: f32, dt: f32) {
        let input = input_db.max(SILENCE_DB);
        let dt = dt.clamp(0.0, 0.25);
        self.level = if input >= self.level {
            input
        } else {
            (self.level - Self::RELEASE_DB_PER_S * dt)
                .max(input)
                .max(SILENCE_DB)
        };
        if self.level >= self.peak {
            self.peak = self.level;
            self.hold = 0.0;
        } else {
            self.hold += dt;
            if self.hold > Self::HOLD_S {
                self.peak = (self.peak - Self::PEAK_FALL_DB_PER_S * dt).max(self.level);
            }
        }
    }
}

pub struct Metering {
    pub strips: Vec<Ballistic>,
    /// Per strip and side (L, R), when the provider measures sides.
    pub strips_lr: Vec<[Ballistic; 2]>,
    /// Measured VU per strip (VU, 0 = reference), −120 when not measured.
    pub strips_vu: Vec<f32>,
    /// Strip levels already include their fader.
    pub post_fader: bool,
    pub deck_inputs: [[Ballistic; 2]; 4],
    pub has_deck_inputs: bool,
    /// One ballistic per channel of the input and output layouts.
    pub input: Vec<Ballistic>,
    pub output: Vec<Ballistic>,
    /// Smoothed band levels in dBFS.
    spectrum: Vec<f32>,
    momentary: f32,
    integrated: f32,
    true_peak: f32,
    true_peak_age: f32,
}

impl Default for Metering {
    fn default() -> Self {
        Metering {
            strips: vec![],
            strips_lr: vec![],
            strips_vu: vec![],
            post_fader: false,
            deck_inputs: Default::default(),
            has_deck_inputs: false,
            input: Default::default(),
            output: Default::default(),
            spectrum: vec![],
            momentary: 0.0,
            integrated: 0.0,
            true_peak: SILENCE_DB,
            true_peak_age: 0.0,
        }
    }
}

impl Metering {
    pub fn step(&mut self, f: &MeterFrame, dt: f32) {
        self.strips.resize(f.strips_db.len(), Ballistic::default());
        for (b, db) in self.strips.iter_mut().zip(&f.strips_db) {
            b.step(*db, dt);
        }
        match &f.strips_lr {
            Some(lr) => {
                self.strips_lr.resize(lr.len(), Default::default());
                for (b, p) in self.strips_lr.iter_mut().zip(lr) {
                    b[0].step(p[0], dt);
                    b[1].step(p[1], dt);
                }
            }
            // No sides measured: both sides are the strip's level.
            None => {
                self.strips_lr = self.strips.iter().map(|b| [*b, *b]).collect();
            }
        }
        self.post_fader = f.strips_post_fader;
        self.has_deck_inputs = f.deck_input_db.is_some();
        if let Some(inputs) = f.deck_input_db {
            for (bank, peaks) in self.deck_inputs.iter_mut().zip(inputs) {
                for (ballistic, db) in bank.iter_mut().zip(peaks) { ballistic.step(db, dt); }
            }
        } else {
            self.deck_inputs = Default::default();
        }
        self.strips_vu.clear();
        self.strips_vu.extend((0..f.strips_db.len()).map(|i| {
            f.strips_vu
                .as_ref()
                .and_then(|v| v.get(i).copied())
                .filter(|v| *v > -70.0)
                .unwrap_or(-120.0)
        }));
        for (bank, levels) in [(&mut self.input, &f.input), (&mut self.output, &f.output)] {
            bank.resize(levels.peak_db.len(), Ballistic::default());
            for (b, db) in bank.iter_mut().zip(&levels.peak_db) {
                b.step(*db, dt);
            }
        }
        // Spectrum: instant rise, 40 dB/s fall (a "fast" analyser release:
        // transients show, bars don't jitter between blocks).
        self.spectrum.resize(f.spectrum_db.len(), SILENCE_DB);
        for (s, db) in self.spectrum.iter_mut().zip(&f.spectrum_db) {
            *s = if *db > *s {
                *db
            } else {
                (*s - 40.0 * dt.clamp(0.0, 0.25)).max(*db)
            };
        }
        // Loudness (simulated): mean power of the master with a 400 ms
        // (momentary) and ~30 s (integrated-like) time constant.
        let p = db_to_power(f.output.max_db());
        self.momentary += (p - self.momentary) * (dt / 0.4).min(1.0);
        self.integrated += (p - self.integrated) * (dt / 30.0).min(1.0);
        // True peak: hold the maximum for 10 s.
        if f.true_peak_db >= self.true_peak {
            self.true_peak = f.true_peak_db;
            self.true_peak_age = 0.0;
        } else {
            self.true_peak_age += dt;
            if self.true_peak_age > 10.0 {
                self.true_peak = f.true_peak_db;
                self.true_peak_age = 0.0;
            }
        }
    }

    /// Peak-to-loudness offset of typical programme, used to turn the
    /// simulated master peaks into plausible LUFS figures.
    const PLR: f32 = 8.0;

    pub fn lufs_momentary(&self) -> f32 {
        (power_to_db(self.momentary) - Self::PLR).max(-70.0)
    }

    pub fn lufs_integrated(&self) -> f32 {
        (power_to_db(self.integrated) - Self::PLR).max(-70.0)
    }

    pub fn true_peak(&self) -> f32 {
        self.true_peak
    }

    /// The smoothed spectrum, dBFS per band (log-spaced 25 Hz … 20 kHz).
    pub fn spectrum_db(&self) -> &[f32] {
        &self.spectrum
    }

    pub fn spectrum_pos(&self) -> impl Iterator<Item = f32> + '_ {
        self.spectrum.iter().map(|db| spectrum_pos(*db))
    }
}

/// "−23.0" LUFS, "−∞" below the absolute gate.
pub fn lufs_text(lufs: f32) -> String {
    if !lufs.is_finite() || lufs <= -70.0 {
        "−∞".into()
    } else {
        crate::num::fixed(lufs, 1).replace('-', "−")
    }
}

/// "+1.5" / "−7.0" VU, "−∞" when silent.
pub fn vu_text(vu: f32) -> String {
    let t = if vu <= -40.0 {
        "−∞".to_string()
    } else {
        crate::num::signed(vu, 1).replace('-', "−")
    };
    format!("{t:>5}")
}

/// "−12.4" style number, "−∞" below the floor.
pub fn db_text(db: f32) -> String {
    let t = if db <= FLOOR_DB - 10.0 {
        "−∞".to_string()
    } else {
        crate::num::fixed(db, 1).replace('-', "−")
    };
    format!("{t:>5}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loudness_history_starts_silent_without_a_measured_snapshot() {
        let mut meter = Metering::default();
        meter.step(&MeterFrame::default(), 1.0 / 60.0);
        assert_eq!(lufs_pos(meter.lufs_momentary()), 0.0);
        assert_eq!(lufs_pos(meter.lufs_integrated()), 0.0);
    }

    #[test]
    fn deck_input_meter_is_independent_and_holds_overloads_then_releases() {
        let mut m = Metering::default();
        let mut frame = MeterFrame { strips_db: vec![SILENCE_DB; 4],
            deck_input_db: Some([[-12.0, -9.0], [3.0, -3.0], [-6.0; 2], [-18.0; 2]]),
            ..Default::default() };
        m.step(&frame, 1.0 / 60.0);
        assert!(m.has_deck_inputs);
        assert_eq!(m.deck_inputs[0][1].level, -9.0);
        assert_eq!(m.deck_inputs[1][0].peak, 3.0);
        assert!(m.strips.iter().all(|b| b.level == SILENCE_DB));
        frame.deck_input_db = Some([[SILENCE_DB; 2]; 4]);
        for _ in 0..60 { m.step(&frame, 1.0 / 60.0); }
        assert_eq!(m.deck_inputs[1][0].peak, 3.0, "a brief overload remains visible");
        for _ in 0..60 { m.step(&frame, 1.0 / 60.0); }
        assert!(m.deck_inputs[1][0].peak < 0.0, "the hold releases");
        frame.deck_input_db = None;
        m.step(&frame, 1.0 / 60.0);
        assert!(!m.has_deck_inputs);
        assert_eq!(m.deck_inputs[1][0].peak, SILENCE_DB, "disconnect clears stale overloads");
    }

    #[test]
    fn peak_meter_follows_iec_60268_18() {
        // Instant attack.
        let mut b = Ballistic::default();
        b.step(-6.0, 1.0 / 60.0);
        assert_eq!(b.level, -6.0);
        // Return: 20 dB in 1.7 s, frame by frame at 60 fps.
        for _ in 0..102 {
            b.step(SILENCE_DB, 1.0 / 60.0);
        }
        assert!((b.level + 26.0).abs() < 0.05, "level {}", b.level);
        // The hold waited 1.5 s, then fell 20 dB/s for 0.2 s.
        assert!((b.peak + 10.0).abs() < 0.1, "peak {}", b.peak);
    }

    #[test]
    fn frame_rate_does_not_change_the_reading() {
        let (mut a, mut b) = (Ballistic::default(), Ballistic::default());
        a.step(-3.0, 0.0);
        b.step(-3.0, 0.0);
        for _ in 0..60 {
            a.step(SILENCE_DB, 1.0 / 60.0);
        }
        for _ in 0..144 {
            b.step(SILENCE_DB, 1.0 / 144.0);
        }
        assert!((a.level - b.level).abs() < 0.01 && (a.peak - b.peak).abs() < 0.01);
    }
}
