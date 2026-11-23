//! Open, offline device-aware output optimization primitives for Patch.
//! Profiles are data only. Parsing and compilation happen on control threads;
//! audio callbacks consume bounded, prevalidated coefficients.
use serde::{Deserialize, Serialize};

pub const PROFILE_FORMAT: u32 = 1;
pub const MAX_FILTERS: usize = 8;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceIdentity {
    /// Stable identifier derived from hardware properties, never the display name alone.
    pub stable_id: String,
    pub backend: Option<String>,
    pub bus: Option<String>,
    pub vendor_id: Option<String>,
    pub product_id: Option<String>,
    pub serial: Option<String>,
    pub node_name: Option<String>,
    pub channel_layout: Vec<String>,
    /// Optional hardware in a logical chain, e.g. DAC plus active monitors.
    #[serde(default)]
    pub components: Vec<DeviceIdentity>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Provenance {
    /// measured | modeled | known | inferred | user
    pub kind: String,
    pub source: String,
    pub confidence: Option<f32>,
    pub conditions: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PeakingFilter {
    pub frequency_hz: f64,
    pub q: f64,
    pub gain_db: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DeviceCorrection {
    pub filters: Vec<PeakingFilter>,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Protection {
    /// Unknown remains None; these are documented estimates, not measured excursion data.
    pub peak_ceiling_dbfs: f32,
    pub max_positive_eq_db: f32,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Preference {
    /// Bounded user layer, independent of the device correction.
    pub bass_db: f32,
    pub mid_db: f32,
    pub treble_db: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct OutputProfile {
    pub format_version: u32,
    pub id: String,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub device_type: String,
    pub identity: DeviceIdentity,
    pub correction: DeviceCorrection,
    pub protection: Option<Protection>,
    /// Room and listener data stay independently stored from portable device truth.
    pub room_response: Option<Vec<f32>>,
    pub listener_preference: Option<Preference>,
    pub sample_rates: Vec<u32>,
    pub latency_budget_ms: Option<f32>,
    pub notes: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BiquadCoefficients {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl PeakingFilter {
    pub fn compile(&self, rate: u32) -> Result<BiquadCoefficients, String> {
        let fs = f64::from(rate);
        if !(8000..=384000).contains(&rate)
            || !self.frequency_hz.is_finite()
            || self.frequency_hz <= 0.0
            || self.frequency_hz >= fs * 0.49
            || !self.q.is_finite()
            || !(0.1..=20.0).contains(&self.q)
            || !self.gain_db.is_finite()
            || self.gain_db.abs() > 12.0
        {
            return Err(
                "filter frequency, Q, gain, or sample rate is outside the safe profile range"
                    .into(),
            );
        }
        let a = 10f64.powf(self.gain_db / 40.0);
        let w = std::f64::consts::TAU * self.frequency_hz / fs;
        let alpha = w.sin() / (2.0 * self.q);
        let c = w.cos();
        let a0 = 1.0 + alpha / a;
        let co = BiquadCoefficients {
            b0: (1.0 + alpha * a) / a0,
            b1: (-2.0 * c) / a0,
            b2: (1.0 - alpha * a) / a0,
            a1: (-2.0 * c) / a0,
            a2: (1.0 - alpha / a) / a0,
        };
        if [co.b0, co.b1, co.b2, co.a1, co.a2]
            .iter()
            .any(|x| !x.is_finite())
            || co.a2.abs() >= 1.0
        {
            return Err("unstable filter coefficients".into());
        }
        Ok(co)
    }
}

impl BiquadCoefficients {
    pub fn identity() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        }
    }
}

impl OutputProfile {
    pub fn from_toml(source: &str) -> Result<Self, String> {
        let p: Self = toml::from_str(source).map_err(|e| format!("profile parse error: {e}"))?;
        p.validate()?;
        Ok(p)
    }
    pub fn to_toml(&self) -> Result<String, String> {
        self.validate()?;
        toml::to_string_pretty(self).map_err(|e| format!("profile encode error: {e}"))
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.format_version != PROFILE_FORMAT {
            return Err(format!(
                "unsupported profile version {}",
                self.format_version
            ));
        }
        if self.id.is_empty() || self.id.len() > 128 || self.id.contains("..") {
            return Err("invalid profile ID".into());
        }
        if self.identity.stable_id.is_empty() {
            return Err("stable hardware identity is required".into());
        }
        validate_provenance(&self.correction.provenance)?;
        if self.correction.filters.len() > MAX_FILTERS {
            return Err(format!(
                "at most {MAX_FILTERS} correction filters are supported"
            ));
        }
        if !self.protection.as_ref().is_none_or(|p| {
            p.peak_ceiling_dbfs.is_finite()
                && (-12.0..=0.0).contains(&p.peak_ceiling_dbfs)
                && p.max_positive_eq_db.is_finite()
                && (0.0..=6.0).contains(&p.max_positive_eq_db)
        }) {
            return Err("invalid protection limits".into());
        }
        if let Some(p) = &self.protection {
            validate_provenance(&p.provenance)?;
        }
        if self.sample_rates.is_empty()
            || self
                .sample_rates
                .iter()
                .any(|r| !(8000..=384000).contains(r))
        {
            return Err("profile must declare valid supported sample rates".into());
        }
        if let Some(room) = &self.room_response
            && (room.len() > 384_000 || room.iter().any(|v| !v.is_finite()))
        {
            return Err("room response is oversized or non-finite".into());
        }
        if let Some(p) = &self.listener_preference
            && [p.bass_db, p.mid_db, p.treble_db]
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 6.0)
        {
            return Err("listener preference exceeds ±6 dB".into());
        }
        if self
            .latency_budget_ms
            .is_some_and(|v| !v.is_finite() || !(0.0..=2000.0).contains(&v))
        {
            return Err("invalid latency budget".into());
        }
        for rate in &self.sample_rates {
            for f in &self.correction.filters {
                f.compile(*rate)?;
            }
        }
        Ok(())
    }

    /// Exact stable identity match only. Weak/name-only matching is intentionally not automatic.
    pub fn matches(&self, identity: &DeviceIdentity) -> bool {
        self.identity.stable_id == identity.stable_id
    }
    pub fn headroom_db(&self) -> f32 {
        self.correction
            .filters
            .iter()
            .map(|f| f.gain_db as f32)
            .filter(|g| *g > 0.0)
            .sum::<f32>()
            .min(24.0)
    }
}

fn validate_provenance(p: &Provenance) -> Result<(), String> {
    if !["measured", "modeled", "known", "inferred", "user"].contains(&p.kind.as_str())
        || p.source.trim().is_empty()
        || p.source.len() > 512
        || p.confidence
            .is_some_and(|v| !v.is_finite() || !(0.0..=1.0).contains(&v))
        || p.conditions.as_ref().is_some_and(|v| v.len() > 2048)
    {
        return Err("invalid provenance fields".into());
    }
    Ok(())
}

/// Stable FNV-1a fingerprint from hardware metadata. Caller supplies normalized
/// identifiers in a deterministic order; description/name must not be included.
pub fn stable_identity_id(parts: &[&str]) -> String {
    let mut h = 0xcbf29ce484222325u64;
    for part in parts {
        for b in part.trim().to_ascii_lowercase().bytes().chain([0xff]) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    format!("patch-output:{h:016x}")
}

/// Peak protection with instantaneous attack and smoothed release. No lookahead
/// and therefore zero algorithmic latency; it is a safety ceiling, not a brickwall mastering limiter.
#[derive(Clone, Copy, Debug)]
pub struct PeakGuard {
    ceiling: f32,
    release: f32,
    gain: f32,
    /// What it was built for, as asked: `ceiling_dbfs()` read back through
    /// the linear ceiling is not exact (−1 dB comes back −1.0000004), so a
    /// caller comparing against that rebuilt the guard every block.
    asked: (f32, u32),
}
impl PeakGuard {
    pub fn new(ceiling_dbfs: f32, rate: u32) -> Self {
        Self {
            ceiling: 10f32.powf(ceiling_dbfs.clamp(-12.0, 0.0) / 20.0),
            release: (-1.0 / (0.08 * rate.max(1) as f32)).exp(),
            gain: 1.0,
            asked: (ceiling_dbfs, rate),
        }
    }
    /// Built for this ceiling and rate (bit for bit): no rebuild needed.
    pub fn is_for(&self, ceiling_dbfs: f32, rate: u32) -> bool {
        self.asked.0.to_bits() == ceiling_dbfs.to_bits() && self.asked.1 == rate
    }
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
            let peak = a.abs().max(b.abs());
            let target = if peak > self.ceiling {
                self.ceiling / peak
            } else {
                1.0
            };
            if target < self.gain {
                self.gain = target;
            } else {
                self.gain = target + (self.gain - target) * self.release;
            }
            *a = (*a * self.gain).clamp(-self.ceiling, self.ceiling);
            *b = (*b * self.gain).clamp(-self.ceiling, self.ceiling);
        }
    }
    pub fn ceiling_dbfs(&self) -> f32 {
        20.0 * self.ceiling.log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_peak_guard_knows_the_ceiling_it_was_built_for() {
        for tenth in 0..=120 {
            let db = -(tenth as f32) / 10.0;
            let g = PeakGuard::new(db, 48_000);
            assert!(g.is_for(db, 48_000), "{db} dB");
            assert!(!g.is_for(db, 44_100));
        }
        assert!(!PeakGuard::new(-1.0, 48_000).is_for(-1.5, 48_000));
    }
    fn profile() -> OutputProfile {
        OutputProfile {
            format_version: 1,
            id: "test-speaker".into(),
            manufacturer: None,
            model: None,
            device_type: "speaker".into(),
            identity: DeviceIdentity {
                stable_id: "patch-output:1234".into(),
                backend: None,
                bus: None,
                vendor_id: None,
                product_id: None,
                serial: None,
                node_name: None,
                channel_layout: vec!["FL".into(), "FR".into()],
                components: vec![],
            },
            correction: DeviceCorrection {
                filters: vec![PeakingFilter {
                    frequency_hz: 120.0,
                    q: 0.8,
                    gain_db: -3.0,
                }],
                provenance: Provenance {
                    kind: "measured".into(),
                    source: "local calibration".into(),
                    confidence: Some(0.7),
                    conditions: None,
                },
            },
            protection: None,
            room_response: None,
            listener_preference: None,
            sample_rates: vec![44100, 48000, 96000],
            latency_budget_ms: Some(5.0),
            notes: None,
        }
    }
    #[test]
    fn validates_rates_and_filter_coefficients() {
        let p = profile();
        assert!(p.validate().is_ok());
        assert!(p.correction.filters[0].compile(44100).is_ok());
    }
    #[test]
    fn open_toml_profile_round_trips_and_revalidates() {
        let p = profile();
        let encoded = p.to_toml().unwrap();
        let decoded = OutputProfile::from_toml(&encoded).unwrap();
        assert_eq!(p, decoded);
    }
    #[test]
    fn refuses_unsafe_or_malformed_profiles() {
        let mut p = profile();
        p.id = "../bad".into();
        assert!(p.validate().is_err());
        let mut p = profile();
        p.correction.filters[0].gain_db = 15.0;
        assert!(p.validate().is_err());
    }
    #[test]
    fn stable_identity_ignores_display_names() {
        assert_eq!(
            stable_identity_id(&["USB", "1234", "serial"]),
            stable_identity_id(&["usb", "1234", "SERIAL"])
        );
        assert_ne!(
            stable_identity_id(&["USB", "1234"]),
            stable_identity_id(&["USB", "5678"])
        );
    }
    #[test]
    fn profile_match_is_identity_only() {
        let p = profile();
        let mut d = p.identity.clone();
        assert!(p.matches(&d));
        d.stable_id = "different".into();
        assert!(!p.matches(&d));
    }
    #[test]
    fn guard_caps_peak_and_releases_smoothly() {
        let mut g = PeakGuard::new(-1.0, 48000);
        let (mut l, mut r) = (vec![2.0; 32], vec![0.5; 32]);
        g.process(&mut l, &mut r);
        assert!(l.iter().all(|x| x.abs() <= 0.892));
        assert!(r[0] < 0.5);
        let (mut l, mut r) = (vec![0.1; 128], vec![0.1; 128]);
        g.process(&mut l, &mut r);
        assert!(l[127] > l[0]);
    }
}
