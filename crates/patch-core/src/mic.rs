//! Portable microphone processing profiles. No device or hardware gain is stored here.

/// What the DJ (or the podcaster) set; each stage on or off on its own.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    pub on: bool,
    /// High-pass corner, Hz (0: off).
    pub hpf_hz: f32,
    /// Gate threshold, dBFS (below it the room is turned down); `None`: off.
    pub gate_db: Option<f32>,
    /// De-esser amount 0…1 (0: off).
    pub deess: f32,
    /// Compressor amount 0…1 (0: off): from gentle levelling to radio.
    pub comp: f32,
    /// Downward 2:1 expansion below this threshold; None bypasses it.
    pub expander_db: Option<f32>,
    /// Per-band noise attenuation threshold; None bypasses the filter bank.
    pub noise_db: Option<f32>,
    pub input_gain_db: f32,
    pub output_gain_db: f32,
}

impl Default for Settings {
    /// A voice preset that suits most mics in most rooms.
    fn default() -> Self {
        Settings {
            on: true,
            hpf_hz: 80.0,
            gate_db: Some(-50.0),
            deess: 0.4,
            comp: 0.5,
            expander_db: None,
            noise_db: None,
            input_gain_db: 0.0,
            output_gain_db: 0.0,
        }
    }
}

impl Settings {
    pub const OFF: Settings = Settings {
        on: false,
        hpf_hz: 0.0,
        gate_db: None,
        deess: 0.0,
        comp: 0.0,
        expander_db: None,
        noise_db: None,
        input_gain_db: 0.0,
        output_gain_db: 0.0,
    };
}

pub const CHANNELS: [&str; 16] = [
    "mic-1", "mic-2", "mic-3", "mic-4", "mic-5", "mic-6", "mic-7", "mic-8", "mic-9", "mic-10",
    "mic-11", "mic-12", "mic-13", "mic-14", "mic-15", "mic-16",
];

/// Canonical input lanes; reject aliases, zero and out-of-range lanes.
pub fn channel(name: &str) -> bool {
    CHANNELS.contains(&name)
}

/// A remote voice: another application's playback (a call, an X Space)
/// taken into a mic lane, so the far end gets the voice chain like a guest
/// on a broadcast console. `stream:<key>` is one playing stream (its lane
/// key, see [`crate::stream::key`]); `app-1`…`app-4` an application channel.
pub fn remote_source(source: &str) -> bool {
    source.starts_with("stream:") || crate::eq::APP_CHANNELS.contains(&source)
}

/// The starting profile a remote voice gets (see [`factory_profiles`]).
pub const REMOTE_PROFILE: &str = "Remote voice";

/// A whole device and one of its ports overlap; distinct ports do not.
/// Used only to choose a free input, never to forbid explicit user routing.
pub fn sources_overlap(a: &str, b: &str) -> bool {
    fn split(name: &str) -> (&str, Option<&str>) {
        name.split_once(':')
            .map_or((name, None), |(device, port)| (device, Some(port)))
    }
    let (a_device, a_port) = split(a);
    let (b_device, b_port) = split(b);
    a_device == b_device && (a_port.is_none() || b_port.is_none() || a_port == b_port)
}

impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        let range = |x: f32, lo: f32, hi: f32| x.is_finite() && (lo..=hi).contains(&x);
        if !range(self.input_gain_db, -24.0, 24.0)
            || !range(self.output_gain_db, -24.0, 24.0)
            || !range(self.hpf_hz, 0.0, 300.0)
            || self.gate_db.is_some_and(|x| !range(x, -80.0, -20.0))
            || self.noise_db.is_some_and(|x| !range(x, -80.0, -20.0))
            || self.expander_db.is_some_and(|x| !range(x, -80.0, -20.0))
            || !range(self.deess, 0.0, 1.0)
            || !range(self.comp, 0.0, 1.0)
        {
            return Err("invalid voice processing parameters".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Profile {
    pub format: String,
    pub version: u32,
    pub name: String,
    pub voice: Settings,
    pub eq: crate::eq::EqState,
}

impl Profile {
    pub fn new(name: &str, voice: Settings, eq: crate::eq::EqState) -> Self {
        Self {
            format: "patch-mic".into(),
            version: 1,
            name: name.into(),
            voice,
            eq,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.format != "patch-mic" || self.version != 1 {
            return Err("unsupported microphone profile format/version".into());
        }
        if self.name.trim().is_empty()
            || self.name.len() > 100
            || self.name.chars().any(char::is_control)
        {
            return Err("profile name must contain 1–100 bytes without control characters".into());
        }
        self.voice.validate()?;
        if self.eq.bands.len() != crate::eq::BANDS
            || self.eq.bands.iter().any(|b| *b != b.clamped())
        {
            return Err("invalid microphone EQ bands".into());
        }
        Ok(())
    }
}

/// Conservative starting points, deliberately independent of device gain.
pub fn factory_profiles() -> Vec<Profile> {
    let mut out = Vec::new();
    for (name, hpf, gate, deess, comp, warmth, clarity) in [
        ("Natural", 60.0, None, 0.0, 0.0, 0.0, 0.0),
        ("Broadcast", 80.0, Some(-55.0), 0.5, 0.7, 2.0, 1.0),
        ("Podcast", 80.0, Some(-55.0), 0.4, 0.4, 1.0, 0.0),
        ("Streaming", 90.0, Some(-50.0), 0.5, 0.6, 0.0, 1.0),
        ("Warm voice", 60.0, None, 0.3, 0.3, 2.0, -1.0),
        ("Clear voice", 100.0, None, 0.5, 0.3, -1.0, 2.0),
        ("Vocal recording", 60.0, None, 0.2, 0.15, 0.0, 0.0),
        ("Noise reduction", 100.0, Some(-45.0), 0.2, 0.0, 0.0, 0.0),
        // A far-end voice arrives already processed (Opus, the caller's own
        // auto gain and noise suppression): no gate or expander to chop it,
        // a firmer compressor to level loud and quiet speakers, a little
        // presence for the codec's dullness.
        (REMOTE_PROFILE, 100.0, None, 0.3, 0.6, 0.0, 1.0),
        ("Custom", 0.0, None, 0.0, 0.0, 0.0, 0.0),
    ] {
        let mut eq = crate::eq::EqState::default();
        eq.bands[1].gain_db = warmth;
        eq.bands[4].gain_db = clarity;
        let mut voice = Settings {
            on: true,
            hpf_hz: hpf,
            gate_db: gate,
            deess,
            comp,
            ..Settings::OFF
        };
        if name == "Noise reduction" {
            voice.noise_db = Some(-55.0);
        }
        out.push(Profile::new(name, voice, eq));
    }
    out
}

/// Future hosted processors use shared graph slots; application adapters live
/// alongside device capabilities and never masquerade as DSP stages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InsertionPoint {
    BeforeVoice,
    BeforeEq,
    AfterEq,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HardwareCapabilities {
    pub device_id: String,
    pub gain_db: Option<(f32, f32)>,
    pub onboard_processors: Vec<String>,
}

/// Control-plane only. An adapter must verify writes against readable device
/// state. No adapter is required to run Mic, and none is implicitly selected.
pub trait HardwareAdapter {
    fn detect(&self, device_id: &str) -> Result<Option<HardwareCapabilities>, String>;
    fn set_gain_verified(&mut self, device_id: &str, gain_db: f32) -> Result<f32, String>;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_sources_are_streams_and_app_channels_not_devices() {
        assert!(remote_source("stream:48989:48988:72202"));
        assert!(remote_source("app-2"));
        assert!(!remote_source("alsa_input.usb-046d_0825_D1D6B6E0-02.mono-fallback"));
        assert!(!remote_source("app-9"));
        let remote = factory_profiles().into_iter().find(|p| p.name == REMOTE_PROFILE).unwrap();
        assert_eq!(remote.voice.gate_db, None);
        assert_eq!(remote.voice.expander_db, None);
    }

    #[test]
    fn portable_profiles_round_trip_and_reject_invalid_data() {
        for p in factory_profiles() {
            p.validate().unwrap();
            let decoded: Profile =
                serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
            assert_eq!(p, decoded);
        }
        let mut p = factory_profiles().remove(0);
        p.voice.comp = f32::NAN;
        assert!(p.validate().is_err());
        p.voice.comp = 0.0;
        p.version = 2;
        assert!(p.validate().is_err());
    }
    #[test]
    fn canonical_channels_only() {
        for n in 1..=16 {
            assert!(channel(&format!("mic-{n}")));
        }
        for s in ["mic-0", "mic-17", "mic-01", "master", "mic-1/../foo"] {
            assert!(!channel(s));
        }
    }
}

#[cfg(test)]
mod channel_tests {
    #[test]
    fn free_input_selection_does_not_duplicate_a_stereo_device() {
        assert!(super::sources_overlap("input", "input:capture_FL"));
        assert!(super::sources_overlap("input:capture_FL", "input"));
        assert!(super::sources_overlap(
            "input:capture_FL",
            "input:capture_FL"
        ));
        assert!(!super::sources_overlap(
            "input:capture_FL",
            "input:capture_FR"
        ));
        assert!(!super::sources_overlap("input", "other:capture_FL"));
    }
}
