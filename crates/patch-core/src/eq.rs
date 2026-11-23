//! The EQ Unit's settings: what the DJ or engineer asks the master EQ to
//! do (docs/eq). Plain data shared by the service, the UI and the CLI; the
//! engine designs the filters from it (`patch_dsp::filter`).

use serde::{Deserialize, Serialize};

/// Where an EQ sits: "master" (the program) or an engine channel. Accepts
/// the short names; returns the canonical one.
pub fn target(name: &str) -> Option<&'static str> {
    Some(match name {
        "master" | "program" | "out" => "master",
        "bus" | "apps" | "app" => "bus",
        // The DJ Booth: both decks through one EQ, as its Mixer strip.
        "dj" | "booth" | "dj-booth" => "dj",
        "deck-a" | "a" => "deck-a",
        "deck-b" | "b" => "deck-b",
        "deck-c" | "c" => "deck-c",
        "deck-d" | "d" => "deck-d",
        _ => return crate::mic::CHANNELS.iter().copied().find(|channel| *channel == name),
    })
}

/// Any EQ target: a fixed one, or an application's ("app:<name>").
pub fn valid_target(t: &str) -> bool {
    crate::mic::channel(t) || target(t).is_some()
        || t.strip_prefix("app:")
            .is_some_and(|n| !n.trim().is_empty() && n.len() <= 100)
}

/// Engine channels for applications with their own EQ.
pub const APP_CHANNELS: [&str; 4] = ["app-1", "app-2", "app-3", "app-4"];

/// The EQ targets in display order: (canonical name, label). The same
/// channels as the Mixer; mics and applications follow as they exist.
/// Single decks stay addressable (`deck-a`…) but have no tab.
pub const TARGETS: [(&str, &str); 3] = [("master", "Master"), ("bus", "Apps"), ("dj", "DJ Booth")];

/// The decks the DJ Booth's EQ shapes.
pub const DJ_DECKS: [&str; 2] = ["deck-a", "deck-b"];

/// Bands of each EQ.
pub const BANDS: usize = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EqKind {
    #[default]
    Bell,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
    Notch,
}

impl EqKind {
    pub const ALL: [EqKind; 6] = [
        EqKind::Bell,
        EqKind::LowShelf,
        EqKind::HighShelf,
        EqKind::LowPass,
        EqKind::HighPass,
        EqKind::Notch,
    ];

    pub fn name(self) -> &'static str {
        match self {
            EqKind::Bell => "bell",
            EqKind::LowShelf => "lowshelf",
            EqKind::HighShelf => "highshelf",
            EqKind::LowPass => "lowpass",
            EqKind::HighPass => "highpass",
            EqKind::Notch => "notch",
        }
    }

    /// Short label for tight views.
    pub fn short(self) -> &'static str {
        match self {
            EqKind::Bell => "Bell",
            EqKind::LowShelf => "Lo shelf",
            EqKind::HighShelf => "Hi shelf",
            EqKind::LowPass => "Lo pass",
            EqKind::HighPass => "Hi pass",
            EqKind::Notch => "Notch",
        }
    }

    pub fn parse(s: &str) -> Option<EqKind> {
        Some(match s.to_ascii_lowercase().as_str() {
            "bell" | "peak" | "peaking" => EqKind::Bell,
            "lowshelf" | "ls" | "loshelf" => EqKind::LowShelf,
            "highshelf" | "hs" | "hishelf" => EqKind::HighShelf,
            "lowpass" | "lp" | "lpf" => EqKind::LowPass,
            "highpass" | "hp" | "hpf" => EqKind::HighPass,
            "notch" => EqKind::Notch,
            _ => return None,
        })
    }

    /// Gain matters for this shape (pass filters and notches have none).
    pub fn has_gain(self) -> bool {
        matches!(self, EqKind::Bell | EqKind::LowShelf | EqKind::HighShelf)
    }

    /// The next shape (cycling in the UI).
    pub fn next(self) -> EqKind {
        let i = EqKind::ALL.iter().position(|k| *k == self).unwrap_or(0);
        EqKind::ALL[(i + 1) % EqKind::ALL.len()]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqBand {
    pub on: bool,
    pub kind: EqKind,
    /// Centre / corner frequency, Hz (20 … 20 000).
    pub freq: f32,
    /// Gain, dB (±18; bells and shelves).
    pub gain_db: f32,
    /// Quality (0.1 … 18; shelves use their standard slope).
    pub q: f32,
}

impl Default for EqBand {
    fn default() -> Self {
        EqBand {
            on: true,
            kind: EqKind::Bell,
            freq: 1000.0,
            gain_db: 0.0,
            q: 1.0,
        }
    }
}

pub const FREQ_RANGE: (f32, f32) = (20.0, 20_000.0);
pub const GAIN_RANGE: f32 = 18.0;
pub const Q_RANGE: (f32, f32) = (0.1, 18.0);

impl EqBand {
    /// Within the ranges (the service stores only clamped bands).
    pub fn clamped(mut self) -> EqBand {
        let ok = |v: f32, d: f32| if v.is_finite() { v } else { d };
        self.freq = ok(self.freq, 1000.0).clamp(FREQ_RANGE.0, FREQ_RANGE.1);
        self.gain_db = ok(self.gain_db, 0.0).clamp(-GAIN_RANGE, GAIN_RANGE);
        self.q = ok(self.q, 1.0).clamp(Q_RANGE.0, Q_RANGE.1);
        self
    }

    /// It changes the sound (an off band, or a flat bell or shelf, doesn't).
    pub fn active(&self) -> bool {
        self.on && (!self.kind.has_gain() || self.gain_db.abs() >= 0.01)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqState {
    pub bypass: bool,
    pub bands: Vec<EqBand>,
    /// The preset loaded last ("" = none), and whether it was edited since.
    pub preset: String,
    pub edited: bool,
    /// Input gain before the bands (dB, ±18): a correction's preamp, the
    /// cut that keeps its boosts from reaching the limiter.
    pub gain_db: f32,
}

impl Default for EqState {
    /// Eight bands spread across the spectrum, flat: a high-pass and a
    /// low-pass waiting switched off at the ends.
    fn default() -> Self {
        let b = |kind, freq: f32, q: f32, on| EqBand {
            on,
            kind,
            freq,
            gain_db: 0.0,
            q,
        };
        EqState {
            bypass: false,
            preset: String::new(),
            edited: false,
            gain_db: 0.0,
            bands: vec![
                b(EqKind::HighPass, 30.0, 0.71, false),
                b(EqKind::LowShelf, 100.0, 0.71, true),
                b(EqKind::Bell, 250.0, 1.0, true),
                b(EqKind::Bell, 700.0, 1.0, true),
                b(EqKind::Bell, 2000.0, 1.0, true),
                b(EqKind::Bell, 5000.0, 1.0, true),
                b(EqKind::HighShelf, 10_000.0, 0.71, true),
                b(EqKind::LowPass, 18_000.0, 0.71, false),
            ],
        }
    }
}

impl EqState {
    /// Exactly `BANDS` bands, each within range.
    pub fn normalized(mut self) -> EqState {
        let d = EqState::default();
        self.bands.truncate(BANDS);
        for i in self.bands.len()..BANDS {
            self.bands.push(d.bands[i]);
        }
        for b in &mut self.bands {
            *b = b.clamped();
        }
        self.gain_db = if self.gain_db.is_finite() { self.gain_db.clamp(-GAIN_RANGE, GAIN_RANGE) } else { 0.0 };
        self
    }

    /// Anything to process at all.
    pub fn active(&self) -> bool {
        !self.bypass && (self.gain_db.abs() >= 0.01 || self.bands.iter().any(|b| b.active()))
    }
}

/// An EQ preset in the interchange format (docs/eq/PRESET_FORMAT.md,
/// `patch-eq` version 1). Unknown fields are kept (`extra`), so a preset
/// made by another tool survives a round trip through Patch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub format: String,
    pub version: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "parametric")]
    pub mode: String,
    #[serde(default = "linked")]
    pub channel_mode: String,
    /// Input gain before the bands (dB, ±18; 0 when absent).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub gain_db: f32,
    pub bands: Vec<PresetBand>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn parametric() -> String {
    "parametric".into()
}
fn linked() -> String {
    "linked".into()
}
fn all() -> String {
    "all".into()
}
fn yes() -> bool {
    true
}
fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresetBand {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(rename = "type")]
    pub kind: String,
    pub frequency_hz: f32,
    #[serde(default)]
    pub gain_db: f32,
    pub q: f32,
    #[serde(default = "all")]
    pub channel: String,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Preset type names (snake case) ↔ band shapes.
fn kind_of(t: &str) -> Option<EqKind> {
    Some(match t {
        "bell" | "peak" => EqKind::Bell,
        "low_shelf" => EqKind::LowShelf,
        "high_shelf" => EqKind::HighShelf,
        "low_pass" => EqKind::LowPass,
        "high_pass" => EqKind::HighPass,
        "notch" => EqKind::Notch,
        _ => return None,
    })
}

fn type_of(k: EqKind) -> &'static str {
    match k {
        EqKind::Bell => "bell",
        EqKind::LowShelf => "low_shelf",
        EqKind::HighShelf => "high_shelf",
        EqKind::LowPass => "low_pass",
        EqKind::HighPass => "high_pass",
        EqKind::Notch => "notch",
    }
}

impl Preset {
    /// Checked before anything is applied (the format's importer rules).
    pub fn validate(&self) -> Result<(), String> {
        if self.format != "patch-eq" {
            return Err(format!("not a Patch EQ preset (format '{}')", self.format));
        }
        if self.version != 1 {
            return Err(format!(
                "preset version {} (this Patch reads version 1)",
                self.version
            ));
        }
        if self.mode != "parametric" || self.channel_mode != "linked" {
            return Err("only parametric, linked presets are supported".into());
        }
        if !self.gain_db.is_finite() || self.gain_db.abs() > GAIN_RANGE {
            return Err(format!("input gain {} dB out of range", self.gain_db));
        }
        if self.bands.len() > BANDS {
            return Err(format!("{} bands (an EQ has {BANDS})", self.bands.len()));
        }
        for b in &self.bands {
            kind_of(&b.kind).ok_or_else(|| format!("unknown band type '{}'", b.kind))?;
            if b.channel != "all" {
                return Err(format!("band channel '{}' (only 'all' for now)", b.channel));
            }
            if !(FREQ_RANGE.0..=FREQ_RANGE.1).contains(&b.frequency_hz)
                || b.gain_db.abs() > GAIN_RANGE
                || !(Q_RANGE.0..=Q_RANGE.1).contains(&b.q)
            {
                return Err(format!(
                    "band '{}' out of range",
                    if b.id.is_empty() { &b.kind } else { &b.id }
                ));
            }
        }
        Ok(())
    }

    /// The EQ settings it describes (its bands first, the rest off).
    pub fn to_state(&self) -> Result<EqState, String> {
        self.validate()?;
        let mut st = EqState::default();
        for (i, slot) in st.bands.iter_mut().enumerate() {
            *slot = match self.bands.get(i) {
                Some(b) => EqBand {
                    on: b.enabled,
                    kind: kind_of(&b.kind).unwrap_or_default(),
                    freq: b.frequency_hz,
                    gain_db: b.gain_db,
                    q: b.q,
                },
                None => EqBand { on: false, ..*slot },
            };
        }
        st.preset = self.name.clone();
        st.gain_db = self.gain_db;
        Ok(st.normalized())
    }

    /// A preset of these settings (the bands in use).
    pub fn from_state(id: &str, name: &str, st: &EqState) -> Preset {
        Preset {
            format: "patch-eq".into(),
            version: 1,
            id: id.into(),
            name: name.into(),
            description: String::new(),
            mode: parametric(),
            channel_mode: linked(),
            gain_db: st.gain_db,
            bands: st
                .bands
                .iter()
                .filter(|b| b.on)
                .map(|b| PresetBand {
                    id: String::new(),
                    enabled: true,
                    kind: type_of(b.kind).into(),
                    frequency_hz: b.freq,
                    gain_db: if b.kind.has_gain() { b.gain_db } else { 0.0 },
                    q: b.q,
                    channel: all(),
                    extra: Default::default(),
                })
                .collect(),
            extra: [(
                "provenance".to_string(),
                serde_json::json!({"kind": "user", "tool": "Patch"}),
            )]
            .into_iter()
            .collect(),
        }
    }
}

/// The presets that come with Patch: starting points, not magic.
pub fn builtin_presets() -> Vec<Preset> {
    let b = |kind: &str, f: f32, g: f32, q: f32| PresetBand {
        id: String::new(),
        enabled: true,
        kind: kind.into(),
        frequency_hz: f,
        gain_db: g,
        q,
        channel: all(),
        extra: Default::default(),
    };
    let p = |id: &str, name: &str, desc: &str, bands: Vec<PresetBand>| Preset {
        format: "patch-eq".into(),
        version: 1,
        id: format!("org.omarchy.patch.{id}"),
        name: name.into(),
        description: desc.into(),
        mode: parametric(),
        channel_mode: linked(),
        gain_db: 0.0,
        bands,
        extra: [(
            "provenance".to_string(),
            serde_json::json!({"kind": "builtin", "tool": "Patch"}),
        )]
        .into_iter()
        .collect(),
    };
    vec![
        p("flat", "Flat", "Nothing: every band off", vec![]),
        p(
            "clean-lows",
            "Clean lows",
            "Cut rumble below 30 Hz",
            vec![b("high_pass", 30.0, 0.0, 0.71)],
        ),
        p(
            "warm",
            "Warm",
            "A little weight, softer top",
            vec![
                b("low_shelf", 120.0, 2.0, 0.71),
                b("bell", 3000.0, -1.0, 0.8),
                b("high_shelf", 9000.0, -1.5, 0.71),
            ],
        ),
        p(
            "air",
            "Air",
            "Open, sparkly top",
            vec![
                b("high_shelf", 10000.0, 2.5, 0.71),
                b("bell", 4000.0, 0.8, 0.7),
            ],
        ),
        p(
            "loudness",
            "Loudness",
            "Lows and highs up for quiet listening",
            vec![
                b("low_shelf", 90.0, 4.0, 0.71),
                b("high_shelf", 8000.0, 3.0, 0.71),
            ],
        ),
        p(
            "voice",
            "Voice clarity",
            "Calls and podcasts: less boom, more words",
            vec![
                b("high_pass", 90.0, 0.0, 0.71),
                b("bell", 300.0, -2.5, 1.0),
                b("bell", 3000.0, 2.5, 1.0),
                b("high_shelf", 10000.0, -1.0, 0.71),
            ],
        ),
        p(
            "tame-sub",
            "Tame sub",
            "Club masters on small rooms: the sub down a little",
            vec![b("bell", 55.0, -3.0, 1.4), b("high_pass", 25.0, 0.0, 0.71)],
        ),
        p(
            "laptop",
            "Laptop speakers",
            "No lows they can't play, clearer mids",
            vec![
                b("high_pass", 120.0, 0.0, 0.8),
                b("bell", 2000.0, 2.0, 1.0),
                b("high_shelf", 9000.0, 1.0, 0.71),
            ],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The user's rule (docs/decisions.md): the EQ shows what really
    /// exists, the Mixer's channels; one DJ Booth tab, no spare decks.
    #[test]
    fn the_tabs_are_the_mixers_channels() {
        let ids: Vec<&str> = TARGETS.iter().map(|(t, _)| *t).collect();
        assert_eq!(ids, ["master", "bus", "dj"]);
        for name in ["dj", "booth", "dj-booth"] {
            assert_eq!(target(name), Some("dj"));
        }
        // Single decks stay addressable from the command line.
        assert_eq!(target("a"), Some("deck-a"));
        assert!(valid_target("mic-2"));
    }

    #[test]
    fn presets_round_trip_and_keep_unknown_fields() {
        for p in builtin_presets() {
            p.validate().unwrap();
            let st = p.to_state().unwrap();
            assert_eq!(st.preset, p.name);
            let back = Preset::from_state(&p.id, &p.name, &st);
            assert_eq!(back.bands.len(), p.bands.len(), "{}", p.name);
        }
        let text = r##"{"format":"patch-eq","version":1,"id":"x","name":"X","created":"2026-09-27T00:00:00Z",
            "bands":[{"type":"bell","frequency_hz":1000,"gain_db":3,"q":1,"color":"#fff"}]}"##;
        let p: Preset = serde_json::from_str(text).unwrap();
        let out = serde_json::to_value(&p).unwrap();
        assert_eq!(out["created"], "2026-09-27T00:00:00Z");
        assert_eq!(out["bands"][0]["color"], "#fff");
        // An input gain loads, saves back, and is left out of files without one.
        let p: Preset = serde_json::from_str(
            r#"{"format":"patch-eq","version":1,"id":"x","name":"X","gain_db":-6.2,"bands":[]}"#,
        )
        .unwrap();
        let st = p.to_state().unwrap();
        assert_eq!(st.gain_db, -6.2);
        assert!(st.active(), "a gain alone changes the sound");
        assert_eq!(Preset::from_state("x", "X", &st).gain_db, -6.2);
        assert!(serde_json::to_value(&builtin_presets()[0]).unwrap().get("gain_db").is_none());
        assert!(Preset { gain_db: -40.0, ..p }.validate().is_err());
        assert!(
            serde_json::from_str::<Preset>(
                r#"{"format":"patch-eq","version":2,"id":"x","name":"X","bands":[]}"#
            )
            .unwrap()
            .validate()
            .is_err()
        );
    }

    #[test]
    fn defaults_are_flat_and_eight_bands() {
        let s = EqState::default();
        assert_eq!(s.bands.len(), BANDS);
        assert!(!s.active(), "a fresh EQ changes nothing");
        let mut t = s.clone();
        t.bands[3].gain_db = 3.0;
        assert!(t.active());
        t.bypass = true;
        assert!(!t.active());
    }

    #[test]
    fn stored_bands_are_clamped() {
        let s = EqState {
            bypass: false,
            bands: vec![EqBand {
                freq: 1e9,
                gain_db: -99.0,
                q: f32::NAN,
                ..Default::default()
            }],
            ..Default::default()
        }
        .normalized();
        assert_eq!(s.bands.len(), BANDS);
        assert_eq!(
            (s.bands[0].freq, s.bands[0].gain_db, s.bands[0].q),
            (20_000.0, -18.0, 1.0)
        );
    }
}
