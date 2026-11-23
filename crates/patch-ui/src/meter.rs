//! Canonical meter scales and level positioning shared by Patch Units.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MeterKind {
    /// Relative operating level for DJ and mixer channels; 0 dB is nominal.
    Performance,
    /// Absolute digital level; 0 dBFS is the ceiling.
    #[default]
    System,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeterProfile {
    pub floor_db: f32,
    pub ceiling_db: f32,
    /// The dBFS input that is reported as 0 on this display.
    pub nominal_dbfs: f32,
    pub marks_db: &'static [f32],
}

pub const PERFORMANCE_MARKS: &[f32] = &[6.0, 3.0, 0.0, -3.0, -6.0, -9.0, -12.0, -18.0, -24.0];
pub const SYSTEM_MARKS: &[f32] = &[
    0.0, -1.0, -3.0, -6.0, -12.0, -18.0, -24.0, -36.0, -48.0, -60.0,
];

impl MeterKind {
    pub const fn profile(self) -> MeterProfile {
        match self {
            Self::Performance => MeterProfile {
                floor_db: -24.0,
                ceiling_db: 6.0,
                nominal_dbfs: -18.0,
                marks_db: PERFORMANCE_MARKS,
            },
            Self::System => MeterProfile {
                floor_db: -60.0,
                ceiling_db: 0.0,
                nominal_dbfs: 0.0,
                marks_db: SYSTEM_MARKS,
            },
        }
    }

    /// Convert an underlying dBFS sample measurement into this meter's
    /// visual 0…1 coordinate. Performance meters are aligned so -18 dBFS is
    /// nominal 0 dB; System meters retain absolute dBFS. System levels use a
    /// piecewise scale, reserving more space for the -12…0 dBFS warning band.
    pub fn position(self, db: f32) -> f32 {
        if !db.is_finite() {
            return 0.0;
        }
        let display_db = self.display_db(db);
        match self {
            Self::Performance => ((display_db + 24.0) / 30.0).clamp(0.0, 1.0),
            Self::System => interpolate(
                display_db,
                &[
                    (-60.0, 0.0),
                    (-48.0, 0.06),
                    (-36.0, 0.14),
                    (-24.0, 0.24),
                    (-18.0, 0.33),
                    (-12.0, 0.44),
                    (-6.0, 0.68),
                    (-3.0, 0.80),
                    (-1.0, 0.90),
                    (0.0, 1.0),
                ],
            ),
        }
    }

    /// dB value printed along the display axis, without changing audio data.
    pub fn display_db(self, dbfs: f32) -> f32 {
        dbfs - self.profile().nominal_dbfs
    }

    /// The meter body changes to warning color at this level.
    pub const fn warning_db(self) -> f32 {
        match self {
            Self::Performance => 0.0,
            Self::System => -6.0,
        }
    }

    /// The stronger warning band starts here. System meters reserve red for
    /// the last 0.5 dB before Patch's output ceiling; for performance meters
    /// +3 dB is very hot.
    pub const fn strong_warning_db(self) -> f32 {
        match self {
            Self::Performance => 3.0,
            Self::System => -0.5,
        }
    }
}

fn interpolate(db: f32, knots: &[(f32, f32)]) -> f32 {
    if db <= knots[0].0 {
        return knots[0].1;
    }
    for pair in knots.windows(2) {
        let (lo_db, lo_pos) = pair[0];
        let (hi_db, hi_pos) = pair[1];
        if db <= hi_db {
            let t = (db - lo_db) / (hi_db - lo_db);
            return lo_pos + t * (hi_pos - lo_pos);
        }
    }
    1.0
}

/// Main-output peak data keeps sample peak, optional future true peak, and
/// clipping as separate facts. LUFS remains a separate loudness measurement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeakReading {
    pub sample_peak_dbfs: f32,
    pub true_peak_dbtp: Option<f32>,
    pub clipping: bool,
}

impl PeakReading {
    pub const fn sample(sample_peak_dbfs: f32, clipping: bool) -> Self {
        Self {
            sample_peak_dbfs,
            true_peak_dbtp: None,
            clipping,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_ranges_keep_nominal_and_ceiling_distinct() {
        assert_eq!(MeterKind::Performance.position(-42.0), 0.0);
        assert!((MeterKind::Performance.position(-18.0) - 0.8).abs() < 1e-6);
        assert_eq!(MeterKind::Performance.position(-12.0), 1.0);
        assert_eq!(MeterKind::Performance.display_db(-12.0), 6.0);
        assert_eq!(MeterKind::Performance.profile().nominal_dbfs, -18.0);
        assert_eq!(MeterKind::System.position(-60.0), 0.0);
        assert_eq!(MeterKind::System.position(0.0), 1.0);
        assert_eq!(MeterKind::System.strong_warning_db(), -0.5);
        assert!(MeterKind::System.position(-6.0) - MeterKind::System.position(-12.0) > 0.20);
        assert!((MeterKind::System.position(-3.0) - 0.80).abs() < 1e-6);
        assert_eq!(MeterKind::System.profile().marks_db[0], 0.0);
    }

    #[test]
    fn main_output_peak_retains_sample_and_future_true_peak_separately() {
        let peak = PeakReading::sample(-1.2, false);
        assert_eq!(peak.sample_peak_dbfs, -1.2);
        assert_eq!(peak.true_peak_dbtp, None);
        assert!(!peak.clipping);
    }
}
