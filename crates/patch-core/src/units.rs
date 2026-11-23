//! Units: the functional applications inside Patch.
//!
//! Patch is the application. A Unit is a substantial, coherent workflow
//! (Mixer, Router, Meter, Tune, …) with one or more surfaces (real Wayland
//! windows). Small features live inside a Unit rather than becoming Units.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum UnitKind {
    Mixer,
    Router,
    Recorder,
    Meter,
    Device,
    DjBooth,
    ClubBooth,
    Player,
    Sequencer,
    Broadcast,
    Controller,
    Plugins,
    Session,
    Analyzer,
    Network,
    Library,
    Tune,
    Mic,
    Eq,
    Performance,
    Visualiser,
    Pattern,
    Tidal,
}

impl UnitKind {
    pub const ALL: [UnitKind; 23] = [
        UnitKind::Mixer,
        UnitKind::Router,
        UnitKind::Recorder,
        UnitKind::Meter,
        UnitKind::Device,
        UnitKind::DjBooth,
        UnitKind::ClubBooth,
        UnitKind::Player,
        UnitKind::Sequencer,
        UnitKind::Broadcast,
        UnitKind::Controller,
        UnitKind::Plugins,
        UnitKind::Session,
        UnitKind::Analyzer,
        UnitKind::Network,
        UnitKind::Library,
        UnitKind::Tune,
        UnitKind::Mic,
        UnitKind::Eq,
        UnitKind::Performance,
        UnitKind::Visualiser,
        UnitKind::Pattern,
        UnitKind::Tidal,
    ];

    pub fn name(self) -> &'static str {
        match self {
            UnitKind::Mixer => "Mixer",
            UnitKind::Router => "Router",
            UnitKind::Recorder => "Recorder",
            UnitKind::Meter => "Meter",
            UnitKind::Device => "Device",
            UnitKind::DjBooth => "DJ Booth",
            UnitKind::ClubBooth => "Club Booth",
            UnitKind::Player => "Player",
            UnitKind::Sequencer => "Sequencer",
            UnitKind::Broadcast => "Broadcast",
            UnitKind::Controller => "Controller",
            UnitKind::Plugins => "Plugins",
            UnitKind::Session => "Session",
            UnitKind::Analyzer => "Analyzer",
            UnitKind::Network => "Network",
            UnitKind::Library => "Library",
            UnitKind::Tune => "Tune",
            UnitKind::Mic => "Mic",
            UnitKind::Eq => "EQ",
            UnitKind::Performance => "Performance",
            UnitKind::Visualiser => "Visualiser",
            UnitKind::Pattern => "Pattern",
            UnitKind::Tidal => "Tidal",
        }
    }

    /// Where the controller lists it: Audio, DJ Booth or Tools.
    pub fn group(self) -> &'static str {
        match self {
            UnitKind::Mixer
            | UnitKind::Router
            | UnitKind::Recorder
            | UnitKind::Player
            | UnitKind::Meter
            | UnitKind::Device
            | UnitKind::Eq
            | UnitKind::Mic => "Audio",
            UnitKind::Sequencer
            | UnitKind::DjBooth
            | UnitKind::ClubBooth
            | UnitKind::Library
            | UnitKind::Visualiser
            | UnitKind::Broadcast => "DJ Booth",
            _ => "Tools",
        }
    }

    /// What the Unit gathers, for the controller.
    pub fn scope(self) -> &'static str {
        match self {
            UnitKind::Mixer => "strips, gain, pan, EQ, dynamics, buses, sends, cue, master",
            UnitKind::Router => "sources, destinations, patching, routing matrix",
            UnitKind::Recorder => "takes, formats, multitrack capture",
            UnitKind::Meter => "peak, true peak, VU, loudness, spectrum, sync",
            UnitKind::Device => "interfaces, ports, clock, rate, buffer",
            UnitKind::DjBooth => "decks, crossfader, cueing, Auto Mix",
            UnitKind::ClubBooth => "the decks and mixer drawn as the gear in a club booth",
            UnitKind::Player => "playback of files and streams",
            UnitKind::Sequencer => "drum synthesis, step patterns, swing, probability and repeats",
            UnitKind::Broadcast => "streaming and broadcast outputs",
            UnitKind::Controller => "MIDI and HID controllers",
            UnitKind::Plugins => "native VST3 hosting, plugin parameters and master inserts",
            UnitKind::Session => "scenes, snapshots, presets, recovery",
            UnitKind::Analyzer => "spectrum, phase, correlation, latency tests",
            UnitKind::Network => "sends and receives on the local network: RTP (AES67 L24) with SAP, VBAN",
            UnitKind::Library => "tracks, stations, analysis",
            UnitKind::Tune => "output identity, device correction, protection, calibration",
            UnitKind::Mic => "universal inputs, voice processing, profiles and calibration",
            UnitKind::Eq => "intentional tonal shaping: the master EQ",
            UnitKind::Performance => {
                "speed, CPU, GPU and memory: Patch against the native audio layer"
            }
            UnitKind::Visualiser => "text effects played to the music, for a screen or a projector",
            UnitKind::Pattern => "algorithmic, live-coded control of any unit",
            UnitKind::Tidal => {
                "live-coded music: TidalCycles, native, with its own instrument and the Visualiser under the code"
            }
        }
    }
}

/// Whether a Unit exists yet, and on what data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitStatus {
    /// Implemented and running on real audio.
    Live,
    /// Implemented, but running on simulated data.
    Mock,
    /// Declared so the architecture reserves its place; not built.
    Planned,
}

impl UnitStatus {
    pub fn label(self) -> &'static str {
        match self {
            UnitStatus::Live => "live",
            UnitStatus::Mock => "mock",
            UnitStatus::Planned => "planned",
        }
    }
}

#[derive(Debug, Clone)]
pub struct UnitSpec {
    pub kind: UnitKind,
    /// Surface ids belonging to this Unit (may be empty for planned Units).
    pub surfaces: Vec<&'static str>,
    pub implemented: bool,
}
