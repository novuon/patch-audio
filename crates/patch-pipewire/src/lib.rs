//! `PipeWireAudioProvider`: Patch's first real provider.
//!
//! It talks to PipeWire through its standard tools (`pactl`, `pw-dump`,
//! `pw-metadata`, `parec`) rather than linking libpipewire, which keeps the
//! prototype pure Rust and easy to replace with a proper engine client later.
//!
//! Routing lives outside this process: the Patch bus is a null sink in the
//! audio server, and the program path from it to the output is the Patch
//! engine (a detached process, `patch-engine`). Streams are moved with
//! `move-sink-input`. If Patch's UI crashes or quits, audio keeps playing;
//! if the engine isn't running, the bus falls back to a plain loopback.
//!
//! Program and bus meters come from the engine, stamped with the graph
//! clock ([`engine_feed`]). Per-stream meters still use `parec` taps until
//! streams become engine channels. Microphones are never recorded.

pub mod engine_feed;
mod meter;
mod contextual;
mod app_strips;
mod held_strips;
mod pactl;
mod pulse_writes;
mod routes;
pub mod sync;
mod system_sounds;

use meter::Tap;
use pactl::{Pactl, SinkInfo, StreamInfo};
use pulse_writes::Write as PulseWrite;
use patch_model::Provenance;
use patch_model::audio::*;
use patch_model::channels::Levels;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Sink name of the Patch bus.
pub const BUS: &str = "patch";
const DECK_A: &str = "engine.deck-a";
const DECK_B: &str = "engine.deck-b";
const DJ_MIXER: &str = "engine.dj-mixer";

/// The engine's DJ nodes: the Router shows them only with the DJ module.
const DJ_NODES: [&str; 4] = [DECK_A, DECK_B, DJ_MIXER, CUE];

/// The DJ module is installed (the setup file, read again when it changes).
fn dj_installed() -> bool {
    thread_local! {
        static SETUP: std::cell::RefCell<patch_core::setup::Watched> = Default::default();
    }
    SETUP.with(|s| s.borrow_mut().get().dj)
}
const PROGRAM: &str = "engine.program";
const CUE: &str = "engine.cue";
const CUE_OFF: &str = "engine.cue.off";
/// The console's buses (Patch selected): AUX 1, 2 and FX 1, 2.
const BUS_IDS: [&str; 4] = ["aux1", "aux2", "fx1", "fx2"];
/// An AUX back to its virtual source only (no physical ports).
const AUX_OFF: [&str; 2] = ["aux1.off", "aux2.off"];
const SYSTEM_SOUNDS: &str = "system-sounds";
const SYSTEM_SOUNDS_SINK: &str = "patch.system-sounds";

/// Leak a string once and reuse it: the model uses `&'static str` ids and the
/// set of distinct names in a session is small.
fn intern(s: &str) -> &'static str {
    static TABLE: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut t = TABLE.lock().unwrap();
    if let Some(x) = t.iter().find(|x| **x == s) {
        return x;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    t.push(leaked);
    leaked
}

pub struct PipeWireAudioProvider {
    pw: Pactl,
    pulse_writes: pulse_writes::PulseWrites,
    sinks: Vec<SinkInfo>,
    streams: Vec<StreamInfo>,
    inputs: Vec<pactl::InputInfo>,
    /// Applications with an engine channel of their own (name → channel),
    /// re-read with the graph: their Advanced Mixer knobs.
    app_channels: HashMap<String, String>,
    app_labels: HashMap<String, String>,
    hidden_streams: HashSet<String>,
    channel_sinks: HashMap<u32, String>,
    professional: bool,
    playback_ports: Vec<String>,
    rate: u32,
    quantum: u32,
    version: String,
    /// Where the Patch bus plays to, when it exists.
    bus_target: Option<String>,
    taps: HashMap<String, Tap>,
    signature: String,
    latency: sync::LatencyWatch,
    trims: HashMap<String, f64>,
    /// Timestamped raw readings, oldest first (≈ 3 s).
    history: VecDeque<Raw>,
    /// Per tap: time of the newest reading already shown.
    shown: HashMap<String, Instant>,
    /// node.name of streams the user routed through Patch. Sticky: if
    /// something else moves them (the system bar's output switch moves every
    /// app stream), they go back onto the bus. Saved across restarts.
    followers: std::collections::BTreeSet<String>,
    /// Last seen system default output, to notice the bar's output switch.
    last_default: Option<String>,
    notice: Option<String>,
    engine: engine_feed::EngineFeed,
    /// Latest graph snapshot from the watcher thread.
    watch: Arc<Mutex<Option<GraphSnap>>>,
    /// Last time the supervisor (re)started the engine.
    engine_restart: Option<Instant>,
    manage_engine: bool,
    reference: patch_model::timing::MeterReference,
    system_sounds_gain_db: f32,
    system_sounds_mute: bool,
    system_sounds_target: Option<String>,
    app_strip_preferences: app_strips::Store,
    /// Which sources the Meter's input counts now (see [`InputHold`]).
    input_hold: InputHold,
    /// Stream indexes whose Pulse volume was already returned to unity
    /// after their engine channel took the fader.
    channel_unity: HashSet<u32>,
    /// When each application strip node was first seen, and when a strip
    /// preference's stream went away: a reopened stream takes its name.
    node_seen: HashMap<String, Instant>,
    orphaned: HashMap<String, Instant>,
    /// Orphans found at startup went at an unknown time: never "just now".
    adoption_primed: bool,
    /// Application strips in their places, a departed one held a while.
    held: held_strips::Held,
}

/// How long a renamed stream's name waits for the stream that replaces it.
const REOPEN_WINDOW: std::time::Duration = std::time::Duration::from_secs(120);

/// What the graph looks like, read off the UI thread.
struct GraphSnap {
    read_at: Instant,
    source_indexes: HashMap<String, u32>,
    source_outputs: Vec<(u32, String, u32)>,
    app_channels: HashMap<String, String>,
    app_labels: HashMap<String, String>,
    hidden_streams: HashSet<String>,
    channel_sinks: HashMap<u32, String>,
    playback_ports: Vec<String>,
    professional: bool,
    inputs: Vec<pactl::InputInfo>,
    sinks: Vec<SinkInfo>,
    streams: Vec<StreamInfo>,
    default: Option<String>,
    clock: Option<(u32, u32)>,
    loopback_target: Option<String>,
    system_sounds_target: Option<String>,
    /// What the System sounds sink itself says: (volume dB, muted). The strip
    /// shows this, not a remembered value.
    system_sounds_live: Option<(f32, bool)>,
}

impl GraphSnap {
    fn read(pw: &Pactl) -> GraphSnap {
        let read_at = Instant::now();
        let sinks = pw.sinks();
        let default = pw.default_sink();
        let system_sounds_target = ensure_system_sounds(pw, default.as_deref(), &sinks);
        let mut streams = pw.streams();
        if system_sounds_target.is_some() {
            let index = sinks
                .iter()
                .find(|s| s.name == SYSTEM_SOUNDS_SINK)
                .map(|s| s.index);
            if let Some(index) = index {
                for stream in streams.iter_mut().filter(|s| s.system_sound) {
                    if stream.sink != index
                        && pw.move_stream(stream.index, SYSTEM_SOUNDS_SINK).is_ok()
                    {
                        stream.sink = index;
                    }
                    // A notification can be created on Patch's default bus,
                    // whose flat-volume level is also the listening volume.
                    // Moving that stream to the dedicated event sink keeps
                    // its inherited (possibly zero) sink-input volume. The
                    // shared System sounds fader already controls that sink;
                    // retaining the inherited stream volume applies the main
                    // output level twice, and a zero-volume default mutes it.
                    // Normalize transient event streams so only the shared
                    // System sounds strip and final output set their levels.
                    if stream.sink == index
                        && (!stream.volume_db.is_finite() || stream.volume_db.abs() > 0.05)
                    {
                        pw.set_stream_volume(stream.index, 0.0);
                        stream.volume_db = 0.0;
                    }
                }
            }
        }
        let system_sounds_live = sinks
            .iter()
            .find(|s| s.name == SYSTEM_SOUNDS_SINK)
            .map(|s| (s.volume_db, s.mute));
        let apps = patch_engine::client::call(&serde_json::json!({"op": "eq_apps"})).unwrap_or_default();
        let channel_sinks = sinks.iter().filter(|s| s.name.starts_with("patch.stream-") || s.name.starts_with("patch.app-"))
            .map(|s| (s.index, s.name.clone())).collect();
        GraphSnap {
            read_at,
            source_indexes: pw.source_indexes(),
            source_outputs: pw.source_outputs(),
            app_channels: serde_json::from_value(apps["assigned"].clone()).unwrap_or_default(),
            app_labels: serde_json::from_value(apps["labels"].clone()).unwrap_or_default(),
            hidden_streams: serde_json::from_value(apps["hidden"].clone()).unwrap_or_default(),
            channel_sinks,
            playback_ports: pw.playback_ports(),
            professional: contextual::professional_selected(),
            inputs: pw.inputs(),
            sinks,
            streams,
            default,
            system_sounds_target,
            system_sounds_live,
            clock: pw.clock(),
            loopback_target: pw.bus_loopback_target(),
        }
    }

    /// A thread that reads the graph (~25 ms of pactl calls) when it
    /// changes: devices, streams, volumes, links. Every 10 s regardless;
    /// every second if the event listeners can't run.
    fn watch() -> Arc<Mutex<Option<GraphSnap>>> {
        let slot: Arc<Mutex<Option<GraphSnap>>> = Arc::default();
        let s = slot.clone();
        let _ = std::thread::Builder::new()
            .name("patch-graph".into())
            .spawn(move || {
                let pw = Pactl::new();
                let events =
                    patch_engine::events::Watch::new(patch_engine::events::Interest::Everything);
                let every = std::time::Duration::from_secs(if events.live { 10 } else { 1 });
                loop {
                    let snap = GraphSnap::read(&pw);
                    let hidden = snap.hidden_streams.clone();
                    *s.lock().unwrap() = Some(snap);
                    // A new browser stream is hidden until it has carried a
                    // second of signal. The engine then shows it without any
                    // graph event: while one waits, ask it (one cheap call)
                    // four times a second, or the strip took up to 10 s.
                    let until = Instant::now() + every;
                    loop {
                        let left = until.saturating_duration_since(Instant::now());
                        let step = if hidden.is_empty() { left } else { left.min(std::time::Duration::from_millis(250)) };
                        if events.wait(step) || Instant::now() >= until {
                            break;
                        }
                        let apps = patch_engine::client::call(&serde_json::json!({"op": "eq_apps"})).unwrap_or_default();
                        let now: HashSet<String> = serde_json::from_value(apps["hidden"].clone()).unwrap_or_default();
                        if now != hidden {
                            break;
                        }
                    }
                }
            });
        slot
    }
}

fn ensure_system_sounds(pw: &Pactl, target: Option<&str>, sinks: &[SinkInfo]) -> Option<String> {
    let mut sink = sinks.iter().find(|s| s.name == SYSTEM_SOUNDS_SINK).cloned();
    // Missing from a listing that may have failed: only a server that
    // answers "not there" gets a new one (else a duplicate sink).
    if sink.is_none() && pw.sink_exists(SYSTEM_SOUNDS_SINK) == Some(false) {
        let rate = target
            .and_then(|t| sinks.iter().find(|s| s.name == t))
            .map(|s| s.rate)
            .unwrap_or(48_000);
        if pw.create_system_sounds_sink(rate).is_ok() {
            pw.set_sink_volume(SYSTEM_SOUNDS_SINK, system_sounds::load().0);
            let mute = system_sounds::load().1;
            pw.set_sink_mute(SYSTEM_SOUNDS_SINK, mute);
            pw.set_source_mute("patch.system-sounds.monitor", mute);
            sink = pw
                .sinks()
                .into_iter()
                .find(|s| s.name == SYSTEM_SOUNDS_SINK);
        }
    }
    sink?;
    let target = target.filter(|t| !t.starts_with("patch.deck-"))?;
    // Exactly one loopback, to the default output. Every reload is itself a
    // graph event (which reads the graph again) and a glitch in the audio:
    // only when the target really changed (or it is an older, active one:
    // replaced once), and at most once in 10 s.
    let loaded = pw.system_sounds_loopbacks();
    if loaded.len() == 1 && loaded[0].1 == target && loaded[0].2 {
        return Some(target.to_string());
    }
    static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    if last.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(10)) {
        return Some(target.to_string());
    }
    *last = Some(std::time::Instant::now());
    for (index, _, _) in loaded {
        pw.unload_module(index);
    }
    pw.create_system_sounds_loopback(target).ok()?;
    Some(target.to_string())
}

/// From a level drawn to it being on the screen: half a 60 fps UI tick,
/// and one refresh of the focused monitor (Hyprland's; 60 Hz otherwise).
fn display_s() -> f64 {
    static S: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *S.get_or_init(|| {
        let hz = std::process::Command::new("hyprctl")
            .args(["monitors", "-j"])
            .output()
            .ok()
            .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
            .and_then(|v| {
                v.as_array().and_then(|a| {
                    a.iter()
                        .find(|m| m["focused"] == true)
                        .or(a.first())
                        .and_then(|m| m["refreshRate"].as_f64())
                })
            })
            .filter(|hz| *hz >= 20.0)
            .unwrap_or(60.0);
        0.5 / 60.0 + 1.0 / hz
    })
}

/// The device a mic channel's source belongs to, and whether the source is
/// one port of it: a mic on `device:capture_FL` is that device's input, one
/// channel of it (the Router names it by the device).
fn mic_device(source: &str) -> (&str, bool) {
    source.split_once(':').map_or((source, false), |(d, _)| (d, true))
}

/// One undelayed reading of every tap.
struct Raw {
    at: Instant,
    peaks: HashMap<String, [f32; 2]>,
    vu: HashMap<String, [f32; 2]>,
    loudness: Option<LoudnessReading>,
    spectrum: Vec<f32>,
}

/// Human name for a stream: "PipeWire ALSA [cliamp]" → "cliamp".
/// A deck's mpv (a stream or radio on it) plays into the deck's own card, so
/// it is the deck, already on the desk as Deck A…D and the DJ Booth. As an
/// application it was a second strip and Router source for the same sound.
/// `keep_decks` still sees these: it reads the snapshot, before this.
fn is_deck_player(s: &StreamInfo) -> bool {
    matches!(s.node_name.as_str(), "Patch Deck A" | "Patch Deck B" | "Patch Deck C" | "Patch Deck D")
}

fn without_deck_players(mut streams: Vec<StreamInfo>) -> Vec<StreamInfo> {
    streams.retain(|s| !is_deck_player(s));
    streams
}

/// Which sources the Meter's input counts: one starts counting after it
/// has played for `START` and stops after `STOP` of silence. A pause
/// between songs, a quiet intro or a deck tail crossing the threshold no
/// longer changes the label (it changed it every few frames).
#[derive(Default)]
struct InputHold {
    seen: HashMap<String, Held>,
}

struct Held {
    /// Above the threshold since (None: below it now).
    since: Option<Instant>,
    /// Last above the threshold.
    last: Instant,
    on: bool,
}

impl InputHold {
    const ACTIVE_DB: f32 = -70.0;
    const START: Duration = Duration::from_millis(400);
    const STOP: Duration = Duration::from_secs(8);

    fn counts(&mut self, key: &str, db: f32, now: Instant) -> bool {
        let h = self.seen.entry(key.to_string()).or_insert(Held { since: None, last: now, on: false });
        if db > Self::ACTIVE_DB {
            let since = *h.since.get_or_insert(now);
            h.last = now;
            if now.duration_since(since) >= Self::START {
                h.on = true;
            }
        } else {
            h.since = None;
            if now.duration_since(h.last) >= Self::STOP {
                h.on = false;
            }
        }
        h.on
    }

    /// Sources not asked about for a minute (closed apps, a removed deck).
    fn forget_old(&mut self, now: Instant) {
        self.seen.retain(|_, h| h.on || h.since.is_some() || now.duration_since(h.last) < Duration::from_secs(60));
    }
}

/// Levels (dB per side) of uncorrelated sources summed by power: one source
/// reads as itself, two equal ones 3 dB louder.
fn power_sum(levels: impl Iterator<Item = [f32; 2]>) -> [f32; 2] {
    let mut e = [0.0f64; 2];
    for l in levels {
        for side in 0..2 {
            e[side] += 10f64.powf(f64::from(l[side].max(SILENCE_DB)) / 10.0);
        }
    }
    e.map(|x| if x > 0.0 { (10.0 * x.log10()).max(f64::from(SILENCE_DB)) as f32 } else { SILENCE_DB })
}

fn stream_name(s: &StreamInfo) -> String {
    let app = s.app_name.clone();
    if let (Some(a), Some(b)) = (app.find('['), app.rfind(']'))
        && b > a + 1
    {
        return app[a + 1..b].to_string();
    }
    if app.is_empty() {
        s.node_name.clone()
    } else {
        app
    }
}

fn node_id_for_stream(s: &StreamInfo) -> &'static str {
    intern(&format!("stream-{}", s.index))
}

impl PipeWireAudioProvider {
    /// `None` when PipeWire (with its Pulse server) isn't reachable.
    pub fn connect() -> Option<Self> { Self::connect_inner(true) }

    /// Attach to an existing engine without starting or upgrading it. Useful
    /// for diagnostics and isolated provider tests; it still permits controls.
    pub fn connect_existing() -> Option<Self> { Self::connect_inner(false) }

    fn connect_inner(manage_engine: bool) -> Option<Self> {
        let pw = Pactl::new();
        let info = pw.server_version()?;
        let (system_sounds_gain_db, system_sounds_mute) = system_sounds::load();
        let mut p = PipeWireAudioProvider {
            pw,
            pulse_writes: pulse_writes::PulseWrites::default(),
            sinks: vec![],
            streams: vec![],
            inputs: vec![],
            professional: false,
            app_channels: HashMap::new(),
            app_labels: HashMap::new(),
            hidden_streams: HashSet::new(),
            channel_sinks: HashMap::new(),
            playback_ports: vec![],
            rate: 48_000,
            quantum: 1024,
            version: info,
            bus_target: None,
            taps: HashMap::new(),
            signature: String::new(),
            latency: sync::LatencyWatch::start(),
            trims: sync::load_trims(),
            history: VecDeque::new(),
            shown: HashMap::new(),
            followers: routes::load(),
            last_default: None,
            notice: None,
            engine: engine_feed::EngineFeed::default(),
            watch: Arc::default(),
            engine_restart: None,
            manage_engine,
            reference: Default::default(),
            system_sounds_gain_db,
            system_sounds_mute,
            system_sounds_target: None,
            app_strip_preferences: app_strips::Store::load(),
            input_hold: InputHold::default(),
            channel_unity: HashSet::new(),
            node_seen: HashMap::new(),
            orphaned: HashMap::new(),
            adoption_primed: false,
            held: Default::default(),
        };
        if p.pw.sinks().iter().any(|s| s.name == SYSTEM_SOUNDS_SINK) {
            p.pw.set_sink_volume(SYSTEM_SOUNDS_SINK, system_sounds_gain_db);
            p.pw.set_sink_mute(SYSTEM_SOUNDS_SINK, system_sounds_mute);
            p.pw
                .set_source_mute("patch.system-sounds.monitor", system_sounds_mute);
        }
        // The engine is a detached process: start it if it isn't running,
        // and bring an older build up to this one without a gap.
        if manage_engine && let Ok(exe) = std::env::current_exe() {
            let _ = patch_engine::client::ensure_running(&exe);
            patch_engine::client::upgrade_if_outdated(&exe);
        }
        p.engine.poll();
        p.refresh_graph();
        p.watch = GraphSnap::watch();
        Some(p)
    }

    fn bus(&self) -> Option<&SinkInfo> {
        self.sinks.iter().find(|s| s.name == BUS)
    }

    fn hardware_sinks(&self) -> impl Iterator<Item = &SinkInfo> {
        self.sinks.iter().filter(|s| s.name != BUS)
    }

    fn port_targets(&self) -> Vec<(&'static str, &'static str, bool)> {
        let mut out = Vec::new();
        for sink in self.hardware_sinks() {
            let prefix = format!("{}:",sink.name);
            let mut ports: Vec<_> = self.playback_ports.iter().filter(|p| p.starts_with(&prefix)).cloned().collect();
            ports.sort_by(|a,b| contextual::port_order(a).cmp(&contextual::port_order(b)));
            let choices: Vec<Vec<String>> = ports.chunks(2).map(|p| p.to_vec())
                .chain(if ports.len() > 1 { ports.iter().map(|p| vec![p.clone()]).collect::<Vec<_>>() } else { vec![] }).collect();
            for (i,pair) in choices.iter().enumerate() {
                let id = contextual::port_node(&sink.name,pair);
                let channels = pair.iter().map(|p| {
                    match p.rsplit(':').next().unwrap_or(p).trim_start_matches("playback_") {
                        "FL" => "Left",
                        "FR" => "Right",
                        "FC" => "Center",
                        "LFE" => "Subwoofer",
                        "SL" => "Side left",
                        "SR" => "Side right",
                        "RL" => "Rear left",
                        "RR" => "Rear right",
                        channel => channel,
                    }
                }).collect::<Vec<_>>().join(" + ");
                let label = format!("{channels} · {}", self.pw.describe(&sink.name, &sink.description));
                let selected = self.engine.output.as_deref() == Some(sink.name.as_str()) &&
                    (self.engine.output_ports == *pair || (self.engine.output_ports.is_empty() && i == 0));
                out.push((id,intern(&label),selected));
            }
        }
        out
    }

    fn sink_by_index(&self, index: u32) -> Option<&SinkInfo> {
        self.sinks.iter().find(|s| s.index == index)
    }

    /// Re-read the graph. Returns true when its structure changed.
    /// Re-read the graph now (after an action of ours). Returns true when
    /// its structure changed.
    fn refresh_graph(&mut self) -> bool {
        let snap = GraphSnap::read(&self.pw);
        if self.engine.live() {
            self.app_channels = self.engine.app_channels();
        }
        self.apply_graph(snap)
    }

    /// The engine channel an application's audio actually runs through.
    /// Patch being its output, an app with a channel of its own plays into
    /// `patch.app-N`: that channel is its strip on the desk — its fader, its
    /// mute and its meters — while the engine ramps the gain. An app playing
    /// straight to a device has none (it keeps its stream volume and tap).
    ///
    /// The app sink is engine plumbing, so it is not in [`Self::sinks`]. The
    /// assignment (`Chromium` → `app-1`) is what makes the strip that channel.
    fn stream_channel(&self, s: &StreamInfo) -> Option<&'static str> {
        let app = self.engine_app(contextual::application_node(s))?;
        self.app_channels.get(&app).filter(|ch| self.channel_sinks.get(&s.sink).is_some_and(|sink| *sink == format!("patch.{ch}"))).and_then(|ch| {
            patch_core::stream::CHANNELS.iter().chain(patch_core::eq::APP_CHANNELS.iter()).find(|c| **c == ch).copied()
        })
    }

    /// The strips the graph has now (no held or kept idle ones), with the
    /// application strips named.
    fn shown_strips(&self) -> Vec<Strip> {
        let raw = self.raw_strips().into_iter().filter(|s|
            self.professional || s.node.strip_prefix("engine.").filter(|c| c.starts_with("mic-"))
                .is_none_or(|c| self.engine.active_inputs.contains(c))).collect();
        let mut shown = contextual::project_strips(raw, &self.streams, self.professional);
        shown.retain(|strip| {
            let explicitly_kept = self.app_strip_preferences.get(strip.node).is_some_and(|p| p.keep_when_silent || !p.custom_name.is_empty());
            explicitly_kept || self.engine_app(strip.node).is_none_or(|key| !self.hidden_streams.contains(&key))
        });
        for s in shown.iter_mut().filter(|s| contextual::is_application_node(s.node)) {
            if let Some(stream) = self.streams.iter().find(|stream| contextual::matches_node(stream, s.node)) {
                s.name = intern(&self.stream_label(stream));
            }
            if let Some(preference) = self.app_strip_preferences.get(s.node)
                && !preference.display_name().is_empty()
            {
                s.name = intern(preference.display_name());
            }
        }
        shown
    }

    /// A native lane belongs to this playback identity, never the app name.
    fn engine_app(&self, node: &str) -> Option<String> {
        let s = self.streams.iter().find(|s| contextual::matches_node(s, node))?;
        (!s.system_sound).then(|| if s.app_name == "Patch Player" { s.app_name.clone() }
            else { patch_core::stream::key(s.index, s.client_id, &s.process_id) })
    }

    fn stream_label(&self, stream: &StreamInfo) -> String {
        self.engine_app(contextual::application_node(stream)).and_then(|key| self.app_labels.get(&key).cloned())
            .unwrap_or_else(|| contextual::strip_name(stream, &self.streams))
    }

    /// A browser closes an idle output and opens a new stream on play. The
    /// new stream inherits the departed one's strip name and keep setting,
    /// when it appeared around the time the old one went (or the strip was
    /// kept), and only if it is a visible strip of the same application.
    fn adopt_reopened_streams(&mut self) {
        let now = Instant::now();
        let live: Vec<(&'static str, &'static str)> = self.streams.iter()
            .filter(|s| s.client_id.is_some() && !s.system_sound)
            .filter(|s| !self.hidden_streams.contains(&patch_core::stream::key(s.index, s.client_id, &s.process_id)))
            .map(|s| (contextual::application_base(s), contextual::application_node(s)))
            .collect();
        let all_live: HashSet<&str> = self.streams.iter().map(contextual::application_node).collect();
        self.node_seen.retain(|node, _| all_live.contains(node.as_str()));
        for node in &all_live {
            self.node_seen.entry(node.to_string()).or_insert(now);
        }
        let ids: Vec<String> = self.app_strip_preferences.ids()
            .filter(|id| id.contains("|stream:") && !all_live.contains(id)).map(String::from).collect();
        self.orphaned.retain(|id, _| ids.contains(id));
        if !std::mem::replace(&mut self.adoption_primed, true) {
            let long_ago = now.checked_sub(REOPEN_WINDOW * 2).unwrap_or(now);
            for id in &ids {
                self.orphaned.insert(id.clone(), long_ago);
            }
        }
        let mut orphans: Vec<(Instant, String)> = ids.into_iter()
            .map(|id| (*self.orphaned.entry(id.clone()).or_insert(now), id)).collect();
        orphans.sort_by_key(|a| std::cmp::Reverse(a.0));
        for (gone, id) in orphans {
            let kept = self.app_strip_preferences.get(&id).is_some_and(|p| p.keep_when_silent);
            if !kept && now.duration_since(gone) > REOPEN_WINDOW {
                continue;
            }
            let base = id.split('|').next().unwrap_or("");
            let reopened = live.iter().find(|(b, node)| *b == base
                && self.app_strip_preferences.get(node).is_none()
                && self.node_seen.get(*node).is_some_and(|seen| *seen + std::time::Duration::from_secs(5) >= gone));
            if let Some((_, node)) = reopened {
                match self.app_strip_preferences.reassign(&id, node) {
                    Ok(true) => { self.orphaned.remove(&id); }
                    Ok(false) => {}
                    Err(error) => self.notice = Some(error),
                }
            }
        }
    }

    /// An engine channel is the strip's level. The Pulse stream underneath
    /// it goes back to unity once, so the two gains do not stack.
    fn release_stream_gain(&mut self) {
        let streams = self.streams.clone();
        let plan: Vec<(u32, bool)> = streams
            .iter()
            .map(|s| {
                let arm = self.stream_channel(s).is_some()
                    && s.volume_db.is_finite()
                    && s.volume_db.abs() > 0.05;
                (s.index, arm)
            })
            .collect();
        let mut once = Vec::new();
        for (index, arm) in plan {
            if arm {
                if self.channel_unity.insert(index) {
                    once.push(index);
                }
            } else {
                self.channel_unity.remove(&index);
            }
        }
        for index in once {
            self.post_pulse(PulseWrite::StreamGain(index, 0.0));
        }
    }

    fn post_pulse(&mut self, write: PulseWrite) {
        match &write {
            PulseWrite::StreamGain(id, db) => {
                if let Some(s) = self.streams.iter_mut().find(|s| s.index == *id) { s.volume_db = *db; }
            }
            PulseWrite::StreamMute(id, mute) => {
                if let Some(s) = self.streams.iter_mut().find(|s| s.index == *id) { s.mute = *mute; }
            }
            PulseWrite::SinkGain(id, db) => {
                if let Some(s) = self.sinks.iter_mut().find(|s| s.name == *id) { s.volume_db = *db; }
            }
            PulseWrite::SinkMute(id, mute) => {
                if let Some(s) = self.sinks.iter_mut().find(|s| s.name == *id) { s.mute = *mute; }
            }
            _ => {}
        }
        self.pulse_writes.post(write);
    }

    /// The engine channel behind a strip with knobs: mics, Pattern, Tidal,
    /// Sequencer, an application with a channel of its own. Ok(None): an
    /// application that can get one; Err: no knobs (DJ Booth, Main).
    fn knob_channel(&self, node: &str) -> Result<Option<String>, ()> {
        if let Some(ch) = node
            .strip_prefix("engine.")
            .filter(|c| c.starts_with("mic-") || matches!(*c, "pattern" | "tidal" | "sequencer"))
        {
            return Ok(Some(ch.to_string()));
        }
        match self.engine_app(node) {
            Some(app) => Ok(self.app_channels.get(&app).cloned()),
            None => Err(()),
        }
    }

    fn channel_solo(&self, ch: &str) -> bool {
        self.engine.solos.get(ch).copied().unwrap_or(false)
    }

    /// Engine-owned strips can sit above unity. An app can too when it has
    /// its own engine channel; a direct PipeWire stream still tops out at 0.
    fn engine_headroom(&self, node: &str) -> bool {
        self.engine.live()
            && (node == "master"
                || node == DJ_MIXER
                || self.streams.iter().any(|s| {
                    contextual::matches_node(s, node) && self.stream_channel(s).is_some()
                })
                || node.strip_prefix("engine.").is_some_and(|c| {
                    c.starts_with("mic-") || matches!(c, "pattern" | "tidal" | "sequencer")
                }))
    }

    /// An application's solo is the engine channel it was given, if any.
    fn stream_solo(&self, s: &crate::pactl::StreamInfo) -> bool {
        self.stream_channel(s)
            .and_then(|ch| self.engine.solos.get(ch))
            .copied()
            .unwrap_or(false)
    }

    fn apply_graph(&mut self, mut snap: GraphSnap) -> bool {
        self.pulse_writes.reconcile(snap.read_at, &mut snap.sinks, &mut snap.streams);
        if self.app_channels != snap.app_channels {
            // A newly assigned/reused caller lane needs its current controls
            // now, rather than displaying the previous occupant for a second.
            self.engine.reconnect_now();
        }
        self.app_channels = snap.app_channels;
        self.engine.bind_stream_channels(&self.app_channels);
        self.app_labels = snap.app_labels;
        self.hidden_streams = snap.hidden_streams;
        self.channel_sinks = snap.channel_sinks;
        // A node caught before its volume is listed has no reading (NAN):
        // keep the last one for that node, or silence. Never 0 dB, which
        // slammed its strip's fader to the top for a frame.
        let streams = self.streams.clone();
        for s in snap.streams.iter_mut().filter(|s| s.volume_db.is_nan()) {
            s.volume_db = streams
                .iter()
                .find(|old| old.index == s.index)
                .map_or(-120.0, |old| old.volume_db);
        }
        let sinks = self.sinks.clone();
        for k in snap.sinks.iter_mut().filter(|k| k.volume_db.is_nan()) {
            k.volume_db = sinks
                .iter()
                .find(|old| old.index == k.index)
                .map_or(-120.0, |old| old.volume_db);
        }
        // Raw `patch.*` soundcards are engine plumbing; expose their logical
        // Patch equivalents in the Router instead of showing PipeWire internals.
        let own = |v: Vec<SinkInfo>| {
            v.into_iter()
                .filter(|s| !s.name.starts_with("patch."))
                .collect::<Vec<_>>()
        };
        let decks_moved = self.keep_decks(&snap.sinks, &snap.streams, snap.default.as_deref());
        let full_sinks = snap.sinks.clone();
        self.sinks = own(snap.sinks);
        self.streams = without_deck_players(snap.streams);
        let replacements: Vec<_> = self.streams.iter()
            .filter(|stream| stream.client_id.is_some())
            .flat_map(|stream| {
                let node = contextual::application_node(stream).to_string();
                let legacy = node.rsplit_once("|stream:").unwrap().0.to_string();
                [(contextual::application_base(stream).to_string(), node.clone()), (legacy, node)]
            }).collect();
        if let Err(error) = self.app_strip_preferences.migrate(&replacements) {
            self.notice = Some(error);
        }
        self.adopt_reopened_streams();
        self.inputs = snap.inputs;
        self.professional = snap.professional;
        self.playback_ports = snap.playback_ports;
        self.system_sounds_target = snap.system_sounds_target;
        // Patch owns the System sounds sink. WirePlumber's stream store keeps
        // a mute and a volume for `patch.system-sounds` and restores them
        // whenever the node (re)appears, which silently undoes the strip: the
        // mixer then shows "muted" while the sound plays on, and M only flips
        // its own memory - the key looks dead. Put the strip's state back on
        // the sink, and on the monitor source (what the loopback carries into
        // the mix) whenever the strip is muted, so a muted strip is silent.
        if let Some((db, mute)) = snap.system_sounds_live {
            if (db - self.system_sounds_gain_db).abs() > 0.05 {
                self.post_pulse(PulseWrite::SinkGain(SYSTEM_SOUNDS_SINK.into(), self.system_sounds_gain_db));
            }
            if mute != self.system_sounds_mute {
                self.post_pulse(PulseWrite::SinkMute(SYSTEM_SOUNDS_SINK.into(), self.system_sounds_mute));
            }
            if mute != self.system_sounds_mute || self.system_sounds_mute {
                self.post_pulse(PulseWrite::SourceMute("patch.system-sounds.monitor".into(), self.system_sounds_mute));
            }
        }
        // Changing the input source in the bar runs Omarchy's
        // `omarchy-audio-input-set-default`, which moves *every* source-output
        // to the new microphone so recording apps follow it. Patch's own
        // captures are in that list: afterwards the meters read the microphone
        // instead of the bus, and the system-sounds loopback carried the
        // microphone into the mix (so the output "stopped" being what it was).
        // Put Patch's captures back where they belong.
        let source_indexes = snap.source_indexes;
        if let (Some(system_sounds), Some(bus)) = (
            source_indexes.get("patch.system-sounds.monitor").copied(),
            source_indexes.get("patch.monitor").copied(),
        ) {
            let monitor_for_sink = |sink: &SinkInfo| source_indexes.get(&sink.monitor).copied();
            let mut want = vec![("input.loopback-".to_string(), system_sounds)];
            for key in self.taps.keys() {
                let source = match key.as_str() {
                    "system-sounds" => Some(system_sounds),
                    "bus" => Some(bus),
                    "output" => self.output_sink().and_then(monitor_for_sink),
                    key if key.starts_with("stream-") => key
                        .trim_start_matches("stream-")
                        .parse::<u32>()
                        .ok()
                        .and_then(|index| self.streams.iter().find(|s| s.index == index))
                        .and_then(|stream| full_sinks.iter().find(|s| s.index == stream.sink))
                        .and_then(monitor_for_sink),
                    _ => None,
                };
                if let Some(source) = source {
                    want.push((format!("Patch meter {key}"), source));
                }
            }
            let want: Vec<(&str, u32)> = want.iter().map(|(name, source)| (name.as_str(), *source)).collect();
            for (stream, source) in Pactl::capture_fixups(&snap.source_outputs, &want) {
                let _ = self.pw.move_source_output(stream, source);
            }
        }
        if self.follow_system(snap.default) || decks_moved {
            self.sinks = own(self.pw.sinks());
            self.streams = without_deck_players(self.pw.streams());
        }
        self.release_stream_gain();
        if let Some((rate, quantum)) = snap.clock {
            self.rate = rate;
            self.quantum = quantum;
        }
        self.engine.poll();
        self.bus_target = if self.engine.live() {
            self.engine.output.clone()
        } else {
            snap.loopback_target
        };
        // Supervisor: without the engine the bus must still reach the output
        // (a plain loopback, at once), and the engine is started again.
        if !self.engine.live() && self.manage_engine {
            // Engine first: when it comes back it links itself, and a
            // loopback beside it would double the program.
            if self
                .engine_restart
                .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(5))
            {
                self.engine_restart = Some(Instant::now());
                if let Ok(exe) = std::env::current_exe()
                    && patch_engine::client::ensure_running(&exe).is_ok()
                {
                    self.engine.reconnect_now();
                    for _ in 0..20 {
                        self.engine.poll();
                        if self.engine.live() {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                }
            }
            if self.engine.live() {
                self.bus_target = self.engine.output.clone();
            } else if self.bus_target.is_none()
                && self.bus().is_some()
                && let Some(target) = self.last_default.clone().filter(|d| d != BUS)
                && self.pw.create_loopback(BUS, &target).is_ok()
            {
                self.bus_target = Some(target);
            }
        }
        self.sync_taps();
        let sig = format!(
            "{:?}|{:?}|{:?}|{:?}|{:?}|{}|{:?}|{:?}|{}|{:?}|{:?}|{:?}|{:?}|{:?}",
            self.sinks
                .iter()
                .map(|s| (s.index, &s.state, (s.volume_db * 10.0) as i32, s.mute))
                .collect::<Vec<_>>(),
            self.streams
                .iter()
                .filter(|s| !s.system_sound)
                .map(|s| (s.index, s.sink, (s.volume_db * 10.0) as i32, s.mute))
                .collect::<Vec<_>>(),
            self.bus_target,
            self.engine.cue_output,
            self.engine.pfls,
            self.engine.live(),
            self.engine.visible_modules,
            self.engine.mics,
            self.professional,
            self.inputs.iter().map(|s| (&s.name, &s.channel_map)).collect::<Vec<_>>(),
            self.playback_ports, self.engine.output_ports, self.engine.cue_ports, self.engine.active_inputs,
        );
        let changed = sig != self.signature;
        self.signature = sig;
        changed
    }

    /// Keep one quiet, persistent PipeWire channel for short system events.
    /// New paplay/event streams are moved into it, and one loopback carries
    /// the channel to the current system output.
    /// Cooperate with the system output switch (Omarchy's bar sets the
    /// default sink and moves every app stream to it):
    ///
    /// * the Patch bus follows the new default output, and
    /// * streams routed through Patch go back onto the bus.
    ///
    /// Returns true when it moved anything.
    fn follow_system(&mut self, default: Option<String>) -> bool {
        let switched = self.last_default.is_some() && default != self.last_default;
        self.last_default = default.clone();
        // The persistent engine owns routing, including app EQ channels and
        // Patch Player. A window must not compete with it over stream moves:
        // only once the engine is known to be gone (not in the second after
        // a window opens, before its first answer, when it moved Chromium
        // off its app channel and said "Chromium kept on the Patch bus").
        if !self.engine.absent() {
            return false;
        }
        let Some(bus) = self.sinks.iter().find(|s| s.name == BUS).map(|s| s.index) else {
            return false;
        };
        let mut moved = false;
        let mut notes = vec![];
        if switched
            && let Some(d) = default.as_deref().filter(|d| *d != BUS)
            && self.pw.bus_loopback_target().as_deref() != Some(d)
            && let Some(lb) = self.pw.bus_loopback_index()
            && self.pw.move_stream(lb, d).is_ok()
        {
            moved = true;
            let name = self
                .sinks
                .iter()
                .find(|s| s.name == d)
                .map(|s| self.pw.describe(&s.name, &s.description))
                .unwrap_or_else(|| d.to_string());
            notes.push(format!(
                "System output is now {name}: the Patch bus plays there"
            ));
        }
        for s in self.streams.clone() {
            if !s.system_sound
                && (self.followers.contains(contextual::application_base(&s))
                    || self.followers.contains(contextual::application_node(&s))
                    || self.followers.contains(&s.node_name))
                && s.sink != bus
                && self.pw.move_stream(s.index, BUS).is_ok()
            {
                moved = true;
                notes.push(format!("{} kept on the Patch bus", stream_name(&s)));
            }
        }
        if !notes.is_empty() {
            self.notice = Some(notes.join(" · "));
        }
        moved
    }

    /// The DJ decks play into their own soundcards (`patch.deck-a/-b`), and
    /// those soundcards are never the system output:
    ///
    /// * the system bar's output switch moves every app stream, decks
    ///   included: put them back;
    /// * if the switch lands on a deck soundcard, move on to the next output.
    ///
    /// Returns true when it moved anything.
    fn keep_decks(
        &mut self,
        sinks: &[SinkInfo],
        streams: &[StreamInfo],
        default: Option<&str>,
    ) -> bool {
        let mut moved = false;
        for (node, sink) in [
            ("Patch Deck A", "patch.deck-a"),
            ("Patch Deck B", "patch.deck-b"),
        ] {
            let Some(want) = sinks.iter().find(|s| s.name == sink).map(|s| s.index) else {
                continue;
            };
            for st in streams
                .iter()
                .filter(|st| st.node_name == node && st.sink != want)
            {
                moved |= self.pw.move_stream(st.index, sink).is_ok();
            }
        }
        if default.is_some_and(|d| d.starts_with("patch.deck-")) {
            // Same step the bar's switch takes, so the cycle continues past it.
            let skipped = std::process::Command::new("omarchy-audio-output-switch")
                .status()
                .is_ok_and(|s| s.success());
            if !skipped && let Some(hw) = self.last_default.clone().filter(|d| !d.starts_with(BUS))
            {
                let _ = self.pw.set_default_sink(&hw);
            }
            moved = true;
        }
        moved
    }

    fn remember(&mut self, stream_index: u32, through: bool) {
        if let Some(s) = self.streams.iter().find(|s| s.index == stream_index) {
            // A route belongs to the application: a browser opens a new
            // stream (and client) every time a paused tab plays again, so a
            // per-stream route would last one stream. Older per-client and
            // per-stream entries for it go.
            let base = contextual::application_base(s);
            let instance = format!("{base}|");
            self.followers.retain(|f| f != &s.node_name && !f.starts_with(&instance));
            if through {
                self.followers.insert(base.to_string());
            } else {
                self.followers.remove(base);
            }
            routes::save(&self.followers);
        }
    }

    /// One meter tap per playing stream, plus the bus and the output it
    /// reaches. Taps are cheap `parec` processes at a low sample rate.
    fn sync_taps(&mut self) {
        // An app mixed on its own engine channel is metered there: a
        // per-stream monitor carries nothing on this stack (pipewire-pulse
        // has no per-stream monitors), so a tap for it would be a dead
        // `parec` process per stream.
        let mut wanted: Vec<(String, Vec<String>)> = self
            .streams
            .iter()
            .filter(|s| !s.system_sound)
            .filter(|s| self.stream_channel(s).is_none())
            .map(|s| {
                (
                    format!("stream-{}", s.index),
                    vec![format!("--monitor-stream={}", s.index)],
                )
            })
            .collect();
        if self.system_sounds_target.is_some() {
            wanted.push((
                SYSTEM_SOUNDS.into(),
                vec![format!("--device={SYSTEM_SOUNDS_SINK}.monitor")],
            ));
        }
        // With the engine, the bus and the program are metered in the engine.
        if !self.engine.live() {
            if self.bus().is_some() {
                wanted.push(("bus".into(), vec![format!("--device={BUS}.monitor")]));
            }
            if let Some(out) = self.output_sink() {
                wanted.push(("output".into(), vec![format!("--device={}", out.monitor)]));
            }
        }
        self.taps
            .retain(|k, t| wanted.iter().any(|(w, a)| w == k && *a == t.args));
        for (key, args) in wanted {
            if !self.taps.contains_key(&key)
                && let Some(t) = Tap::start(&key, args, self.rate, key == "output")
            {
                self.taps.insert(key, t);
            }
        }
    }

    /// The hardware sink the master feeds: the bus target, else the default.
    fn output_sink(&self) -> Option<&SinkInfo> {
        // The default the graph watcher keeps current (asking pactl here ran
        // a process per call).
        let name = self
            .bus_target
            .clone()
            .or_else(|| self.last_default.clone())?;
        self.sinks.iter().find(|s| s.name == name)
    }

    /// The system volume's sink: what the menu bar's slider and the volume
    /// keys set. The `patch` sink while Patch is the system's audio (the
    /// engine applies its volume to every channel), else the device the
    /// Patch Mix plays to.
    fn system_out(&self) -> Option<&SinkInfo> {
        if self.last_default.as_deref() == Some(BUS) {
            return self.sinks.iter().find(|s| s.name == BUS);
        }
        match self.engine.output.as_deref().filter(|_| self.engine.live()) {
            Some(o) => self.sinks.iter().find(|s| s.name == o),
            None => self.output_sink(),
        }
    }

    /// Stable hardware metadata for Tune to match against local output profiles.
    /// Empty `stable_id` means this backend lacks enough identity metadata for
    /// automatic profile activation.
    pub fn selected_output_identity(&self) -> Option<patch_tune::DeviceIdentity> {
        self.output_sink().map(SinkInfo::output_identity)
    }

    /// Seconds from a tap to your ears, minus the tap's own buffering.
    fn delay_s(&self, key: &str) -> f64 {
        let q = self.quantum as f64 / self.rate.max(1) as f64;
        let out = self.output_sink();
        let device = out
            .map(|s| self.latency.device(&s.name, self.quantum, self.rate))
            .unwrap_or(0.0);
        let trim = out
            .and_then(|s| self.trims.get(&s.name))
            .copied()
            .unwrap_or(0.0)
            / 1000.0;
        let to_ear_from_output = device + q + trim;
        let via_bus = sync::LOOPBACK_S + q + to_ear_from_output;
        let path = match key {
            "output" => to_ear_from_output,
            "bus" => via_bus,
            k => {
                let bus = self.bus().map(|b| b.index);
                let through = self
                    .streams
                    .iter()
                    .any(|s| format!("stream-{}", s.index) == k && Some(s.sink) == bus);
                if through { via_bus } else { to_ear_from_output }
            }
        };
        (path - sync::TAP_S).clamp(0.0, 2.5)
    }

    /// Engine block to the output device's DAC, and how that was found:
    /// a deliberate measurement (troubleshooting), else the delay the
    /// device's driver tracks every cycle plus what the device reports
    /// beyond its buffer (a Bluetooth codec), else what it reports, else
    /// one quantum. No microphone needed to be in sync out of the box.
    fn output_path_s(&self) -> (f64, patch_model::timing::LatencySource) {
        use patch_model::timing::LatencySource;
        let out = self.output_sink();
        if let Some(m) = out.and_then(|s| self.latency.measured(&s.name)) {
            return (m, LatencySource::Measured);
        }
        if let Some(d) = self.engine.output_delay.filter(|_| self.engine.live()) {
            let beyond = out
                .map(|s| self.latency.beyond_buffer(&s.name, self.engine.rate))
                .unwrap_or(0.0);
            return (d + beyond, LatencySource::Tracked);
        }
        // What the device reports counts the cycle waiting in its buffer
        // (adding a quantum on top counted that cycle twice: the meters
        // came 21 ms late).
        let q = self.engine.quantum.max(1) as f64 / self.engine.rate.max(1) as f64;
        match out
            .map(|s| {
                self.latency
                    .device(&s.name, self.engine.quantum, self.engine.rate)
            })
            .unwrap_or(0.0)
        {
            d if d > 0.0 => (d, LatencySource::Reported),
            _ => (q, LatencySource::Reported),
        }
    }

    /// Engine block to ear: the output path and the trim.
    fn engine_delay_ns(&self) -> u64 {
        if self.reference == patch_model::timing::MeterReference::Engine {
            return 0;
        }
        let out = self.output_sink();
        let trim = out
            .and_then(|s| self.trims.get(&s.name))
            .copied()
            .unwrap_or(0.0)
            / 1000.0;
        let path = self.output_path_s().0;
        // The screen shows a level later than it is drawn: half a UI frame
        // waiting for the tick, then one refresh to the panel. Showing it
        // that much earlier puts the needle on the beat you hear.
        ((path + trim - display_s()).clamp(0.0, 2.5) * 1e9) as u64
    }

    /// Meters with the engine: program and bus from its stamped blocks,
    /// streams from their taps, all shown when they are heard.
    fn engine_meters(&mut self, _t: f64) -> MeterFrame {
        self.engine.poll();
        let delay = self.engine_delay_ns();
        let (ch, out) = self.engine.take_due(delay);
        let snap = self.engine.snap_due(delay).cloned().unwrap_or_default();

        // Streams: taps, delayed like the program (they feed the bus).
        let now = Instant::now();
        let due = now
            .checked_sub(std::time::Duration::from_nanos(delay))
            .unwrap_or(now);
        self.history.push_back(Raw {
            at: now,
            peaks: self
                .taps
                .iter()
                .map(|(k, t)| (k.clone(), t.take_peak_db()))
                .collect(),
            vu: self.taps.iter().map(|(k, t)| (k.clone(), t.vu())).collect(),
            loudness: None,
            spectrum: vec![],
        });
        while self
            .history
            .front()
            .is_some_and(|r| now.duration_since(r.at).as_secs_f64() > 3.0)
        {
            self.history.pop_front();
        }
        let mut peaks: HashMap<String, [f32; 2]> = HashMap::new();
        let mut vus: HashMap<String, [f32; 2]> = HashMap::new();
        for key in self.taps.keys().cloned().collect::<Vec<_>>() {
            let since = self.shown.get(&key).copied();
            let mut p = [SILENCE_DB; 2];
            let mut newest = since;
            for r in self
                .history
                .iter()
                .filter(|r| r.at <= due && since.is_none_or(|s| r.at > s))
            {
                if let Some(v) = r.peaks.get(&key) {
                    p = [p[0].max(v[0]), p[1].max(v[1])];
                }
                newest = Some(r.at);
            }
            if let Some(n) = newest {
                self.shown.insert(key.clone(), n);
            }
            peaks.insert(key.clone(), p);
            if let Some(v) = self
                .history
                .iter()
                .rev()
                .find(|r| r.at <= due)
                .and_then(|r| r.vu.get(&key))
            {
                vus.insert(key, *v);
            }
        }
        let get = |k: &str| peaks.get(k).copied().unwrap_or([SILENCE_DB; 2]);
        let vu_of = |k: &str| vus.get(k).map(|v| v[0].max(v[1])).unwrap_or(-120.0);
        let visible_streams: Vec<&StreamInfo> =
            self.streams.iter().filter(|s| !s.system_sound).collect();
        // Each app's level and VU: its own engine channel where Patch mixes
        // it (the app's real signal, ramped and stamped with everything else,
        // and the only meter that works there: a per-stream monitor tap
        // carries nothing on this stack), else its tap.
        let app: Vec<([f32; 2], [f32; 2])> = visible_streams
            .iter()
            .map(|s| match self
                .stream_channel(s)
                .and_then(|c| self.engine.channels.iter().position(|x| x == c))
            {
                Some(i) => (
                    ch.get(i).copied().unwrap_or([SILENCE_DB; 2]),
                    self.engine.channel_vu(i).unwrap_or([-120.0; 2]),
                ),
                None => {
                    let k = format!("stream-{}", s.index);
                    (get(&k), vus.get(&k).copied().unwrap_or([-120.0; 2]))
                }
            })
            .collect();
        // Same order as `strips()`: app streams, System sounds, DJ Booth, mics, Patch mix,
        // then the decks (for the DJ Booth's own windows).
        // Deck channels: stereo peaks and their VU (from the engine's
        // per-channel levels, run through the same VU ballistics).
        let deck_idx: Vec<usize> = self
            .engine
            .channels
            .iter()
            .enumerate()
            .filter(|(_, c)| c.starts_with("deck-"))
            .map(|(i, _)| i)
            .collect();
        let deck_lr: Vec<[f32; 2]> = deck_idx
            .iter()
            .map(|i| ch.get(*i).copied().unwrap_or([SILENCE_DB; 2]))
            .collect();
        let deck_vu: Vec<Option<[f32; 2]>> = deck_idx
            .iter()
            .map(|i| self.engine.channel_vu(*i))
            .collect();
        let decks: Vec<f32> = deck_lr.iter().map(|p| p[0].max(p[1])).collect();
        // Microphones, in `strips()`'s order after the decks (they were
        // missing here: Mic 1 showed the master's level, Mic 2 none).
        let mic_idx: Vec<Option<usize>> = self
            .engine
            .mics
            .iter()
            .map(|(c, _)| self.engine.channels.iter().position(|x| x == c))
            .collect();
        let mics: Vec<f32> = mic_idx
            .iter()
            .map(|i| {
                i.and_then(|i| ch.get(i).copied())
                    .map_or(SILENCE_DB, |p| p[0].max(p[1]))
            })
            .collect();
        let mic_vu: Vec<f32> = mic_idx
            .iter()
            .map(|i| {
                i.and_then(|i| self.engine.channel_vu(i))
                    .map_or(-120.0, |v| v[0].max(v[1]))
            })
            .collect();
        // The engine's own voices, in `strips()`'s order after the mics.
        let synth_idx: Vec<Option<usize>> = ["pattern", "tidal", "sequencer"]
            .iter()
            .filter(|c| self.engine.visible_modules.contains(**c))
            .map(|c| self.engine.channels.iter().position(|x| x == c))
            .collect();
        let synth: Vec<[f32; 2]> = synth_idx
            .iter()
            .map(|i| {
                i.and_then(|i| ch.get(i).copied())
                    .unwrap_or([SILENCE_DB; 2])
            })
            .collect();
        let synth_vu: Vec<f32> = synth_idx
            .iter()
            .map(|i| {
                i.and_then(|i| self.engine.channel_vu(i))
                    .map_or(-120.0, |v| v[0].max(v[1]))
            })
            .collect();
        // The DJ Booth: the decks' sum after its own fader.
        let dj = self.engine.peaks.dj;
        let dj_vu = self.engine.dj_vu().map_or(-120.0, |v| v[0].max(v[1]));
        // The Patch mix strip is the program: its level and VU are the
        // output's (it showed the bus channel: apps through Patch only).
        // Output: the program after the system volume (the device's, or
        // the engine's output stage; only an older engine has it in the mix
        // already).
        let sys_db = match self.system_out() {
            Some(s) if s.mute => -200.0,
            Some(s) if s.name != BUS || self.engine.system_post => s.volume_db,
            _ => 0.0,
        };
        let heard = [
            (out[0] + sys_db).max(SILENCE_DB),
            (out[1] + sys_db).max(SILENCE_DB),
        ];
        let strips_db: Vec<f32> = app
            .iter()
            .map(|([l, r], _)| l.max(*r))
            .chain([get(SYSTEM_SOUNDS)[0].max(get(SYSTEM_SOUNDS)[1])])
            .chain(self.engine.visible_modules.contains("dj").then_some(dj[0].max(dj[1])))
            .chain(mics.iter().copied())
            .chain(synth.iter().map(|p| p[0].max(p[1])))
            .chain([out[0].max(out[1])])
            .chain([heard[0].max(heard[1])])
            .chain(decks.iter().copied())
            .collect();
        let strips_vu: Vec<f32> = app
            .iter()
            .map(|(_, [l, r])| l.max(*r))
            .chain([vu_of(SYSTEM_SOUNDS)])
            .chain(self.engine.visible_modules.contains("dj").then_some(dj_vu))
            .chain(mic_vu.iter().copied())
            .chain(synth_vu.iter().copied())
            .chain([snap.vu[0].max(snap.vu[1])])
            .chain([(snap.vu[0].max(snap.vu[1]) + sys_db).max(-120.0)])
            .chain(
                deck_vu
                    .iter()
                    .map(|v| v.map(|v| v[0].max(v[1])).unwrap_or(-120.0)),
            )
            .collect();
        // Input: everything feeding the program, summed, as a console's
        // input bus reads (it showed one source, the loudest, and swapped
        // label and level whenever another was louder: it flickered with
        // several playing). The DJ Booth is one source (its decks, a LINE
        // app on a deck included); each application another, its streams
        // together. A source counts once it plays for a moment and until
        // it has been quiet for a while (`InputHold`), so pauses between
        // songs and quiet intros keep the label still.
        let now = Instant::now();
        let mut sources: Vec<(String, String, [f32; 2], Option<[f32; 2]>)> = vec![];
        let held: Vec<(String, [f32; 2], Option<[f32; 2]>)> = deck_idx
            .iter()
            .zip(&deck_lr)
            .zip(&deck_vu)
            .map(|((i, p), v)| (self.engine.channels[*i].clone(), *p, *v))
            .filter(|(c, p, _)| self.input_hold.counts(c, p[0].max(p[1]), now))
            .collect();
        if !held.is_empty() {
            let names: Vec<String> = held
                .iter()
                .map(|(c, _, _)| c.trim_start_matches("deck-").to_uppercase())
                .collect();
            let peak = power_sum(held.iter().map(|(_, p, _)| *p));
            let vu = held.iter().map(|(_, _, v)| *v).collect::<Option<Vec<_>>>().map(|v| power_sum(v.into_iter()));
            sources.push(("dj".into(), format!("DJ Booth · Deck {}", names.join("+")), peak, vu));
        }
        // An app on a deck's LINE plays through the Booth: counted there.
        let deck_sinks: HashSet<u32> = self
            .sinks
            .iter()
            .filter(|k| k.name.starts_with("patch.deck-"))
            .map(|k| k.index)
            .collect();
        let mut apps: Vec<(String, [f32; 2], [f32; 2])> = vec![];
        for (st, (p, vu)) in visible_streams.iter().zip(&app) {
            if deck_sinks.contains(&st.sink) {
                continue;
            }
            let name = stream_name(st);
            match apps.iter_mut().find(|(n, _, _)| *n == name) {
                Some(a) => {
                    a.1 = power_sum([a.1, *p].into_iter());
                    a.2 = power_sum([a.2, *vu].into_iter());
                }
                None => apps.push((name, *p, *vu)),
            }
        }
        for (name, p, vu) in apps {
            if self.input_hold.counts(&format!("app:{name}"), p[0].max(p[1]), now) {
                sources.push((format!("app:{name}"), name, p, Some(vu)));
            }
        }
        self.input_hold.forget_old(now);
        let input_sources: Vec<String> = sources.iter().map(|s| s.1.clone()).collect();
        let (input, input_vu, input_label) = match sources.as_slice() {
            [] => match visible_streams
                .iter()
                .zip(&app)
                .find(|(st, _)| !deck_sinks.contains(&st.sink))
            {
                Some((st, (p, vu))) => (*p, Some(*vu), stream_name(st)),
                None => ([SILENCE_DB; 2], None, "no stream".into()),
            },
            [(_, name, p, vu)] => (*p, *vu, name.clone()),
            many => (
                power_sum(many.iter().map(|s| s.2)),
                many.iter().map(|s| s.3).collect::<Option<Vec<_>>>().map(|v| power_sum(v.into_iter())),
                format!("{} sources", many.len()),
            ),
        };
        let mic_lr: Vec<[f32; 2]> = mic_idx
            .iter()
            .map(|i| {
                i.and_then(|i| ch.get(i).copied())
                    .unwrap_or([SILENCE_DB; 2])
            })
            .collect();
        let strips_lr: Vec<[f32; 2]> = app
            .iter()
            .map(|(p, _)| *p)
            .chain([get(SYSTEM_SOUNDS)])
            .chain(self.engine.visible_modules.contains("dj").then_some(dj))
            .chain(mic_lr.iter().copied())
            .chain(synth.iter().copied())
            .chain([out])
            .chain([heard])
            .chain(deck_lr.iter().copied())
            .collect();
        MeterFrame {
            buses_db: if self.engine.buses.is_some() { self.engine.peaks.buses.to_vec() } else { vec![] },
            strips_lr: Some(strips_lr),
            strips_post_fader: self.engine.post_fader,
            deck_input_db: Some(["deck-a", "deck-b", "deck-c", "deck-d"].map(|name| {
                self.engine.channel(name).and_then(|i| self.engine.input_ch.get(i).copied())
                    .unwrap_or([SILENCE_DB; 2])
            })),
            input: Levels::stereo(input, input_vu),
            output: Levels::stereo(out, Some(snap.vu)),
            true_peak_db: out[0].max(out[1]),
            loudness: snap.loudness,
            strips_vu: Some(strips_vu),
            spectrum_db: snap.spectrum,
            strips_db,
            input_label,
            input_sources,
            output_label: self
                .output_sink()
                .map(|s| self.pw.describe(&s.name, &s.description))
                .unwrap_or_else(|| "output".into()),
            sync_ms: Some(delay as f32 / 1e6),
            timing: Some(Timing {
                reference: self.reference,
                source: "Patch engine".into(),
                latency_ms: delay as f32 / 1e6,
                level: self.output_path_s().1,
                trim_ms: self
                    .output_sink()
                    .and_then(|s| self.trims.get(&s.name))
                    .copied()
                    .unwrap_or(0.0) as f32,
            }),
        }
    }

    /// Adjust meter sync for the current output by `ms` (saved per device).
    pub fn trim_sync(&mut self, ms: f64) -> Result<f64, String> {
        let name = self
            .output_sink()
            .map(|s| s.name.clone())
            .ok_or("no output")?;
        let v = self.trims.entry(name).or_insert(0.0);
        *v = (*v + ms).clamp(-500.0, 500.0);
        let now = *v;
        sync::save_trims(&self.trims).map_err(|e| e.to_string())?;
        Ok(now)
    }

    /// Create the Patch bus (if needed) playing to the current default sink.
    fn ensure_bus(&mut self) -> Result<(), String> {
        if self.bus().is_none() {
            // The snapshot may come from a listing that failed: ask again.
            match self.pw.sink_exists(BUS) {
                Some(false) => {}
                Some(true) => {
                    self.refresh_graph();
                    return Ok(());
                }
                None => return Err("the audio server did not answer".into()),
            }
            let target = self.pw.default_sink().ok_or("no default output")?;
            let rate = self
                .sinks
                .iter()
                .find(|s| s.name == target)
                .map(|s| s.rate)
                .unwrap_or(self.rate);
            if self.engine.live() {
                self.pw.create_null_sink(BUS, rate)?;
            } else {
                self.pw.create_bus(BUS, &target, rate)?;
            }
            self.refresh_graph();
        }
        Ok(())
    }

    /// Stream `node` plays through Patch (true) or straight to the output.
    pub fn set_through_patch(&mut self, node: &str, through: bool) -> Result<(), String> {
        if !through && self.engine.live() && patch_core::setup::load().system_audio {
            return Err("system playback stays in the mixer; choose the Patch Mix destination".into());
        }
        let streams: Vec<_> = self.streams.iter().filter(|s| contextual::matches_node(s, node)).cloned().collect();
        if streams.is_empty() { return Err("not a playback stream".into()); }
        let target = if through {
            self.ensure_bus()?;
            BUS.to_string()
        } else {
            self.bus_target.clone().or_else(|| self.pw.default_sink()).ok_or("no output")?
        };
        for stream in streams {
            self.pw.move_stream(stream.index, &target)?;
            self.remember(stream.index, through);
        }
        self.refresh_graph();
        Ok(())
    }

    /// Put every stream back on the output and remove the Patch bus.
    pub fn restore(&mut self) -> Result<(), String> {
        if self.engine.live() && patch_core::setup::load().system_audio {
            return Err("disable Patch system audio before restoring direct playback".into());
        }
        if let Some(bus) = self.bus().map(|b| b.index) {
            let target = self
                .bus_target
                .clone()
                .or_else(|| self.pw.default_sink())
                .ok_or("no output")?;
            for s in self.streams.iter().filter(|s| s.sink == bus) {
                self.pw.move_stream(s.index, &target)?;
            }
            self.pw.remove_bus(BUS)?;
            self.followers.clear();
            routes::save(&self.followers);
        }
        self.refresh_graph();
        Ok(())
    }

    /// Node id for a CLI name: a stream's app name, "bus", "master", or an
    /// output device's name or description.
    pub fn resolve(&self, needle: &str) -> Option<&'static str> {
        let n = needle.to_lowercase();
        match n.as_str() {
            "bus" | "patch" => return self.bus().map(|_| BUS),
            "master" => return Some("master"),
            "system sounds" | "system-sounds" => return Some(SYSTEM_SOUNDS),
            _ => {}
        }
        if let Some(id) = self.find_stream(needle) {
            return Some(id);
        }
        self.hardware_sinks()
            .find(|s| {
                s.name.to_lowercase().contains(&n)
                    || self
                        .pw
                        .describe(&s.name, &s.description)
                        .to_lowercase()
                        .contains(&n)
            })
            .map(|s| intern(&s.name))
    }

    /// Node id of the first stream whose app name contains `needle`.
    pub fn find_stream(&self, needle: &str) -> Option<&'static str> {
        let n = needle.to_lowercase();
        self.streams
            .iter()
            .filter(|s| !s.system_sound)
            .find(|s| {
                stream_name(s).to_lowercase().contains(&n) || s.binary.to_lowercase().contains(&n)
            })
            .map(contextual::application_node)
    }

    pub fn is_through_patch(&self, node: &str) -> bool {
        let bus = self.bus().map(|b| b.index);
        self.streams
            .iter()
            .any(|s| contextual::matches_node(s, node) && Some(s.sink) == bus)
    }
}

impl PipeWireAudioProvider {
    fn raw_strips(&self) -> Vec<Strip> {
        let mut strips: Vec<Strip> = self
            .streams
            .iter()
            .filter(|s| !s.system_sound)
            .map(|s| {
                // An app mixed on its own channel shows that channel's fader
                // and mute (the engine ramps them). One at the stream's own
                // volume until the channel takes over.
                let ch = self.stream_channel(s);
                Strip {
                    node: node_id_for_stream(s),
                    name: intern(&stream_name(s)),
                    gain_db: ch
                        .and_then(|c| self.engine.gains.get(c).copied())
                        .unwrap_or(s.volume_db),
                    headroom: self.engine.live() && ch.is_some(),
                    mute: ch
                        .and_then(|c| self.engine.mutes.get(c).copied())
                        .unwrap_or(s.mute),
                    solo: self.stream_solo(s),
                    master: false,
                }
            })
            .collect();
        strips.push(Strip {
            node: SYSTEM_SOUNDS,
            name: "System sounds",
            gain_db: self.system_sounds_gain_db,
            headroom: false,
            mute: self.system_sounds_mute,
            solo: false,
            master: false,
        });
        // The DJ Booth: one channel on the Patch mix (decks, crossfader
        // and EQ live in the DJ Booth's own windows), as on a club's desk.
        if self.engine.live() && self.engine.visible_modules.contains("dj") {
            strips.push(Strip {
                node: DJ_MIXER,
                name: "DJ Booth",
                gain_db: self.engine.dj_group.0,
                headroom: true,
                mute: self.engine.dj_group.1,
                solo: self.engine.dj_solo,
                master: false,
            });
        }
        // Microphones on mic channels: their own strips (gain, mute; the
        // Inspector holds their voice processing).
        if self.engine.live() {
            for (ch, source) in &self.engine.mics {
                let n = ch.trim_start_matches("mic-");
                // An application's voice on a mic lane: a call's far end
                // (mix-minus) or a voice source (sendable, like a mic).
                let kind = match (patch_core::mic::remote_source(source), self.engine.mix_minus.contains(ch)) {
                    (true, true) => "Remote",
                    (true, false) => "Voice",
                    _ => "Mic",
                };
                strips.push(Strip {
                    node: intern(&format!("engine.{ch}")),
                    name: intern(&format!("{kind} {n}")),
                    gain_db: self.engine.gains.get(ch).copied().unwrap_or(0.0),
                    headroom: true,
                    mute: self.engine.mutes.get(ch).copied().unwrap_or(false),
                    solo: self.channel_solo(ch),
                    master: false,
                });
            }
        }
        // The engine's own voices on strips of their own: the Pattern Unit
        // (its drum voices and the material it plays), the Tidal Unit's
        // instrument, then the step Sequencer. Fader, mute and a meter each, like any other strip.
        if self.engine.live() {
            for (ch, name) in [("pattern", "Pattern"), ("tidal", "Tidal"), ("sequencer", "Sequencer")] {
                if !self.engine.visible_modules.contains(ch) {
                    continue;
                }
                strips.push(Strip {
                    node: intern(&format!("engine.{ch}")),
                    name: intern(name),
                    gain_db: self.engine.gains.get(ch).copied().unwrap_or(0.0),
                    headroom: true,
                    mute: self.engine.mutes.get(ch).copied().unwrap_or(false),
                    solo: self.channel_solo(ch),
                    master: false,
                });
            }
        }
        // The Patch mix: everything that plays through Patch (with the
        // engine, its master; without, the Patch bus or the output device).
        if self.engine.live() {
            strips.push(Strip {
                node: "master",
                name: "Program mix",
                gain_db: self.engine.master.0,
                headroom: true,
                mute: self.engine.master.1,
                solo: false,
                master: false,
            });
        } else if let Some(m) = self.bus() {
            strips.push(Strip {
                node: "master",
                name: "Program mix",
                gain_db: m.volume_db,
                headroom: false,
                mute: m.mute,
                solo: false,
                master: false,
            });
        }
        // Main output: the same listening-volume control as the menu bar.
        // Program mix is independent and never presented as this master.
        if let Some(o) = self.system_out() {
            strips.push(Strip {
                node: "output",
                name: "Main output",
                gain_db: o.volume_db,
                headroom: false,
                mute: o.mute,
                solo: false,
                master: true,
            });
        }
        // The decks last, after every strip the Mixer shows: the DJ Booth's
        // windows meter them (the Mixer shows the DJ Booth instead).
        if self.engine.live() {
            for ch in self
                .engine
                .channels
                .iter()
                .filter(|c| c.starts_with("deck-"))
            {
                let letter = ch.trim_start_matches("deck-").to_uppercase();
                strips.push(Strip {
                    node: intern(&format!("engine.{ch}")),
                    name: intern(&format!("Deck {letter}")),
                    gain_db: 0.0,
                    headroom: false,
                    mute: self.engine.mutes.get(ch).copied().unwrap_or(false),
                    solo: false,
                    master: false,
                });
            }
        }
        strips
    }

    fn raw_meters(&mut self, t: f64) -> MeterFrame {
        if self.engine.live() {
            return self.engine_meters(t);
        }
        // 1. Read every tap exactly once (peaks are read-and-reset).
        let now = Instant::now();
        let raw = Raw {
            at: now,
            peaks: self
                .taps
                .iter()
                .map(|(k, t)| (k.clone(), t.take_peak_db()))
                .collect(),
            vu: self.taps.iter().map(|(k, t)| (k.clone(), t.vu())).collect(),
            loudness: self.taps.get("output").and_then(|t| t.loudness()),
            spectrum: self
                .taps
                .get("output")
                .map(|t| t.spectrum_db(32))
                .unwrap_or_default(),
        };
        self.history.push_back(raw);
        while self
            .history
            .front()
            .is_some_and(|r| now.duration_since(r.at).as_secs_f64() > 3.0)
        {
            self.history.pop_front();
        }

        // 2. Show each point as it sounds *now*: delayed by its path latency.
        let keys: Vec<String> = self.taps.keys().cloned().collect();
        let mut peaks: HashMap<String, [f32; 2]> = HashMap::new();
        let mut vus: HashMap<String, [f32; 2]> = HashMap::new();
        for key in &keys {
            let delay = std::time::Duration::from_secs_f64(self.delay_s(key));
            let due = now.checked_sub(delay).unwrap_or(now);
            let since = self.shown.get(key).copied();
            // Peaks: the max over every reading that came due since last frame.
            let mut p = [SILENCE_DB; 2];
            let mut newest = since;
            for r in self
                .history
                .iter()
                .filter(|r| r.at <= due && since.is_none_or(|s| r.at > s))
            {
                if let Some(v) = r.peaks.get(key) {
                    p = [p[0].max(v[0]), p[1].max(v[1])];
                }
                newest = Some(r.at);
            }
            if let Some(n) = newest {
                self.shown.insert(key.clone(), n);
            }
            peaks.insert(key.clone(), p);
            if let Some(r) = self.history.iter().rev().find(|r| r.at <= due)
                && let Some(v) = r.vu.get(key)
            {
                vus.insert(key.clone(), *v);
            }
        }
        let output_due = now
            .checked_sub(std::time::Duration::from_secs_f64(self.delay_s("output")))
            .unwrap_or(now);
        let out_raw = self.history.iter().rev().find(|r| r.at <= output_due);
        let loudness = out_raw.and_then(|r| r.loudness);
        let spectrum = out_raw.map(|r| r.spectrum.clone()).unwrap_or_default();

        let get = |k: &str| peaks.get(k).copied().unwrap_or([SILENCE_DB; 2]);
        // Same order as `strips()`: app streams, System sounds, Patch Mix,
        // Output.
        let tail: Vec<&str> = self
            .bus()
            .map(|_| "bus")
            .into_iter()
            .chain(self.system_out().map(|_| "output"))
            .collect();
        let strips_db: Vec<f32> = self
            .streams
            .iter()
            .filter(|s| !s.system_sound)
            .map(|s| {
                let [l, r] = get(&format!("stream-{}", s.index));
                l.max(r)
            })
            .chain([get(SYSTEM_SOUNDS)[0].max(get(SYSTEM_SOUNDS)[1])])
            .chain(tail.iter().map(|k| {
                let [l, r] = get(k);
                l.max(r)
            }))
            .collect();
        let vu_of = |k: &str| vus.get(k).map(|v| v[0].max(v[1])).unwrap_or(-120.0);
        let strips_vu: Vec<f32> = self
            .streams
            .iter()
            .filter(|s| !s.system_sound)
            .map(|s| vu_of(&format!("stream-{}", s.index)))
            .chain([vu_of(SYSTEM_SOUNDS)])
            .chain(tail.iter().map(|k| vu_of(k)))
            .collect();
        let first = self
            .streams
            .iter()
            .find(|s| !s.system_sound)
            .map(|s| format!("stream-{}", s.index));
        let input = first.as_deref().map(get).unwrap_or([SILENCE_DB; 2]);
        let output = get("output");
        MeterFrame {
            buses_db: vec![],
            strips_lr: None,
            strips_post_fader: false,
            deck_input_db: None,
            // The taps capture two channels (parec --channels=2); the engine
            // will report each bus in its own layout.
            input: Levels::stereo(input, first.as_deref().and_then(|k| vus.get(k).copied())),
            output: Levels::stereo(output, vus.get("output").copied()),
            true_peak_db: output[0].max(output[1]),
            loudness,
            strips_vu: Some(strips_vu),
            spectrum_db: spectrum,
            strips_db,
            input_label: self
                .streams
                .iter()
                .find(|s| !s.system_sound)
                .map(stream_name)
                .unwrap_or_else(|| "no stream".into()),
            input_sources: vec![],
            output_label: self
                .output_sink()
                .map(|s| self.pw.describe(&s.name, &s.description))
                .unwrap_or_else(|| "output".into()),
            sync_ms: Some((self.delay_s("output") * 1000.0) as f32),
            timing: None,
        }
    }

}

impl AudioProvider for PipeWireAudioProvider {
    fn provenance(&self) -> Provenance {
        Provenance::Live
    }

    /// Periodic refresh: apply the watcher's latest snapshot, if any. The
    /// UI thread never waits for pactl here (that would hitch the meters).
    fn refresh(&mut self) -> bool {
        let snap = self.watch.lock().unwrap().take();
        let selected = contextual::professional_selected();
        let mode_changed = self.professional != selected;
        let applied = snap.is_some();
        let mut changed = match snap {
            Some(mut s) => { s.professional = selected; self.apply_graph(s) },
            None => false,
        };
        self.professional = selected;
        let now = Instant::now();
        if applied {
            let live = self.shown_strips().into_iter()
                .filter(|s| contextual::is_application_node(s.node))
                .map(|strip| {
                    let stream = self.streams.iter().find(|s| contextual::matches_node(s, strip.node));
                    held_strips::Live {
                        app: stream.map_or(strip.node, contextual::application_base),
                        process: stream.map(|s| s.process_id.clone()).unwrap_or_default(),
                        controls: self.strip_controls(strip.node),
                        strip,
                    }
                })
                .collect();
            changed |= self.held.update(live, now, |pid| std::path::Path::new("/proc").join(pid).exists());
        } else {
            changed |= self.held.expire(now);
        }
        changed || mode_changed
    }

    fn engine(&self) -> EngineStatus {
        // With the engine, report the cycle it actually runs at.
        let live = self.engine.live() && self.engine.rate > 0;
        EngineStatus {
            backend: if live {
                let (avg, max) = self.engine.load;
                let rt = if self.engine.realtime {
                    ""
                } else {
                    " · not real-time"
                };
                format!(
                    "Patch engine · DSP {:.2}% avg · {:.2}% peak{rt}",
                    avg * 100.0,
                    max * 100.0
                )
            } else {
                format!("PipeWire {}", self.version)
            },
            sample_rate: if live { self.engine.rate } else { self.rate },
            buffer: if live {
                self.engine.quantum
            } else {
                self.quantum
            },
            // The xruns PipeWire reported to Patch's engine node since it
            // started: the Meter said "PipeWire XRUN unavailable" all the
            // time (the user, 2026-11-16).
            xruns: if live { self.engine.xruns } else { None },
            dsp_load: if live { self.engine.load.0 } else { 0.0 },
        }
    }

    fn devices(&self) -> Vec<Device> {
        self.hardware_sinks().map(|s| Device {
            id: intern(&s.name), name: intern(&self.pw.describe(&s.name, &s.description)),
            bus: intern(&s.bus_label()), sample_rate: s.rate,
            clock: intern(if s.state == "RUNNING" { "graph driver · running" } else { "idle" }),
            groups: vec![PortGroup { name: intern(&s.port_label()), direction: Direction::Out,
                ports: s.channel_map.iter().map(|c| pactl::channel_short(c)).collect() }], mock: false,
        }).chain(self.inputs.iter().map(|s| Device {
            id: intern(&s.name), name: intern(&s.description), bus: "Capture",
            sample_rate: s.rate, clock: "input · available",
            groups: vec![PortGroup { name: "Inputs", direction: Direction::In,
                ports: s.channel_map.iter().map(|c| pactl::channel_short(c)).collect() }], mock: false,
        })).collect()
    }

    fn nodes(&self) -> Vec<Node> {
        let mut nodes: Vec<Node> = self
            .streams
            .iter()
            .filter(|s| !s.system_sound)
            .map(|s| {
                let id = contextual::application_node(s);
                let default = self.stream_label(s);
                let name = self.app_strip_preferences.get(id).map(|p| p.display_name())
                    .filter(|name| !name.is_empty()).unwrap_or(&default);
                Node {
                    id,
                    name: intern(name),
                    kind: NodeKind::Source,
                    role: NodeRole::AppStream {
                        process: intern(if s.binary.is_empty() {
                            &s.node_name
                        } else {
                            &s.binary
                        }),
                        rules: vec![],
                    },
                    channels: s.channels,
                }
            })
            .collect();
        let mut seen = std::collections::HashSet::new();
        nodes.retain(|node| seen.insert(node.id));
        nodes.push(Node {
            id: SYSTEM_SOUNDS,
            name: "System sounds",
            kind: NodeKind::Source,
            role: NodeRole::Virtual {
                purpose: "short audio events · shared persistent fader",
            },
            channels: 2,
        });
        if let Some(bus) = self.bus() {
            nodes.push(Node {
                id: BUS,
                name: "Patch bus",
                kind: NodeKind::Processor,
                role: NodeRole::Virtual {
                    purpose: "null sink + loopback · server-side",
                },
                channels: bus.channels,
            });
        }
        if self.engine.live() {
            let dj = dj_installed();
            nodes.extend([
                Node { id: DECK_A, name: "Deck A", kind: NodeKind::Source, role: NodeRole::Virtual { purpose: "Patch internal DJ deck · fader, EQ and PFL in DJ Booth" }, channels: 2 },
                Node { id: DECK_B, name: "Deck B", kind: NodeKind::Source, role: NodeRole::Virtual { purpose: "Patch internal DJ deck · fader, EQ and PFL in DJ Booth" }, channels: 2 },
                Node { id: DJ_MIXER, name: "DJ Booth", kind: NodeKind::Processor, role: NodeRole::Virtual { purpose: "Patch internal · the decks after crossfader and the DJ Booth's fader" }, channels: 2 },
                Node { id: PROGRAM, name: "Patch Program", kind: NodeKind::Processor, role: NodeRole::Virtual { purpose: "Patch internal program output · choose hardware target" }, channels: 2 },
                Node { id: CUE, name: "Headphone Cue", kind: NodeKind::Processor, role: NodeRole::Virtual { purpose: "Patch internal DJ cue · PFL from either deck" }, channels: 2 },
            ]
            .into_iter()
            .filter(|n| dj || !DJ_NODES.contains(&n.id)));
        }
        for s in self.hardware_sinks() {
            nodes.push(Node {
                id: intern(&s.name),
                name: intern(&if self.professional {
                    format!("Device · {}", self.pw.describe(&s.name, &s.description))
                } else {
                    self.pw.describe(&s.name, &s.description)
                }),
                kind: NodeKind::Destination,
                role: NodeRole::HardwareOut {
                    device: intern(&s.name),
                },
                channels: s.channels,
            });
        }
        for input in &self.inputs {
            nodes.push(Node {
                id: intern(&input.name), name: intern(&input.description),
                kind: NodeKind::Source,
                role: NodeRole::HardwareIn { device: intern(&input.name) },
                channels: input.channels,
            });
        }
        if self.professional {
            for (id,name,_) in self.port_targets() {
                let (_,ports) = contextual::parse_port_node(id).unwrap();
                nodes.push(Node { id,name,kind:NodeKind::Destination,
                    role:NodeRole::HardwareOut {device:id},channels:ports.len() as u16 });
            }
        }
        for (channel,source) in &self.engine.mics {
            let (device, port) = mic_device(source);
            nodes.push(Node { id:intern(&format!("engine.{channel}")),name:intern(&format!("Input {}",channel.trim_start_matches("mic-"))),kind:NodeKind::Processor,
                role:NodeRole::HardwareIn {device:intern(device)},
                channels:if port { 1 } else { self.inputs.iter().find(|s| s.name == device).map(|s| s.channels.min(2)).unwrap_or(1) } });
        }
        let mut seen = std::collections::HashSet::new();
        nodes.retain(|n| seen.insert(n.id));
        nodes
    }

    fn routes(&self) -> Vec<Route> {
        let mut routes: Vec<Route> = self
            .streams
            .iter()
            .filter(|s| !s.system_sound)
            .filter_map(|s| {
                let sink = self.sink_by_index(s.sink)?;
                Some(Route {
                    from: contextual::application_node(s),
                    to: intern(&sink.name),
                    channels: s.channels,
                })
            })
            .collect();
        if let Some(target) = &self.system_sounds_target {
            routes.push(Route {
                from: SYSTEM_SOUNDS,
                to: intern(target),
                channels: 2,
            });
        }
        if self.engine.live() {
            routes.push(Route {
                from: DECK_A,
                to: DJ_MIXER,
                channels: 2,
            });
            routes.push(Route {
                from: DECK_B,
                to: DJ_MIXER,
                channels: 2,
            });
            if self.engine.pfls.get("deck-a").copied().unwrap_or(false) {
                routes.push(Route {
                    from: DECK_A,
                    to: CUE,
                    channels: 2,
                });
            }
            if self.engine.pfls.get("deck-b").copied().unwrap_or(false) {
                routes.push(Route {
                    from: DECK_B,
                    to: CUE,
                    channels: 2,
                });
            }
            routes.push(Route {
                from: DJ_MIXER,
                to: PROGRAM,
                channels: 2,
            });
            // Microphones play in the Patch mix.
            for (ch, _) in &self.engine.mics {
                routes.push(Route {
                    from: intern(&format!("engine.{ch}")),
                    to: PROGRAM,
                    channels: 2,
                });
            }
            if let Some(bus) = self.bus() {
                routes.push(Route {
                    from: BUS,
                    to: PROGRAM,
                    channels: bus.channels,
                });
            }
            if let Some(target) = self.engine.output.as_deref() {
                routes.push(Route {
                    from: PROGRAM,
                    to: intern(target),
                    channels: 2,
                });
            }
            if let Some(target) = self.engine.cue_output.as_deref() {
                routes.push(Route {
                    from: CUE,
                    to: intern(target),
                    channels: 2,
                });
            }
        } else if let (Some(bus), Some(target)) = (self.bus(), self.bus_target.as_deref()) {
            routes.push(Route {
                from: BUS,
                to: intern(target),
                channels: bus.channels,
            });
        }
        if self.professional {
            for (bus,device,ports) in [(PROGRAM,self.engine.output.as_deref(),&self.engine.output_ports),(CUE,self.engine.cue_output.as_deref(),&self.engine.cue_ports)] {
                if let Some(device) = device.filter(|_| !ports.is_empty()) {
                    for route in &mut routes {
                        if route.from == bus && route.to == device {
                            route.to = contextual::port_node(device,ports);
                            route.channels = ports.len() as u16;
                        }
                    }
                }
            }
        }
        if !dj_installed() {
            routes.retain(|r| !DJ_NODES.contains(&r.from) && !DJ_NODES.contains(&r.to));
        }
        routes
    }

    fn strips(&self) -> Vec<Strip> {
        let mut shown = self.shown_strips();
        let at = shown.iter().position(|s| contextual::is_application_node(s.node))
            .or_else(|| shown.iter().position(|s| !contextual::is_application_node(s.node)))
            .unwrap_or(shown.len());
        let (apps, rest): (Vec<Strip>, Vec<Strip>) = shown.drain(..).partition(|s| contextual::is_application_node(s.node));
        let mut apps = self.held.order(apps);
        let mut idle = Vec::new();
        for (id, preference) in self.app_strip_preferences.kept() {
            if !apps.iter().any(|s| s.node == id) {
                idle.push(Strip {
                    node: intern(id),
                    name: intern(preference.display_name()),
                    gain_db: -60.0,
                    headroom: false,
                    mute: false,
                    solo: false,
                    master: false,
                });
            }
        }
        // Application rows precede the fixed system/master and DJ rows;
        // keeping the same ordering makes their projected meters line up.
        apps.extend(idle);
        shown = rest;
        let at = at.min(shown.len());
        shown.splice(at..at, apps);
        shown
    }


    fn is_application_strip(&self, node: &str) -> bool {
        contextual::is_application_node(node) && node.contains("|client:")
    }

    fn strip_available(&self, node: &str) -> bool {
        !contextual::is_application_node(node)
            || self.streams.iter().any(|s| contextual::matches_node(s, node))
    }

    fn application_strip_kept(&self, node: &str) -> bool {
        self.app_strip_preferences
            .get(node)
            .is_some_and(|p| p.keep_when_silent)
    }

    fn configure_application_strip(&mut self, node: &str, name: &str, keep: bool) -> Result<(), String> {
        if !self.is_application_strip(node) {
            return Err("this is not an application strip".into());
        }
        let default_name = self
            .streams
            .iter()
            .find(|s| contextual::matches_node(s, node))
            .map(|stream| contextual::strip_name(stream, &self.streams))
            .or_else(|| self.app_strip_preferences.get(node).map(|p| p.default_name.clone()))
            .unwrap_or_default();
        self.app_strip_preferences.set(node, &default_name, name, keep)
    }

    fn professional_routing(&self) -> bool {
        self.professional
    }

    fn presets(&self) -> Vec<Preset> {
        vec![]
    }

    fn active_preset(&self) -> Option<&'static str> {
        None
    }

    fn set_gain(&mut self, node: &str, db: f32) {
        let hi = if self.engine_headroom(node) { 6.0 } else { 0.0 };
        // Sink-volume strips use Pulse's 0…100 % scale, whose bottom is
        // true silence (−∞ dB / zero volume), not the ordinary −60 dB gain
        // floor used by channel faders.
        let db = if node == SYSTEM_SOUNDS || node == "output" {
            pactl::clamp_volume_db(db)
        } else {
            db.clamp(-60.0, hi)
        };
        if node == SYSTEM_SOUNDS {
            self.system_sounds_gain_db = db;
            self.post_pulse(PulseWrite::SinkGain(SYSTEM_SOUNDS_SINK.into(), db));
            self.post_pulse(PulseWrite::SystemSounds(self.system_sounds_gain_db, self.system_sounds_mute));
            return;
        }
        if let Some(ch) = node
            .strip_prefix("engine.")
            .filter(|c| c.starts_with("mic-") || matches!(*c, "pattern" | "tidal" | "sequencer"))
        {
            let _ = self.engine.set_gain(ch, db);
            return;
        }
        if node == DJ_MIXER {
            let _ = self.engine.set_dj_group(Some(db), None);
            return;
        }
        if node == "output" {
            if let Some(o) = self.system_out().map(|o| o.name.clone()) {
                self.post_pulse(PulseWrite::SinkGain(o, db));
            }
            return;
        }
        if node == "master" && self.engine.live() {
            let _ = self.engine.set_master(Some(db), None);
            return;
        }
        if node == "master" {
            if let Some(m) = self
                .bus()
                .or_else(|| self.output_sink())
                .map(|m| m.name.clone())
            {
                self.post_pulse(PulseWrite::SinkGain(m, db));
            }
        } else {
            // An app with an engine channel of its own is mixed there: its
            // fader is that channel's gain, which the engine ramps. Driving
            // the stream's own volume instead spawned a `pactl` per pointer
            // move and stepped the level (the crackle on the first move).
            let streams: Vec<StreamInfo> = self
                .streams
                .iter()
                .filter(|s| contextual::matches_node(s, node))
                .cloned()
                .collect();
            for s in streams {
                match self.stream_channel(&s) {
                    Some(ch) => {
                        let _ = self.engine.set_gain(ch, db);
                        // The channel is the app's level now: its stream goes
                        // back to unity once, so the two do not stack.
                        if s.volume_db.is_finite() && s.volume_db.abs() > 0.05 {
                            self.post_pulse(PulseWrite::StreamGain(s.index, 0.0));
                        }
                    }
                    None => self.post_pulse(PulseWrite::StreamGain(s.index, db)),
                }
            }
        }
    }

    fn set_mute(&mut self, node: &str, mute: bool) {
        if node == SYSTEM_SOUNDS {
            self.system_sounds_mute = mute;
            self.post_pulse(PulseWrite::SinkMute(SYSTEM_SOUNDS_SINK.into(), mute));
            self.post_pulse(PulseWrite::SourceMute("patch.system-sounds.monitor".into(), mute));
            self.post_pulse(PulseWrite::SystemSounds(self.system_sounds_gain_db, self.system_sounds_mute));
            return;
        }
        if node == DJ_MIXER {
            let _ = self.engine.set_dj_group(None, Some(mute));
            return;
        }
        if node == "output" {
            if let Some(o) = self.system_out().map(|o| o.name.clone()) {
                self.post_pulse(PulseWrite::SinkMute(o, mute));
            }
            return;
        }
        if node == "master" && self.engine.live() {
            let _ = self.engine.set_master(None, Some(mute));
            return;
        }
        if let Some(ch) = node.strip_prefix("engine.") {
            let _ = self.engine.set_mute(ch, mute);
            return;
        }
        if node == "master" {
            if let Some(m) = self
                .bus()
                .or_else(|| self.output_sink())
                .map(|m| m.name.clone())
            {
                self.post_pulse(PulseWrite::SinkMute(m, mute));
            }
        } else {
            let streams: Vec<StreamInfo> = self
                .streams
                .iter()
                .filter(|s| contextual::matches_node(s, node))
                .cloned()
                .collect();
            // An engine channel mutes in the mix. Re-reading PipeWire here
            // only delays the button; the spare Pulse stream is not the fader.
            for s in &streams {
                match self.stream_channel(s) {
                    Some(ch) => {
                        let _ = self.engine.set_mute(ch, mute);
                    }
                    None => {
                        self.post_pulse(PulseWrite::StreamMute(s.index, mute));
                    }
                }
            }
        }
    }

    fn set_solo(&mut self, node: &str, solo: bool) -> Result<(), String> {
        // Program mix and Main are not soloable. The key does nothing.
        if node == "master" || node == "output" {
            return Ok(());
        }
        if node == DJ_MIXER {
            return self.engine.set_dj_solo(solo);
        }
        match self.knob_channel(node) {
            Ok(Some(ch)) => self.engine.set_solo(&ch, solo),
            Ok(None) => Err(
                "solo needs this strip's own channel: turn a knob once to give it one".into(),
            ),
            // No engine channel to solo (system sounds, a hardware path).
            Err(()) => Ok(()),
        }
    }

    fn apply_preset(&mut self, _id: &str) {}

    fn meters(&mut self, t: f64) -> MeterFrame {
        let frame = self.raw_meters(t);
        contextual::project_meters(frame, &self.raw_strips(), &self.strips(), &self.streams)
    }

    fn through_patch(&self, node: &str) -> Option<bool> {
        self.streams
            .iter()
            .any(|s| contextual::matches_node(s, node))
            .then(|| self.is_through_patch(node))
    }

    fn route_through_patch(&mut self, node: &str, through: bool) -> Result<(), String> {
        self.set_through_patch(node, through)
    }

    fn restore_routing(&mut self) -> Result<(), String> {
        self.restore()
    }

    fn find_stream(&self, app: &str) -> Option<&'static str> {
        PipeWireAudioProvider::resolve(self, app)
    }

    fn targets(&self, node: &str) -> Vec<(&'static str, &'static str, bool)> {
        let node = if matches!(node, "master" | "output") {
            if self.engine.live() {
                PROGRAM
            } else if self.bus().is_some() {
                BUS
            } else {
                "master"
            }
        } else {
            node
        };
        let current = if node == PROGRAM {
            self.engine.output.as_deref().map(intern)
        } else if node == CUE {
            self.engine.cue_output.as_deref().map(intern)
        } else if node == "master" {
            self.output_sink().map(|s| intern(&s.name))
        } else {
            self.routes()
                .into_iter()
                .find(|r| r.from == node)
                .map(|r| r.to)
        };
        let is_stream = self.streams.iter().any(|s| contextual::matches_node(s, node));
        if node == BUS && self.engine.live() {
            return vec![]; // fixed internal feed into Patch Program
        }
        if self.professional && node == PROGRAM { return self.port_targets(); }
        // AUX 1, 2: their own physical pair (none of Main's or Cue's), or
        // the source only.
        if let Some(i) = ["aux1", "aux2"].iter().position(|a| *a == node) {
            let route = self.engine.buses.as_ref().and_then(|b| b.aux_routes[i].clone());
            let taken: Vec<String> = self.engine.output_ports.iter().chain(&self.engine.cue_ports)
                .chain(self.engine.buses.as_ref().and_then(|b| b.aux_routes[1 - i].as_ref()).map(|(_, p)| p).into_iter().flatten())
                .cloned().collect();
            let mut targets = vec![(AUX_OFF[i], "Source only (record, stream)", route.is_none())];
            if self.professional {
                targets.extend(self.port_targets().into_iter().filter_map(|(id, name, _)| {
                    let (device, ports) = contextual::parse_port_node(id)?;
                    if ports.iter().any(|p| taken.contains(p)) { return None; }
                    Some((id, name, route.as_ref().is_some_and(|(d, p)| *d == device && *p == ports)))
                }));
            }
            return targets;
        }
        if self.professional && node == CUE {
            let mut targets = vec![(CUE_OFF,"Off",self.engine.cue_output.is_none())];
            targets.extend(self.port_targets().into_iter().filter_map(|(id,name,_)| {
                let (device,ports)=contextual::parse_port_node(id)?;
                if ports.len() != 2 || ports.iter().any(|p| self.engine.output_ports.contains(p)) { return None; }
                Some((id,name,self.engine.cue_output.as_deref() == Some(device.as_str()) && self.engine.cue_ports == ports))
            }));
            return targets;
        }
        if node == CUE {
            let mut targets = vec![(CUE_OFF, "Off", self.engine.cue_output.is_none())];
            targets.extend(
                self.hardware_sinks()
                    .filter(|s| Some(s.name.as_str()) != self.engine.output.as_deref())
                    .map(|s| {
                        let id = intern(&s.name);
                        (
                            id,
                            intern(&self.pw.describe(&s.name, &s.description)),
                            current == Some(id),
                        )
                    }),
            );
            return targets;
        }
        if node == DECK_A || node == DECK_B {
            let channel = if node == DECK_A { "deck-a" } else { "deck-b" };
            let pfl = self.engine.pfls.get(channel).copied().unwrap_or(false);
            return vec![
                (CUE, "Headphone Cue · PFL on", pfl),
                (DJ_MIXER, "DJ Mixer · PFL off", !pfl),
            ];
        }
        if node == PROGRAM {
            return self
                .hardware_sinks()
                .map(|s| {
                    let id = intern(&s.name);
                    (
                        id,
                        intern(&self.pw.describe(&s.name, &s.description)),
                        current == Some(id),
                    )
                })
                .collect();
        }
        if !is_stream && node != BUS && node != "master" {
            return vec![];
        }
        let mut out: Vec<(&'static str, &'static str, bool)> = vec![];
        if is_stream {
            out.push((BUS, "Patch bus", current == Some(BUS)));
            if self.engine.live() && patch_core::setup::load().system_audio {
                return out;
            }
        }
        for s in self.hardware_sinks() {
            let id = intern(&s.name);
            out.push((
                id,
                intern(&self.pw.describe(&s.name, &s.description)),
                current == Some(id),
            ));
        }
        out
    }

    fn main_outputs(&self) -> Vec<OutputChoice> {
        let live = self.engine.live();
        let current = if live {
            self.engine.output.clone()
        } else {
            self.output_sink().map(|s| s.name.clone())
        };
        // Professional routing patches physical ports: a device keeps the
        // pair it plays on now, else takes its first (front left + right).
        let pairs = if live && self.professional { self.port_targets() } else { vec![] };
        self.hardware_sinks()
            .map(|s| {
                let on_device = |id: &&(&'static str, &'static str, bool)| {
                    contextual::parse_port_node(id.0).is_some_and(|(d, _)| d == s.name)
                };
                let id = pairs
                    .iter()
                    .filter(on_device)
                    .find(|p| p.2)
                    .or_else(|| pairs.iter().find(on_device))
                    .map(|p| p.0)
                    .unwrap_or_else(|| intern(&s.name));
                OutputChoice {
                    id,
                    name: self.pw.describe(&s.name, &s.description),
                    kind: s.kind(),
                    detail: s.format_detail(),
                    current: current.as_deref() == Some(s.name.as_str()),
                }
            })
            .collect()
    }

    fn strip_controls(&self, node: &str) -> Option<StripControls> {
        if !self.engine.live() {
            return None;
        }
        if let Some(held) = self.held.controls(node) {
            return Some(held);
        }
        let buses = self.engine.buses.as_ref();
        // The DJ Booth: sends only (its decks' EQ is the DJ mixer's).
        if node == DJ_MIXER {
            return buses.map(|b| StripControls { sends: Some(b.dj_sends), ..Default::default() });
        }
        match self.knob_channel(node).ok()? {
            Some(ch) => self.engine.controls.get(&ch).copied().map(|mut c| {
                // Sends that do nothing say so (2026-11-22: a Remote lane's
                // AUX knob moved and did nothing; the user couldn't tell).
                c.sends_off = if self.engine.mix_minus.contains(&ch) {
                    let remote = self.engine.mics.iter().any(|(lane, source)| {
                        *lane == ch && patch_core::mic::remote_source(source)
                    });
                    Some(if remote {
                        patch_model::audio::SendsOff::Remote
                    } else {
                        patch_model::audio::SendsOff::CaptureOff
                    })
                } else {
                    self.engine.remote_lanes.get(&ch).and_then(|mic| {
                        Some(patch_model::audio::SendsOff::OnLane {
                            lane: mic.trim_start_matches("mic-").parse().ok()?,
                            far_end: self.engine.mix_minus.contains(mic),
                        })
                    })
                };
                c
            }),
            // Flat until its first turn gives it a channel.
            None => {
                let trims = self.engine.controls.values().any(|c| c.gain_db.is_some());
                Some(StripControls {
                    gain_db: trims.then_some(0.0),
                    tone: true,
                    sends: buses.map(|_| std::array::from_fn(|i| patch_model::audio::Send { db: None, pre: i < 2 })),
                    ..Default::default()
                })
            }
        }
    }

    fn buses(&self) -> Vec<BusStrip> {
        let Some(b) = self.engine.buses.as_ref().filter(|_| self.professional && self.engine.live()) else {
            return vec![];
        };
        let routed = |i: usize| match &b.aux_routes[i] {
            // The engine skips an AUX link onto Main's or Cue's ports (no
            // doubled signal), so a route saved before Main moved there
            // plays nothing: say so instead of naming the device
            // (2026-11-12: AUX 2 showed the Blue USB, silent).
            Some((device, ports))
                if ports.iter().any(|p| self.engine.output_ports.contains(p) || self.engine.cue_ports.contains(p)) =>
            {
                format!("silent · {} is Main's output", self.pw.describe(device, device))
            }
            Some((device, ports)) => {
                let names: Vec<_> = ports.iter().map(|p| p.rsplit(':').next().unwrap_or(p).trim_start_matches("playback_")).collect();
                format!("{} · {}", self.pw.describe(device, device), names.join(" + "))
            }
            None => format!("Patch AUX {} source", i + 1),
        };
        let names = ["AUX 1", "AUX 2", "FX 1 · REVERB", "FX 2 · DELAY"];
        (0..4)
            .map(|i| BusStrip {
                id: BUS_IDS[i],
                name: names[i],
                gain_db: b.faders[i].0,
                mute: b.faders[i].1,
                output: if i < 2 { routed(i) } else { "into Main".into() },
                param: match i {
                    2 => Some(("SIZE", b.reverb_size)),
                    3 => Some(("TIME", ((b.delay_ms - 40.0) / 960.0).clamp(0.0, 1.0))),
                    _ => None,
                },
            })
            .collect()
    }

    fn set_bus(&mut self, id: &str, db: Option<f32>, mute: Option<bool>, param: Option<f32>) -> Result<(), String> {
        let bus = BUS_IDS.iter().position(|b| *b == id).ok_or("no such bus")?;
        self.engine.set_bus(bus, db, mute, param)
    }

    fn strip_channel(&self, node: &str) -> Option<String> {
        self.knob_channel(node).ok().flatten()
    }

    fn assign_channel(&mut self, node: &str) -> Result<String, String> {
        if let Some(ch) = self.strip_channel(node) {
            return Ok(ch);
        }
        if !self.engine.live() {
            return Err("the engine is not running".into());
        }
        let app = self.engine_app(node).ok_or("no application behind this strip")?;
        let ch = self.engine.assign_app(&app)?;
        self.app_channels.insert(app, ch.clone());
        self.engine.bind_stream_channels(&self.app_channels);
        Ok(ch)
    }

    fn set_strip_control(&mut self, node: &str, control: &str, value: f32) -> Result<(), String> {
        if !self.engine.live() {
            return Err("the engine is not running".into());
        }
        let send_bus = |prefix: &str| {
            control.strip_prefix(prefix).and_then(|n| n.parse::<usize>().ok()).filter(|n| (1..=4).contains(n)).map(|n| n - 1)
        };
        if node == DJ_MIXER {
            return match (send_bus("send"), send_bus("pre")) {
                (Some(b), _) => self.engine.set_send("dj", b, Some((value > -60.0).then_some(value)), None),
                (_, Some(b)) => self.engine.set_send("dj", b, None, Some(value > 0.5)),
                _ => Err("the DJ Booth's EQ is on its decks".into()),
            };
        }
        let ch = match self.knob_channel(node) {
            Ok(Some(ch)) => ch,
            Ok(None) => {
                let app = self.engine_app(node).ok_or("no application behind this strip")?;
                let ch = self.engine.assign_app(&app)?;
                self.app_channels.insert(app, ch.clone());
        self.engine.bind_stream_channels(&self.app_channels);
                ch
            }
            Err(()) => return Err("this strip has no knobs".into()),
        };
        if let Some(b) = send_bus("send") {
            return self.engine.set_send(&ch, b, Some((value > -60.0).then_some(value)), None);
        }
        if let Some(b) = send_bus("pre") {
            return self.engine.set_send(&ch, b, None, Some(value > 0.5));
        }
        self.engine.set_control(&ch, control, value)
    }

    fn connect(&mut self, from: &str, to: &str) -> Result<(), String> {
        if let Some(i) = AUX_OFF.iter().position(|a| *a == to) {
            return self.engine.clear_aux_route(["aux1", "aux2"][i]);
        }
        if let Some((device,ports)) = contextual::parse_port_node(to) {
            if !self.professional { return Err("select Patch in the bar to assign physical ports".into()); }
            let bus = if matches!(from,"master"|"output"|PROGRAM) { "main" }
                else if from == CUE { "cue" }
                else if from == "aux1" || from == "aux2" { from }
                else { return Err("route the Main or Cue bus to hardware; sources stay in the core mixer".into()); };
            self.engine.route_output_ports(bus,&device,&ports)?;
            self.refresh_graph(); return Ok(());
        }
        // Master = "where everything ends up": the bus when it exists,
        // otherwise every stream.
        if matches!(from, "master" | "output") {
            if self.engine.live() {
                return self.connect(PROGRAM, to);
            }
            if self.bus().is_some() {
                return self.connect(BUS, to);
            }
            let sink = self
                .sinks
                .iter()
                .find(|s| s.name == to)
                .map(|s| s.name.clone())
                .ok_or("not an output")?;
            // Streams feeding a unit's internal sink (DJ decks) stay put.
            for s in self
                .streams
                .clone()
                .into_iter()
                .filter(|s| self.sink_by_index(s.sink).is_some())
            {
                self.pw.move_stream(s.index, &sink)?;
            }
            self.refresh_graph();
            return Ok(());
        }
        if from == DJ_MIXER && self.engine.live() && to != CUE {
            return Err(
                "the DJ Booth plays in the Patch Mix: patch the Patch Mix row to move it".into(),
            );
        }
        if from.starts_with("engine.mic-") && self.engine.live() {
            return Err(
                "a microphone plays in the Patch Mix: patch the Patch Mix row to move it".into(),
            );
        }
        if (from == DECK_A || from == DECK_B) && self.engine.live() {
            let channel = if from == DECK_A { "deck-a" } else { "deck-b" };
            return match to {
                CUE => self.engine.set_pfl(channel, true),
                DJ_MIXER => self.engine.set_pfl(channel, false),
                _ => Err("DJ decks feed the DJ Mixer; choose Headphone Cue to toggle PFL".into()),
            };
        }
        if from == CUE && to == CUE_OFF && self.engine.live() {
            return self.engine.set_cue_output(None);
        }
        if from == BUS && to == PROGRAM && self.engine.live() {
            return Ok(());
        }
        if to == BUS {
            return self.set_through_patch(from, true);
        }
        let Some(sink) = self
            .sinks
            .iter()
            .find(|s| s.name == to)
            .map(|s| s.name.clone())
        else {
            return Err(format!("{to} is not an output"));
        };
        if from == CUE && self.engine.live() {
            if Some(sink.as_str()) == self.engine.output.as_deref() {
                return Err("Headphone Cue needs an output separate from Patch Program".into());
            }
            self.engine.set_cue_output(Some(&sink))?;
        } else if from == PROGRAM && self.engine.live() {
            if self.professional {
                self.engine.set_output(&sink)?;
            } else {
                self.engine.select_output(&sink)?;
            }
        } else if from == BUS && self.engine.live() {
            // The engine carries the program: point its output elsewhere.
            self.engine.set_output(&sink)?;
        } else if from == BUS {
            // Re-point the bus: move its loopback stream. Audio keeps flowing.
            let lb = self
                .pw
                .bus_loopback_index()
                .ok_or("the Patch bus has no loopback")?;
            self.pw.move_stream(lb, &sink)?;
            self.pw.set_bus_target(&sink)?;
        } else {
            let streams: Vec<_> = self.streams.iter().filter(|s| contextual::matches_node(s, from)).cloned().collect();
            if streams.is_empty() { return Err("only playback streams and the Patch bus can be patched".into()); }
            if self.engine.live() && patch_core::setup::load().system_audio {
                return Err("application audio stays in the core mixer; choose Main output's destination".into());
            }
            for stream in streams {
                self.pw.move_stream(stream.index, &sink)?;
                self.remember(stream.index, false);
            }
        }
        self.refresh_graph();
        Ok(())
    }

    fn trim_sync(&mut self, ms: f64) -> Result<f64, String> {
        PipeWireAudioProvider::trim_sync(self, ms)
    }

    fn recordable(&self) -> Vec<(String, String, String)> {
        let mut out = vec![];
        if self.engine.live() {
            out.push(("input".into(), "Processed microphones · Patch Input".into(), format!("device:{}", patch_engine::links::INPUT)));
        }
        // The console's AUX mixes (Patch selected): a stream or a
        // recording of its own.
        if self.engine.live() && self.professional && self.engine.buses.is_some() {
            for (i, a) in patch_engine::links::AUX.iter().enumerate() {
                out.push((format!("aux{}", i + 1), format!("AUX {} · Patch console", i + 1), format!("device:{a}")));
            }
        }
        if self.engine.live() {
            // Sample-accurate: the engine's output before the device.
            out.push((
                "program".into(),
                "Program · Patch engine".into(),
                format!("device:{}", patch_engine::links::PROGRAM),
            ));
        }
        if self.bus().is_some() {
            out.push((
                "bus".into(),
                "Patch bus".into(),
                format!("device:{BUS}.monitor"),
            ));
        }
        if let Some(o) = self.output_sink() {
            out.push((
                "output".into(),
                format!(
                    "What you hear · {}",
                    self.pw.describe(&o.name, &o.description)
                ),
                format!("device:{}", o.monitor),
            ));
        }
        for s in self.streams.iter().filter(|s| !s.system_sound) {
            out.push((
                node_id_for_stream(s).into(),
                stream_name(s),
                format!("stream:{}", s.index),
            ));
        }
        out
    }

    fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    fn dj_mixer(&self) -> Option<(f32, [f32; 4])> {
        if self.engine.live() {
            self.engine.dj_mixer
        } else {
            None
        }
    }

    fn dj_eq(&self) -> Option<[[f32; 4]; 4]> {
        if self.engine.live() {
            self.engine.dj_eq
        } else {
            None
        }
    }

    fn dj_stems(&self) -> Option<([[f32; 4]; 4], [[f32; 4]; 4])> {
        if self.engine.live() {
            self.engine.dj_stems
        } else {
            None
        }
    }

    fn deck_playheads(&self) -> Vec<Option<(f64, f64)>> {
        if self.engine.live() {
            self.engine.heads_now()
        } else {
            vec![]
        }
    }

    fn deck_loads(&self) -> Vec<u32> {
        if self.engine.live() {
            self.engine.deck_loads.clone()
        } else {
            vec![]
        }
    }

    fn set_meter_reference(&mut self, r: patch_model::timing::MeterReference) {
        self.reference = r;
    }

    fn reset_loudness(&mut self) {
        self.engine.reset_loudness();
        if let Some(t) = self.taps.get("output") {
            t.reset_loudness();
        }
    }
}

/// Set the default input, and move the capture streams that should follow a
/// microphone — but never Patch's own. Omarchy's `omarchy-audio-input-set-default`
/// moves *every* source-output, and moving Patch's two (the system-sounds
/// loopback's capture side, and the UI's meter which reads the bus) is what made
/// the output stutter: the loopback briefly carried a microphone into the mix,
/// and the graph churn cost the engine a relink. The bar's input rows run this
/// instead. The provider's re-assert stays as a safety net for the stock menu.
pub fn set_default_input(name: &str) -> Result<(), String> {
    let pw = pactl::Pactl::new();
    pw.set_default_source(name)?;
    for (index, node, _source) in pw.source_outputs() {
        if node.starts_with("patch") || node.starts_with("Patch meter") || node.contains("loopback-")
        {
            continue;
        }
        let _ = pw.move_source_output_to(index, name);
    }
    Ok(())
}

#[cfg(test)]
mod mic_device_tests {
    #[test]
    fn a_port_mic_is_named_by_its_device() {
        let blue = "alsa_input.usb-BLUE_BLUE_USB_Audio_2.0_0000-00.analog-stereo";
        assert_eq!(super::mic_device(&format!("{blue}:capture_FL")), (blue, true));
        assert_eq!(super::mic_device(blue), (blue, false));
    }
}

#[cfg(test)]
mod input_hold_tests {
    use super::{power_sum, InputHold};
    use std::time::{Duration, Instant};

    #[test]
    fn a_source_counts_after_a_moment_and_stays_through_pauses() {
        let mut h = InputHold::default();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        // A click is not a source.
        assert!(!h.counts("app:Chromium", -12.0, at(0)));
        assert!(!h.counts("app:Chromium", -12.0, at(200)));
        assert!(h.counts("app:Chromium", -12.0, at(500)));
        // A gap between songs (3 s of silence) keeps it.
        assert!(h.counts("app:Chromium", -120.0, at(1_000)));
        assert!(h.counts("app:Chromium", -120.0, at(3_500)));
        assert!(h.counts("app:Chromium", -12.0, at(3_600)));
        // Stopped for good: it leaves after the hold.
        assert!(h.counts("app:Chromium", -120.0, at(4_000)));
        assert!(!h.counts("app:Chromium", -120.0, at(12_000)));
    }

    #[test]
    fn sources_sum_by_power() {
        let one = power_sum([[-12.0, -6.0]].into_iter());
        assert!((one[0] + 12.0).abs() < 1e-3 && (one[1] + 6.0).abs() < 1e-3);
        let two = power_sum([[-12.0, -12.0], [-12.0, -12.0]].into_iter());
        assert!((two[0] + 8.99).abs() < 0.01);
        assert_eq!(power_sum(std::iter::empty()), [super::SILENCE_DB; 2]);
    }
}
