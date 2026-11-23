use crate::Provenance;

pub type NodeId = &'static str;

#[derive(Debug, Clone)]
pub struct EngineStatus {
    pub backend: String,
    pub sample_rate: u32,
    pub buffer: u32,
    /// PipeWire server xruns, when measured. Engine callback counters are separate.
    pub xruns: Option<u32>,
    pub dsp_load: f32,
}

impl EngineStatus {
    pub fn latency_ms(&self) -> f32 {
        self.buffer as f32 / self.sample_rate as f32 * 1000.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
}

#[derive(Debug, Clone)]
pub struct PortGroup {
    /// "Analog", "ADAT", "S/PDIF", "Mic/Inst", …
    pub name: &'static str,
    pub direction: Direction,
    pub ports: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Device {
    pub id: &'static str,
    pub name: &'static str,
    pub bus: &'static str,
    pub sample_rate: u32,
    pub clock: &'static str,
    pub groups: Vec<PortGroup>,
    pub mock: bool,
}

impl Device {
    pub fn channels(&self, dir: Direction) -> usize {
        self.groups
            .iter()
            .filter(|g| g.direction == dir)
            .map(|g| g.ports.len())
            .sum()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Source,
    Processor,
    Destination,
}

/// What a node *is*. Drives the contextual inspector.
#[derive(Debug, Clone)]
pub enum NodeRole {
    /// Physical capture, e.g. a USB microphone or interface inputs.
    HardwareIn { device: &'static str },
    /// Physical playback.
    HardwareOut { device: &'static str },
    /// An application stream (Firefox, Spotify, a game).
    AppStream {
        process: &'static str,
        rules: Vec<String>,
    },
    /// Desktop/system sounds.
    System,
    /// DSP processor.
    Processor { params: Vec<(&'static str, String)> },
    /// Virtual sink or recorder.
    Virtual { purpose: &'static str },
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: NodeId,
    pub name: &'static str,
    pub kind: NodeKind,
    pub role: NodeRole,
    /// Arbitrary channel counts: 1, 2, 8, 18, 64 …
    pub channels: u16,
}

#[derive(Debug, Clone)]
pub struct Route {
    pub from: NodeId,
    pub to: NodeId,
    pub channels: u16,
}

/// A mixer strip.
#[derive(Debug, Clone)]
pub struct Strip {
    pub node: NodeId,
    pub name: &'static str,
    pub gain_db: f32,
    /// Whether this strip can boost to +6 dB (0 dB sits below the top).
    pub headroom: bool,
    pub mute: bool,
    pub solo: bool,
    pub master: bool,
}

/// One send to a bus: its level (dB; None = off) and whether it's taken
/// before the fader.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Send {
    pub db: Option<f32>,
    pub pre: bool,
}

/// A console strip's knobs (Patch selected in the bar), as the engine has
/// them now.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct StripControls {
    /// Input trim, dB; None while the engine can't trim (an older one).
    pub gain_db: Option<f32>,
    /// EQ and pan exist (not on the DJ Booth: its decks have the DJ EQ).
    pub tone: bool,
    pub high_db: f32,
    pub mid_db: f32,
    pub low_db: f32,
    /// −1 (left) … 1 (right).
    pub pan: f32,
    /// To AUX 1, AUX 2, FX 1, FX 2; None while the engine has no buses.
    pub sends: Option<[Send; 4]>,
    /// Why the sends do nothing (the knobs show greyed, with the reason).
    pub sends_off: Option<SendsOff>,
}

/// A strip whose sends do nothing, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendsOff {
    /// A microphone taken out of Patch Input and all sends.
    CaptureOff,
    /// A Remote voice (mix-minus): it feeds no send, so a call never hears
    /// itself back through AUX 1/2 or an FX return.
    Remote,
    /// A stream whose sound a mic lane took: that lane's number, and whether
    /// it is a call's far end ("Remote N") or a voice source ("Voice N").
    OnLane { lane: u8, far_end: bool },
}

/// A bus of the console: AUX 1, 2 (mixes of their own, to an output and a
/// recordable source) and FX 1, 2 (reverb and delay, returned into Main).
#[derive(Debug, Clone, PartialEq)]
pub struct BusStrip {
    /// "aux1", "aux2", "fx1", "fx2".
    pub id: &'static str,
    pub name: &'static str,
    pub gain_db: f32,
    pub mute: bool,
    /// Where it goes: "Scarlett · 3 + 4", "source only", "into Main".
    pub output: String,
    /// The effect's one control (FX): its label and 0…1.
    pub param: Option<(&'static str, f32)>,
}

/// A device the main output can play to, as the Mixer's output picker
/// lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputChoice {
    /// What `connect("output", id)` takes.
    pub id: &'static str,
    pub name: String,
    /// "Built-in", "USB", "Bluetooth", "HDMI" … (empty: unknown).
    pub kind: &'static str,
    /// Format as the device runs, e.g. "48 kHz · 2 ch · LDAC".
    pub detail: String,
    pub current: bool,
}

#[derive(Debug, Clone)]
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub summary: &'static str,
    /// Human-readable list of what applying it would change.
    pub changes: Vec<&'static str>,
    /// Depends on a plugin that does not exist yet.
    pub plugin_mock: Option<&'static str>,
}

/// EBU R128 figures for one stream.
#[derive(Debug, Clone, Copy, Default)]
pub struct LoudnessReading {
    pub momentary: f32,
    pub short_term: f32,
    pub integrated: f32,
    /// Loudness range, LU.
    pub range: f32,
    /// Maximum true peak since reset, dBTP.
    pub true_peak_max: f32,
}

/// How the meters are timed: which moment they show, and how that moment's
/// latency is known. Nothing here is invented: a level that isn't measured
/// is shown as what it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Timing {
    pub reference: crate::timing::MeterReference,
    /// Where the readings come from, e.g. "Patch engine".
    pub source: String,
    /// Path latency applied (OUTPUT) — 0 in ENGINE.
    pub latency_ms: f32,
    pub level: crate::timing::LatencySource,
    /// Per-device correction by ear, ms.
    pub trim_ms: f32,
}

/// Silence floor used for meters, in dBFS.
pub const SILENCE_DB: f32 = -120.0;

/// One block of meter data: instantaneous sample peaks in dBFS, as an engine
/// reports them. Ballistics (release, peak hold) are a display concern and
/// live in the UI, so every surface can meter the same signal its own way.
#[derive(Debug, Clone, Default)]
pub struct MeterFrame {
    /// Post-fader peak per mixer strip (same order as `strips()`).
    pub strips_db: Vec<f32>,
    /// Measured VU per strip, when available (same order).
    pub strips_vu: Option<Vec<f32>>,
    /// Peak per strip and side (L, R), when the provider measures sides.
    pub strips_lr: Option<Vec<[f32; 2]>>,
    /// Strip levels are after their fader (as heard in the mix). False:
    /// deck strips are pre-fader and a view applies the fader itself.
    pub strips_post_fader: bool,
    /// A/B/C/D post-EQ input peaks, before channel fader, crossfader and mute.
    /// None when the provider cannot measure these independently.
    pub deck_input_db: Option<[[f32; 2]; 4]>,
    /// What the Meter's input section measures, per channel.
    pub input: crate::channels::Levels,
    /// The output the listener hears, per channel.
    pub output: crate::channels::Levels,
    /// Master true peak in dBTP.
    pub true_peak_db: f32,
    /// Spectrum band levels in dBFS, low to high. Empty when not measured.
    pub spectrum_db: Vec<f32>,
    /// Measured BS.1770-4 / EBU R128 loudness of the output, when available.
    pub loudness: Option<LoudnessReading>,
    /// Delay applied so the output meters match what you hear (ms).
    pub sync_ms: Option<f32>,
    /// How the meters are timed, when the provider knows.
    pub timing: Option<Timing>,
    /// The console's buses as they leave (AUX 1, 2, FX 1, 2): peak dB per
    /// side. Empty without them.
    pub buses_db: Vec<[f32; 2]>,
    /// What the input and output meters measure, e.g. "Mic", "cliamp".
    pub input_label: String,
    /// Each source the input sums, when there are several ("3 sources"
    /// in the label): the Meter lists them on hover.
    pub input_sources: Vec<String>,
    pub output_label: String,
}

pub trait AudioProvider {
    fn provenance(&self) -> Provenance;
    /// Explicit Patch mode: expose hardware assignments and routing controls.
    fn professional_routing(&self) -> bool { false }
    fn engine(&self) -> EngineStatus;
    fn devices(&self) -> Vec<Device>;
    fn nodes(&self) -> Vec<Node>;
    fn routes(&self) -> Vec<Route>;
    fn strips(&self) -> Vec<Strip>;
    /// Whether a mixer row is an application group that can be named or kept
    /// on the desk after its final stream closes.
    fn is_application_strip(&self, _node: &str) -> bool { false }
    /// Whether a shown strip currently has a live source behind it.
    fn strip_available(&self, _node: &str) -> bool { true }
    /// Whether an application group is kept visible while it has no streams.
    fn application_strip_kept(&self, _node: &str) -> bool { false }
    /// Set an application's display name (empty restores the live name) and
    /// whether its console slot remains visible while silent.
    fn configure_application_strip(&mut self, _node: &str, _name: &str, _keep: bool) -> Result<(), String> {
        Err("application strip settings are unavailable".into())
    }
    /// The engine channel behind a strip (a mic, an app with a channel of
    /// its own, the engine's voices), where a plugin can be inserted.
    fn strip_channel(&self, _node: &str) -> Option<String> {
        None
    }
    /// Give an app strip an engine channel of its own (as its knobs do)
    /// and return it.
    fn assign_channel(&mut self, node: &str) -> Result<String, String> {
        self.strip_channel(node).ok_or_else(|| "this strip has no engine channel".into())
    }
    fn presets(&self) -> Vec<Preset>;
    fn active_preset(&self) -> Option<&'static str>;

    fn set_gain(&mut self, node: &str, db: f32);
    fn set_mute(&mut self, node: &str, mute: bool);
    fn set_solo(&mut self, node: &str, solo: bool) -> Result<(), String>;
    fn apply_preset(&mut self, id: &str);

    /// Instantaneous meter values at time `t` (seconds).
    fn meters(&mut self, t: f64) -> MeterFrame;

    /// Re-read external state. Returns true when the graph changed.
    fn refresh(&mut self) -> bool {
        false
    }

    /// For a playback stream: does it play through the Patch bus?
    /// `None` for nodes that aren't streams.
    fn through_patch(&self, _node: &str) -> Option<bool> {
        None
    }

    /// Route a playback stream through the Patch bus, or straight to the output.
    fn route_through_patch(&mut self, _node: &str, _through: bool) -> Result<(), String> {
        Err("not supported by this provider".into())
    }

    /// Node id of a playback stream matching an application name.
    fn find_stream(&self, _app: &str) -> Option<&'static str> {
        None
    }

    /// Where `node` could be sent: (destination id, name, is current target).
    fn targets(&self, _node: &str) -> Vec<(&'static str, &'static str, bool)> {
        vec![]
    }

    /// Where the main output can play: one entry per device, the current
    /// one marked. Chosen with `connect("output", id)`.
    fn main_outputs(&self) -> Vec<OutputChoice> {
        self.targets("output")
            .into_iter()
            .map(|(id, name, current)| OutputChoice { id, name: name.to_string(), kind: "", detail: String::new(), current })
            .collect()
    }

    /// The knobs a strip has in the Advanced Mixer (gain, EQ, pan); None
    /// when it has none (the DJ Booth, Main).
    fn strip_controls(&self, _node: &str) -> Option<StripControls> {
        None
    }

    /// Turn one: "gain", "high", "mid", "low" (dB), "pan" (−1 … 1),
    /// "send1" … "send4" (dB; −60 or below: off) or "pre1" … "pre4" (0, 1).
    fn set_strip_control(&mut self, _node: &str, _control: &str, _value: f32) -> Result<(), String> {
        Err("not supported by this provider".into())
    }

    /// The console's buses (Patch selected in the bar); empty otherwise.
    fn buses(&self) -> Vec<BusStrip> {
        vec![]
    }

    /// A bus's fader (dB), mute, or its effect's control (0 … 1).
    fn set_bus(&mut self, _id: &str, _db: Option<f32>, _mute: Option<bool>, _param: Option<f32>) -> Result<(), String> {
        Err("not supported by this provider".into())
    }

    /// Send `from` to `to` (replacing its current target).
    fn connect(&mut self, _from: &str, _to: &str) -> Result<(), String> {
        Err("not supported by this provider".into())
    }

    /// Nudge meter/ear sync for the current output; returns the trim in ms.
    fn trim_sync(&mut self, _ms: f64) -> Result<f64, String> {
        Err("not supported by this provider".into())
    }

    /// Points that can be recorded: (id, label, capture spec). The spec is
    /// `device:<pipewire source>` or `stream:<index>`.
    fn recordable(&self) -> Vec<(String, String, String)> {
        vec![]
    }

    /// Something the provider did on its own that the user should know
    /// about (e.g. following a system output switch). Taken once.
    fn take_notice(&mut self) -> Option<String> {
        None
    }

    /// Restart integrated loudness, LRA and max true peak.
    fn reset_loudness(&mut self) {}

    /// Native deck playheads as heard (seconds, source frames per output
    /// frame), from the engine's stamped blocks; empty without the engine.
    /// The DJ mixer (crossfader 0…1, channel faders 0…1) as the engine has
    /// it now, when there is one.
    fn dj_mixer(&self) -> Option<(f32, [f32; 4])> {
        None
    }
    /// The decks' EQ knobs (high, mid, low, filter per deck A, B) as the
    /// engine moves them, every frame (None without the engine).
    fn dj_eq(&self) -> Option<[[f32; 4]; 4]> {
        None
    }
    /// The decks' stem gains (NaN: none attached) and levels, canonical
    /// order (vocals, drums, bass, other), every frame (None without the
    /// engine).
    fn dj_stems(&self) -> Option<([[f32; 4]; 4], [[f32; 4]; 4])> {
        None
    }
    fn deck_playheads(&self) -> Vec<Option<(f64, f64)>> {
        vec![]
    }
    /// Per native deck, which track its playhead is in: a count that
    /// changes on every load, stamped with the playhead (empty without
    /// the engine).
    fn deck_loads(&self) -> Vec<u32> {
        vec![]
    }

    /// Which moment the meters show (OUTPUT: what you hear now).
    fn set_meter_reference(&mut self, _r: crate::timing::MeterReference) {}

    /// Undo everything Patch changed in the audio server.
    fn restore_routing(&mut self) -> Result<(), String> {
        Ok(())
    }
}
