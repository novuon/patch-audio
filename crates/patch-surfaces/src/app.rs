use crate::dj::{self, Deck, LibraryItem};
use crate::inspect;
use crate::metering::{self, Metering};
use crate::spec;
use crate::ui::*;
use patch_core::command::{
    self, AiAction, Command, Context as CommandContext, Origin, RecordFormat, Transport,
};
use patch_core::{AppId, AppSpec, UnitKind};
use patch_mock::MockAudioProvider;
use patch_model::audio::*;
use patch_model::channels::Levels;
use patch_model::timing::{LatencySource, MeterReference};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

#[path = "ai_dj.rs"]
mod ai_dj;
#[path = "sequencer_view.rs"]
mod sequencer_view;
#[path = "pattern_view.rs"]
mod pattern_view;
#[path = "tidal_view.rs"]
mod tidal_view;
// The parts of AudioApp, by what they serve (app/).
mod routing;
mod player_bind;
mod recorder;
mod broadcast;
mod network;
mod eq;
mod dj_bind;
mod dj_actions;
mod dj_analysis;
mod library;
mod set_prep;
mod dj_mixer;
mod deck_view;
mod deck_sample;
mod dj_poll;
mod mixer_view;
mod frame;
mod diagnostics;
mod plugins;
mod hosted;
mod module_keys;
mod booth_skin;
mod booth_controls;

fn latency_level(l: LatencySource) -> &'static str {
    match l {
        LatencySource::Reported => "REPORTED",
        LatencySource::Tracked => "TRACKED",
        LatencySource::Measured => "MEASURED",
    }
}

fn timestamp_utc() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = seconds.div_euclid(86_400);
    let rem = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    y += i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}
use patch_host::HostedApp;

patch_host::surface_window!(PatchWindow);

/// Headless screenshots of the real graph (`PATCH_SHOTS_LIVE=1`).
fn live_shots() -> bool {
    std::env::var_os("PATCH_SHOTS_LIVE").is_some()
}

/// PipeWire when reachable; the mock for screenshots or `PATCH_PROVIDER=mock`.
/// The service's DJ session as Light DJ shows it.
#[derive(Default)]
struct LightView {
    set_on: bool,
    /// The set may play drum swaps (rehearsed, opt-in).
    drum_swaps: bool,
    set_text: String,
    /// (artist – title, style · BPM · key · time), newest first.
    set_log: Vec<(String, String)>,
    say_reply: String,
    queue: Vec<String>,
    automix: bool,
    direction: String,
    next: String,
    /// The next track's title and technique, for the mixer's NEXT MIX.
    next_title: String,
    next_style: String,
    note: String,
    plan: String,
    master_db: f32,
}

/// The Beat FX unit's state (what its panel shows and the engine runs).
#[derive(Clone, Copy, Debug)]
struct FxUnit {
    deck: usize,
    kind: patch_core::command::FxKind,
    /// Effects chained after `kind`, in order.
    chain: [Option<patch_core::command::FxKind>; 2],
    beats: f32,
    depth: f32,
    on: bool,
}

impl Default for FxUnit {
    fn default() -> Self {
        FxUnit {
            deck: 0,
            kind: patch_core::command::FxKind::Echo,
            chain: [None; 2],
            beats: 0.75,
            depth: 0.5,
            on: false,
        }
    }
}

impl FxUnit {
    fn kinds(&self) -> Vec<patch_core::command::FxKind> {
        std::iter::once(self.kind)
            .chain(self.chain.iter().flatten().copied())
            .collect()
    }

    /// "echo+sweep".
    fn label(&self) -> String {
        self.kinds()
            .iter()
            .map(|k| k.name())
            .collect::<Vec<_>>()
            .join("+")
    }
}

/// The Broadcast window's form and its poller. The form belongs to the
/// user (edits stay); the poll only reads the running stream back.
struct BcView {
    cursor: Cell<usize>,
    kind: RefCell<String>,
    codec: RefCell<String>,
    bitrate: Cell<u32>,
    rate: Cell<u32>,
    target: RefCell<String>,
    mount: RefCell<String>,
    user: RefCell<String>,
    password: RefCell<String>,
    /// The RTMP stream key (a secret, like the password).
    key: RefCell<String>,
    /// The picture an RTMP stream carries: `performer`, `visualiser`, `none`.
    picture: RefCell<String>,
    /// Where this process's window sends the running broadcast's picture.
    sink: RefCell<Option<Rc<patch_broadcast::video::FrameSink>>>,
    name: RefCell<String>,
    busy: Cell<bool>,
    message: RefCell<String>,
    snapshot: RefCell<serde_json::Value>,
    /// The broadcast this window follows (its start, Unix seconds): when it
    /// ends by itself, its outcome says whether it failed and why.
    followed: Cell<Option<f64>>,
    /// When the followed broadcast went away with no outcome written yet.
    gone_at: Cell<Option<Instant>>,
    /// Why the last broadcast failed (FAILED until the next start).
    failure: RefCell<Option<String>>,
    polls: crate::poll::Poller,
    actions: crate::poll::Poller,
    last_edited: Cell<Instant>,
}

impl Default for BcView {
    fn default() -> Self {
        BcView {
            cursor: Cell::new(0),
            kind: RefCell::new("file".into()),
            codec: RefCell::new("opus".into()),
            bitrate: Cell::new(128),
            rate: Cell::new(48_000),
            target: RefCell::new(String::new()),
            mount: RefCell::new("patch".into()),
            user: RefCell::new("source".into()),
            password: RefCell::new(String::new()),
            key: RefCell::new(String::new()),
            picture: RefCell::new("performer".into()),
            sink: RefCell::new(None),
            name: RefCell::new("Patch".into()),
            busy: Cell::new(false),
            message: RefCell::new(String::new()),
            snapshot: RefCell::new(serde_json::Value::Null),
            followed: Cell::new(None),
            gone_at: Cell::new(None),
            failure: RefCell::new(None),
            polls: crate::poll::Poller::default(),
            actions: crate::poll::Poller::default(),
            last_edited: Cell::new(Instant::now()),
        }
    }
}

impl BcView {
    /// A form field changed (the name the surface sends with its new text).
    fn set_field(&self, field: &str, value: &str) {
        let slot = match field {
            "kind" => &self.kind,
            "codec" => &self.codec,
            "target" => &self.target,
            "mount" => &self.mount,
            "user" => &self.user,
            "password" => &self.password,
            "key" => &self.key,
            "picture" => &self.picture,
            "name" => &self.name,
            _ => return,
        };
        *slot.borrow_mut() = value.to_string();
    }
}


/// A list a window shows already holds `rows`. A new model is a change to
/// Slint even with the same rows: every window showing it redraws, so the
/// 5 Hz poll sets one only when its rows changed.
fn same_rows<T: Clone + PartialEq + 'static>(model: &slint::ModelRc<T>, rows: &[T]) -> bool {
    use slint::Model as _;
    model.row_count() == rows.len()
        && rows
            .iter()
            .enumerate()
            .all(|(i, r)| model.row_data(i).as_ref() == Some(r))
}

/// `new` with the lists `old` already holds kept as they are (cues, their
/// names, phrases): a deck item with fresh but equal lists still reads as
/// changed, and every window showing a deck redrew five times a second.
fn same_lists(mut new: DeckItem, old: &DeckItem) -> DeckItem {
    use slint::Model as _;
    fn keep<T: Clone + PartialEq + 'static>(new: &mut slint::ModelRc<T>, old: &slint::ModelRc<T>) {
        let rows: Vec<T> = new.iter().collect();
        if same_rows(old, &rows) {
            *new = old.clone();
        }
    }
    keep(&mut new.cues, &old.cues);
    keep(&mut new.cue_texts, &old.cue_texts);
    keep(&mut new.phrases, &old.phrases);
    keep(&mut new.parts, &old.parts);
    new
}

/// "¾ beat", "16 beats".
fn fx_beats_text(b: f32) -> String {
    let frac = [(0.125, "⅛"), (0.25, "¼"), (0.5, "½"), (0.75, "¾")];
    match frac.iter().find(|(v, _)| (b - v).abs() < 1e-3) {
        Some((_, t)) => format!("{t} beat"),
        None if (b - 1.0).abs() < 1e-3 => "1 beat".into(),
        None => format!("{b:.0} beats"),
    }
}

fn choose_provider(frozen: bool) -> Box<dyn AudioProvider> {
    let want_mock =
        (frozen && !live_shots()) || std::env::var("PATCH_PROVIDER").is_ok_and(|v| v == "mock");
    if !want_mock {
        // Live screenshots inspect the installed engine; rendering a dev
        // binary must never replace that engine with the screenshot build.
        let provider = if frozen {
            patch_pipewire::PipeWireAudioProvider::connect_existing()
        } else {
            patch_pipewire::PipeWireAudioProvider::connect()
        };
        if let Some(p) = provider {
            return Box::new(p);
        }
    }
    Box::new(MockAudioProvider::default())
}

const HISTORY: usize = 48;
/// DJ channel meters: −24 … +6 dBFS, so mastered programme moves in the
/// meter instead of sitting at the top, and overs (EQ boost) stay visible.
fn meter_pos(kind: patch_ui::meter::MeterKind, db: f32) -> f32 {
    kind.position(db)
}

/// The Mixer's meter scale is absolute dBFS for every strip. Fader gain is
/// shown separately in the readout below the cap; it must not change the
/// meaning of the meter's scale labels.
fn strip_meter_kind(_id: &str, _master: bool) -> patch_ui::meter::MeterKind {
    patch_ui::meter::MeterKind::System
}

/// One window's own way through the library: its filter, selection and
/// rows. Every Library window has one, so several can browse at once.
struct LibView {
    filter: RefCell<String>,
    /// Analysis standing ("all", "ready", "soft", "check", "waiting",
    /// "failed").
    state: RefCell<String>,
    /// Played history, combinable with the rest ("" | session | hour | earlier | unplayed).
    played: RefCell<String>,
    /// Key and tempo fit the deck that is playing.
    fits_only: Cell<bool>,
    /// Only tracks marked with a heart.
    fav_only: Cell<bool>,
    /// Only grids measured straight.
    straight_only: Cell<bool>,
    /// Favourites on top, each part in the library's own order.
    favourites_first: Cell<bool>,
    cursor: Cell<usize>,
    collection: Cell<Option<i64>>,
    collection_view: Cell<Option<i64>>,
    /// Browse only the tracks under this folder (a source, or a folder in one).
    location: RefCell<String>,
    /// Every row; the window's model holds only those in view.
    all: RefCell<Vec<LibItem>>,
    /// Each row's track (its URL), in the same order: the highlighted track
    /// without filtering the whole library again (16,000 rows, several
    /// times a poll, made the pre-listen HUD lag).
    urls: RefCell<Vec<String>>,
    first: Cell<usize>,
    shown: Cell<usize>,
    model: Rc<VecModel<LibItem>>,
    window: slint::Weak<PatchWindow>,
}

/// Shared state of one Audio session. Models are shared by every window;
/// scalar properties are broadcast.
/// Seconds of track in a deck's scrolling waveform strip (8 s show).
const SCOPE_STRIP_S: f64 = 16.0;

/// A readiness as the Library window names it ("waiting" when unknown).
fn state_name(r: Option<patch_library::Readiness>) -> &'static str {
    match r {
        Some(patch_library::Readiness::Ready) => "ready",
        Some(patch_library::Readiness::Check) => "check",
        Some(patch_library::Readiness::Soft) => "soft",
        Some(patch_library::Readiness::Failed) => "failed",
        Some(patch_library::Readiness::Waiting) | None => "waiting",
    }
}

/// What a Library row shows of a track's analysis.
#[derive(Clone)]
pub(crate) struct Brief {
    downbeat: Option<u8>,
    bpm: Option<f64>,
    bpm_confidence: f32,
    key: Option<patch_analysis::key::Key>,
    lufs: f64,
    /// The measured beat grid: how straight it reads, and why.
    grid: Option<(String, String)>,
}

impl Brief {
    pub(crate) fn of(a: &patch_analysis::Analysis) -> Brief {
        // The measured grid's tempo and downbeat when there is one (as the
        // decks use), without copying the analysis.
        let g = a.grid.as_ref();
        let grid = g.map(|g| {
            // The chip is one word, because the column is shared with the
            // title; a beat that wanders a few ms is not the same as one that
            // cannot hold a layer, and the row's line spells out the numbers.
            let label = if g.straight() {
                "straight"
            } else if g.mixability >= 0.5 {
                "slightly"
            } else {
                "loose"
            };
            let why = if g.straight() {
                String::new()
            } else if g.mixability >= 0.5 {
                format!(
                    "a beat that wanders (drift {:.0} ms, ±{:.1} ms): layers still fit, just shorter",
                    g.drift_ms, g.jitter_ms
                )
            } else {
                format!(
                    "loose beat (drift {:.0} ms, ±{:.1} ms): the AI keeps layers with it short",
                    g.drift_ms, g.jitter_ms
                )
            };
            (label.to_string(), why)
        });
        let (downbeat, bpm, bpm_confidence) = match g {
            Some(g) => (
                g.downbeat.or(a.downbeat),
                Some(g.bpm),
                a.bpm_confidence.max(0.5 + 0.5 * g.mixability),
            ),
            None => (a.downbeat, a.bpm, a.bpm_confidence),
        };
        Brief {
            downbeat,
            bpm,
            bpm_confidence,
            key: a.key,
            lufs: a.lufs,
            grid,
        }
    }
}

pub struct AudioApp {
    /// The Plugins Unit's windows: the rack, faces, editors.
    plugins: plugins::PluginUi,
    provider: RefCell<Box<dyn AudioProvider>>,
    nodes: Rc<VecModel<GNode>>,
    edges: Rc<VecModel<GEdge>>,
    strips: Rc<VecModel<StripItem>>,
    in_channels: Rc<VecModel<ChannelLevel>>,
    out_channels: Rc<VecModel<ChannelLevel>>,
    /// The MAIN column's PLAYS TO rows, in `dests` order.
    main_dests: Rc<VecModel<bool>>,
    /// The console: the last size it fitted to, its last fit (so a rebuild
    /// that changes nothing broadcasts nothing), and the desk's placement.
    console_inner: Cell<f32>,
    /// The Mixer window draws the Patch-style bands.
    console_patch_style: Cell<bool>,
    console_w: Cell<f32>,
    fit_cache: Cell<Option<(crate::console_fit::ConsoleFit, f32)>>,
    devices: Rc<VecModel<DeviceItem>>,
    presets: Rc<VecModel<PresetItem>>,
    sections: Rc<VecModel<InspSection>>,
    /// Microphones and the inputs to choose from (the engine's "mics"),
    /// read in the background while a mic strip is selected; a mic just
    /// added, to select once its strip is there.
    mic_polls: crate::poll::Poller,
    mic_info: RefCell<serde_json::Value>,
    mic_calibration: RefCell<Option<serde_json::Value>>,
    mic_pending: RefCell<Option<String>>,
    /// Where deck A and B sit in the provider's strips (the meters' order):
    /// after the Mixer's strips, which show the DJ Booth instead.
    deck_strips: Cell<[Option<usize>; 4]>,
    targets: Rc<VecModel<TargetItem>>,
    /// The Main output strip's device picker.
    outputs: Rc<VecModel<OutputItem>>,
    /// The console's buses (Patch selected), an AUX's output choices, and
    /// the buses' meters (dB as shown, falling).
    buses: Rc<VecModel<BusItem>>,
    bus_targets: Rc<VecModel<TargetItem>>,
    bus_meter: RefCell<[[f32; 2]; 4]>,
    spectrum: Rc<VecModel<f32>>,
    history: Rc<VecModel<f32>>,
    selected: RefCell<String>,
    expanded: RefCell<HashSet<&'static str>>,
    device_cursor: Cell<usize>,
    preset_cursor: Cell<usize>,
    toast: RefCell<String>,
    metering: RefCell<Metering>,
    sequencer: sequencer_view::View,
    pattern: pattern_view::View,
    tidal: tidal_view::View,
    player: crate::player::Player,
    /// The Library's pre-listen: its own stream, beside the Player.
    previewer: crate::player::Player,
    /// The pre-listen moved to AUX 1 was told about in this run.
    aux_told: Cell<bool>,
    player_picker: crate::poll::Poller,
    player_picking: Cell<bool>,
    /// The Player's cart wall: its board, and what the windows show of it.
    carts: player_bind::CartsUi,
    /// The Player's queue, what played lately and the waveform.
    desk: player_bind::DeskUi,
    // Recording unit
    rec_cursor: Cell<usize>,
    rec_polls: crate::poll::Poller,
    rec_scans: crate::poll::Poller,
    rec_actions: crate::poll::Poller,
    rec_busy: Cell<bool>,
    rec_watch: Cell<bool>,
    rec_edited: Cell<Instant>,
    rec_snapshot: RefCell<serde_json::Value>,
    rec_files: RefCell<serde_json::Value>,
    rec_scan_at: Cell<Option<Instant>>,
    rec_float: Cell<bool>,
    rec_message: RefCell<String>,
    // Broadcast unit
    bc: BcView,
    // Network unit
    net: network::NetView,
    // DJ booth unit
    decks: [Deck; 4],
    library: RefCell<Vec<LibraryItem>>,
    library_keys: RefCell<Vec<(String, String, String)>>,
    library_loader: crate::library_worker::Loader,
    library_briefs: RefCell<HashMap<String, Brief>>,
    lib_collections: RefCell<Vec<(i64, String, usize)>>,
    /// Collections folded away under "Inactive" in the Library.
    lib_inactive: RefCell<HashSet<i64>>,
    lib_members: RefCell<HashMap<i64, HashSet<String>>>,
    /// The collections that are set preps, and each one's tracks in order.
    lib_sets: RefCell<HashSet<i64>>,
    /// The listed order of set preps and of collections played in order.
    lib_set_order: RefCell<HashMap<i64, Vec<String>>>,
    /// How each collection plays where chosen (true: in its listed order).
    lib_play_order: RefCell<HashMap<i64, bool>>,
    /// The set prep `s` adds to (the last one viewed).
    set_target: Cell<Option<i64>>,
    /// The Library's pre-listen.
    preview: RefCell<set_prep::Preview>,
    /// The HUD's highlighted track while nothing is pre-listened to.
    hud_idle: RefCell<set_prep::Preview>,
    /// Since when the pre-listen is paused: a minute paused ends it.
    preview_paused: Cell<Option<Instant>>,
    /// The paths of the favourite tracks (the decks ask every poll).
    lib_favourites: RefCell<HashSet<String>>,
    library_scan: RefCell<Option<std::sync::mpsc::Receiver<Result<(), String>>>>,
    session_id: String,
    /// Tempo, keylock and key of each deck.
    pitch: RefCell<[dj::Pitch; 4]>,
    /// Background track analysis (none in screenshots).
    analyst: Option<dj::Analyst>,
    analyses: RefCell<std::collections::HashMap<String, patch_analysis::Analysis>>,
    /// Library windows, to refresh when analyses arrive.
    lib_views: RefCell<Vec<Weak<LibView>>>,
    club_views: RefCell<Vec<Weak<booth_controls::View>>>,
    club_master: Cell<usize>,
    /// Analyses finished since the Library lists were last rebuilt: they
    /// are rebuilt at most every 2 s (a scan finishes many a second).
    lib_stale: Cell<Option<Instant>>,

    /// The librarian's last status, when it was read, and each track's
    /// readiness (path → state, why it failed).
    lib_status: RefCell<Option<patch_engine::librarian::Status>>,
    lib_status_at: Cell<Option<Instant>>,
    lib_ready: RefCell<HashMap<String, (patch_library::Readiness, Option<String>)>>,
    /// The loudness history is one flat line (silence): no scrolling.
    hist_flat: Cell<bool>,
    /// The crossfader view: when it was drawn, and the glow it showed.
    law_drawn: Cell<Option<Instant>>,
    /// Per deck, the track time the scrolling waveform strip is centred on.
    scope_anchor: RefCell<Vec<Option<f64>>>,
    /// Per deck: the overview images and what they were drawn for.
    overviews: RefCell<
        Vec<
            Option<(
                String,
                slint::Image,
                slint::Image,
                slint::Image,
                slint::Image,
                ModelRc<f32>,
            )>,
        >,
    >,
    /// Per deck: the track map (lane image, parts) and what it was made for.
    track_maps: RefCell<Vec<Option<(String, slint::Image, ModelRc<TrackPart>, f32)>>>,
    /// How the deck waveforms are drawn (the deck's switch; saved).
    scope_style: Cell<crate::scope::Style>,
    /// How meters are drawn (Classic, Banded, LED), for every window.
    meter_look: Cell<crate::meter_style::Look>,
    law_glow: Cell<u32>,
    /// The planned mix as last reported: beats in (negative before it
    /// starts), the deck it mixes out of, that deck's beat then, and its
    /// length in bars. Counted on per frame from the deck's own beat.
    mix_anchor: Cell<Option<(f64, usize, f32, f64)>>,
    prof_on: bool,
    /// PATCH_TRACE_HEAP=<file>: the heap's growth per update step (and
    /// between updates: drawing, input), written every 10 s.
    heap_trace: Option<std::path::PathBuf>,
    heap_at: Cell<f64>,
    heap_by: RefCell<Vec<(&'static str, f64)>>,
    heap_written: Cell<Option<Instant>>,
    prof_at: Cell<Option<Instant>>,
    prof: RefCell<Vec<(&'static str, Duration)>>,
    /// Auto-gain trims last sent to the engine.
    trims: Cell<[f64; 4]>,
    input_trims: Cell<[f32; 4]>,
    /// Match deck loudness from the analysis (EBU R128), never clipping.
    auto_gain: Cell<bool>,
    /// DJ EQ knob positions per deck: [high, mid, low, filter], −1 … 1.
    eq: Cell<[[f32; 4]; 4]>,
    /// Headphones: PFL per deck, cue ↔ master knob (−1 cue … 1 master),
    /// level knob (−1 … 1 → −30 … +6 dB).
    pfl: Cell<[bool; 4]>,
    /// Headphone device name, for the mixer.
    phones_name: RefCell<Option<String>>,
    /// Each deck channel's input selector: the application on LINE ("" =
    /// the deck), and the applications a LINE can take now.
    deck_lines: RefCell<[String; 4]>,
    line_sources: RefCell<Vec<String>>,
    /// Each channel's crossfader side (A B C D) and the curve, as the
    /// engine has them.
    xf_assign: Cell<[patch_engine::session::XfAssign; 4]>,
    xf_curve: Cell<patch_engine::session::XfCurve>,
    cue_knobs: Cell<(f32, f32)>,
    /// Hot cues per track.
    cues: RefCell<std::collections::HashMap<String, dj::Cues>>,
    /// The library's stamp when `cues` was last read from it. The booth and the
    /// engine both write cues, so the store's own modification time is what says
    /// this copy is stale — no message to miss, and no second source of truth.
    cues_stamp: Cell<Option<std::time::SystemTime>>,
    mission_cues: RefCell<HashMap<String, f64>>,
    /// Last known playhead per deck: (seconds, when, playing, speed), for
    /// 60 fps beat phase without asking the players every frame.
    heads: Cell<[(f64, Instant, bool, f64); 4]>,
    /// The transition plan the service is running (its progress, as the
    /// service reports it); None when none runs.
    plan: RefCell<Option<serde_json::Value>>,
    /// The AI DJ (suggest / prepare; a human runs the mix).
    /// Whether the AI DJ was on at the last poll, so a session's start is seen
    /// exactly once and the decks can be put on the phrase timeline then.
    ai_was_on: Cell<bool>,
    /// Whether the engine's AI DJ is acting (aivis Mode::Act). The app's own
    /// `ai.on` only covers actions the app ran, so this is the session signal
    /// the decks' AI-timeline default follows.
    ai_acting: Cell<bool>,
    ai: RefCell<ai_dj::AiDj>,
    /// Keyboard/MIDI bindings for the DJ surfaces (data, see patch-core::input).
    keymap: patch_core::input::Keymap,
    /// The Beat FX unit: one effect, aimed at one deck.
    fx: Cell<FxUnit>,
    /// When the DJ last set the Beat FX here (the engine's report then
    /// lags a poll: it doesn't overrule a fresh touch).
    fx_touched: Cell<Option<Instant>>,
    /// The FX unit before a held roll (restored on release).
    fx_before: Cell<Option<FxUnit>>,
    /// Scrolling waveforms: detailed waveform per deck (path, data, last try).
    /// Loop per deck from the last poll (seconds, engaged), for the scope.
    deck_loops: Cell<[(Option<(f64, f64)>, bool); 4]>,
    scope_detail:
        RefCell<[Option<(String, Option<Rc<patch_analysis::detail::Detail>>, Instant)>; 4]>,
    /// Diagnostics window: rows, CPU meters, worst UI frame, last timing.
    diag_rows: Rc<VecModel<DiagRow>>,
    /// Analyzer windows: engine polling, simulation, images.
    analyzer: Rc<crate::analyzer::Panel>,
    visual: Rc<crate::visual::Panel>,
    /// The Visualiser under the Tidal Unit's code (its own director, so a
    /// Visualiser window elsewhere keeps its show).
    tidal_visual: Rc<crate::visual::Panel>,
    broadcast_views: RefCell<Vec<crate::broadcast_performer::View>>,
    /// Each deck's level now (0…1), for the Visualiser.
    vis_levels: Cell<[f32; 2]>,
    /// AI DJ windows: the service's AI state, drawn.
    aidj: crate::aidj::Panel,
    dj_workspaces: RefCell<Vec<Rc<crate::dj_workspace::View>>>,
    stem_views: RefCell<Vec<Rc<crate::stems::View>>>,
    /// EQ windows: the master EQ, drawn and edited.
    master_eq: Rc<crate::eq_view::Panel>,
    /// Hardware input is engine-owned; this is asynchronous monitor telemetry.
    controller_polls: crate::poll::Poller,
    /// The deck sample maker's panel (x on a deck).
    sample: deck_sample::SampleMaker,
    controller_poll: Cell<Option<Instant>>,
    cpu_engine: crate::diag::CpuMeter,
    perf: crate::perf_view::Live,
    cpu_ui: crate::diag::CpuMeter,
    frame_worst_ms: Cell<f64>,
    /// One drawing budget for every window (budget.rs), and the frames the
    /// driver ran.
    budget: crate::budget::Budget,
    frames: Cell<u64>,
    /// When each window last presented a frame (keyed by its Window).
    /// Hyprland draws nothing on a hidden workspace, so a window that stops
    /// presenting while it animates is out of sight.
    presented: RefCell<HashMap<usize, Rc<Cell<Instant>>>>,
    frame_no: Cell<u64>,
    frame_loop: FrameLoop,
    keys: KeyGuard,
    last_timing: RefCell<Option<patch_model::audio::Timing>>,
    /// Grid last sent to each native deck (path|bpm).
    grid_sent: RefCell<[String; 4]>,
    /// What each deck has loaded (path), from the last poll.
    deck_sources: RefCell<[String; 4]>,
    xfader: Cell<f32>,
    mixer_decks: RefCell<dj_mixer::MixerDecks>,
    /// Last local crossfader/fader move: the engine's per-frame positions
    /// don't overwrite a drag in progress.
    mixer_touched: Cell<Option<Instant>>,
    /// When the DJ mixer was last read back from the service.
    dj_synced: Cell<Option<Instant>>,
    /// The `history_key` of the set log on screen (the engine leaves an
    /// unchanged log out of its replies).
    dj_history: RefCell<String>,
    dj_polls: crate::poll::Poller,
    /// The DJ mixer (crossfader, faders, auto-gain) on its way to the
    /// engine, newest first: a drag never waits on a reply.
    mixer_sends: crate::poll::Poller,
    plan_adjust_polls: crate::poll::Poller,
    plan_adjusting: Cell<bool>,
    dj_presence_at: Cell<Option<Instant>>,
    diag_polls: crate::poll::Poller,
    diag_snapshot: RefCell<serde_json::Value>,
    dj_changed: Cell<Option<Instant>>,
    engine_online: Cell<bool>,
    /// What Light DJ shows of the service's session.
    light: RefCell<LightView>,
    /// The track last selected in any Library window (Light DJ queues it).
    lib_selected: RefCell<Option<LibraryItem>>,
    reference: Cell<MeterReference>,
    /// DJ stop was asked while a deck played: a second ask within 5 s stops.
    stop_armed: Cell<Option<Instant>>,
    deck_gain: Cell<[f32; 4]>,
    dj_status: RefCell<String>,
    clip_until: RefCell<Vec<f64>>,
    hist_values: RefCell<std::collections::VecDeque<f32>>,
    last_t: Cell<f64>,
    last_scope: Cell<f64>,
    last_hist: Cell<f64>,
    last_text: Cell<f64>,
    windows: RefCell<Vec<slint::Weak<PatchWindow>>>,
    timer: slint::Timer,
    start: Instant,
    frozen: bool,
    me: Weak<Self>,
}

pub fn gain_to_pos(db: f32) -> f32 {
    ((db + 60.0) / 66.0).clamp(0.0, 1.0)
}

fn pos_to_gain(pos: f32) -> f32 {
    (pos * 66.0 - 60.0).clamp(-60.0, 6.0)
}

/// Engine-owned strips and app streams assigned their own engine channel
/// reach +6 dB while the engine is mixing. Direct PipeWire streams and
/// system sounds top out at 0 dB.
pub fn strip_has_headroom(id: &str, live: bool) -> bool {
    live && (id == "master"
        || id == "engine.dj-mixer"
        || id == "engine.pattern"
        || id == "engine.tidal"
        || id == "engine.sequencer"
        || id.starts_with("engine.mic-"))
}

/// Fader position to dB. The mark at the top is the top of what that strip
/// can do: +6 on an engine channel, 0 dB on a direct stream, and the bar's
/// amplitude scale on Main.
#[cfg(test)]
pub fn fader_db(id: &str, live: bool, pos: f32) -> f32 {
    fader_db_with_headroom(id, live, false, pos)
}

pub fn fader_db_with_headroom(id: &str, live: bool, strip_headroom: bool, pos: f32) -> f32 {
    let pos = if pos.is_finite() { pos.clamp(0.0, 1.0) } else { 0.0 };
    if id == "output" || id == "system-sounds" {
        output_db(pos)
    } else if (live && strip_headroom) || strip_has_headroom(id, live) {
        pos_to_gain(pos)
    } else if id.starts_with("application:") {
        output_db(pos)
    } else {
        (pos * 60.0 - 60.0).clamp(-60.0, 0.0)
    }
}

/// dB back to a fader position, on the same law as [`fader_db`].
#[cfg(test)]
pub fn fader_pos(id: &str, live: bool, db: f32) -> f32 {
    fader_pos_with_headroom(id, live, false, db)
}

pub fn fader_pos_with_headroom(id: &str, live: bool, strip_headroom: bool, db: f32) -> f32 {
    if id == "output" || id == "system-sounds" {
        output_pos(db)
    } else if (live && strip_headroom) || strip_has_headroom(id, live) {
        gain_to_pos(db)
    } else if id.starts_with("application:") {
        output_pos(db)
    } else {
        ((db.clamp(-60.0, 0.0) + 60.0) / 60.0).clamp(0.0, 1.0)
    }
}

/// Sink-volume strips and the bar's audio menu's OUTPUT slider share one
/// control scale. The slider is a 0…1 amplitude fraction (Pulse's own scale:
/// 1 = 100 % = 0 dB), so the strip's fader and readout use the same scale —
/// the same position, the same percentage the bar shows. The mixer's −60…+6
/// dB law cannot: its floor (−60 dB) is 10 % on the bar, so the bar never read
/// 0 and the two sliders never sat together.
pub fn output_pos(db: f32) -> f32 {
    if db <= -120.0 {
        0.0
    } else {
        10f32.powf(db / 60.0).clamp(0.0, 1.0)
    }
}

/// The bar's scale back to dB (0 is silence; the strip's floor).
fn output_db(pos: f32) -> f32 {
    if pos <= 0.0 {
        -120.0
    } else {
        60.0 * pos.log10()
    }
}

/// A sink-volume strip's readout: the percentage the bar's OUTPUT shows.
pub fn output_text(pos: f32) -> String {
    format!("{}%", (pos * 100.0).round() as i64)
}

/// A routed application's fader shows Pulse's cubic-volume percentage for
/// direct streams; engine-backed channels use amplitude percentage and can
/// reach +6 dB (about 200%).
pub fn app_volume_text(db: f32, headroom: bool) -> String {
    let max = if headroom { 200.0 } else { 100.0 };
    let percent = if !headroom {
        output_pos(db) * 100.0
    } else if db.is_finite() {
        (100.0 * 10f32.powf(db.clamp(-120.0, 6.0) / 20.0)).clamp(0.0, max)
    } else if db.is_sign_positive() {
        max
    } else {
        0.0
    };
    format!("{}%", percent.round() as i64)
}

/// The fader caption for every mixer strip. Keep rebuilds and incremental
/// refreshes on the same scale: application streams display their linear
/// volume, output strips display the bar's percentage, and channel strips
/// display dB.
pub fn strip_gain_text(id: &str, live: bool, headroom: bool, db: f32) -> String {
    if id.starts_with("application:") {
        app_volume_text(db, headroom || strip_has_headroom(id, live))
    } else if id == "output" || id == "system-sounds" {
        output_text(output_pos(db))
    } else {
        db_text(db)
    }
}

/// Keep the compact destination labels useful in MAIN's narrow strip.
/// Physical devices drop the generic "Device ·" prefix; channel targets use
/// their concise port label (such as "Left" or "Right").
pub fn main_destination_label(name: &str) -> String {
    let label = name
        .strip_prefix("Device · ")
        .unwrap_or(name)
        .split(" · ")
        .next()
        .unwrap_or(name)
        .trim();
    if name.starts_with("Device · ") {
        label.split_whitespace().take(2).collect::<Vec<_>>().join(" ")
    } else {
        label.to_string()
    }
}

pub fn db_text(db: f32) -> String {
    if db <= -59.9 {
        "−∞ dB".into()
    } else {
        format!("{} dB", crate::num::fixed(db, 1)).replace('-', "−")
    }
}

/// Linear amplitude → dBFS (−120 for silence).
fn amp_db(linear: f32) -> f32 {
    if linear > 0.0 {
        20.0 * linear.log10()
    } else {
        -120.0
    }
}

/// `~/x` → `$HOME/x` (the broadcast daemon writes straight to its path).
fn expand_home(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
            .join(rest)
            .to_string_lossy()
            .into_owned(),
        None => path.to_string(),
    }
}

fn short_port(name: &str) -> String {
    match name.rsplit(' ').next() {
        Some(last) if last.chars().all(|c| c.is_ascii_digit()) || last.len() == 1 => {
            last.to_string()
        }
        _ => name.chars().next().map(String::from).unwrap_or_default(),
    }
}

fn selection_exists<'a>(
    selected: &str,
    nodes: impl IntoIterator<Item = &'a str>,
    strips: impl IntoIterator<Item = &'a str>,
) -> bool {
    matches!(selected, "master" | "output")
        || nodes.into_iter().any(|id| id == selected)
        || strips.into_iter().any(|id| id == selected)
}

impl AudioApp {
    fn is_live(&self) -> bool {
        !self.provider.borrow().provenance().is_simulated()
    }

    fn log_track_event(&self, kind: &str, deck: &str, source: &str, data: serde_json::Value) {
        if source.is_empty() || source.contains("://") {
            return;
        }
        if let Ok(db) = patch_library::open_quick()
            && let Some(track) = db.track_by_path(source)
            && let Err(e) = db.log_session_event(
                &self.session_id,
                &timestamp_utc(),
                kind,
                Some(track),
                Some(deck),
                None,
                &data,
            )
        {
            eprintln!("Patch DJ session log: {e}");
        }
    }

    fn bind(&self, w: &PatchWindow) {
        match w.get_surface().as_str() {
            "plugins" => self.bind_plugins(w),
            "plugin" => self.bind_plugin_face(w),
            "dj-club" => {
                booth_skin::bind(w);
                self.bind_club_controls(w);
                if !patch_host::shot::active() {
                    w.window().set_maximized(true);
                }
            }
            _ => {}
        }
        w.global::<DiagState>()
            .set_rows(self.diag_rows.clone().into());
        self.analyzer.bind(w);
        self.visual.bind(w);
        if matches!(w.get_surface().as_str(), "dj" | "deck-a" | "deck-b" | "deck-c" | "deck-d") {
            self.stem_views
                .borrow_mut()
                .push(crate::stems::View::bind(w));
        }
        self.bind_aidj(w);
        if crate::dj_workspace::is_workspace(w.get_surface().as_str()) {
            let view = crate::dj_workspace::View::bind(w);
            // Recorder and replay actions use the existing shared transports.
            let me = self.me.clone();
            let weak = Rc::downgrade(&view);
            w.global::<crate::ui::DjWorkspaceState>()
                .on_action(move |kind, id| {
                    if let Some(app) = me.upgrade() {
                        if kind == "record" {
                            if let Err(e) = app.toggle_record_mode(Some(("program", None)))
                                && let Some(view) = weak.upgrade()
                            {
                                view.show_message(&e);
                            }
                            return;
                        }
                        if kind == "play_replay" {
                            if (0..2).any(|d| {
                                app.decks[d].status().loaded && !app.decks[d].status().paused
                            }) {
                                *app.toast.borrow_mut() =
                                    "Pause the live mix before playing a replay through Player"
                                        .into();
                                app.push_toast();
                                return;
                            }
                            app.player.send(crate::player::Action::Load(id.to_string()));
                            return;
                        }
                        if let Some(view) = weak.upgrade()
                            && let Err(e) = view.act(kind.as_str(), id.as_str())
                        {
                            view.show_message(&e);
                        }
                    }
                });
            self.dj_workspaces.borrow_mut().push(view);
        }
        self.bind_eq(w);
        self.bind_perf(w);
        self.bind_dj(w);
        self.bind_recorder(w);
        self.bind_broadcast(w);
        self.bind_network(w);
        self.bind_controllers(w);
        self.bind_player(w);
        self.bind_meter_style(w);
        self.bind_sequencer(w);
        self.bind_pattern(w);
        self.bind_tidal(w);
        let s = w.global::<AudioState>();
        s.set_nodes(self.nodes.clone().into());
        if let Some((on, name, _)) = self.plugin_node() {
            s.set_plugins_enabled(on);
            s.set_plugins_insert(if on { name.to_uppercase() } else { "OFF".into() }.into());
        }
        s.set_edges(self.edges.clone().into());
        s.set_strips(self.strips.clone().into());
        s.set_in_channels(self.in_channels.clone().into());
        s.set_out_channels(self.out_channels.clone().into());
        s.set_main_dests(self.main_dests.clone().into());
        let dests: Vec<SharedString> = self
            .provider_nodes()
            .iter()
            .filter(|n| n.kind == NodeKind::Destination)
            .map(|n| SharedString::from(main_destination_label(n.name)))
            .collect();
        s.set_dests(ModelRc::new(VecModel::from(dests)));
        let dest_ids: Vec<SharedString> = self
            .provider_nodes()
            .iter()
            .filter(|n| n.kind == NodeKind::Destination)
            .map(|n| SharedString::from(n.id))
            .collect();
        s.set_dest_ids(ModelRc::new(VecModel::from(dest_ids)));
        s.set_devices(self.devices.clone().into());
        s.set_presets(self.presets.clone().into());
        s.set_sections(self.sections.clone().into());
        s.set_targets(self.targets.clone().into());
        s.set_outputs(self.outputs.clone().into());
        s.set_buses(self.buses.clone().into());
        s.set_bus_targets(self.bus_targets.clone().into());
        s.set_main_output(self.outputs.iter().find(|o| o.current).map(|o| o.name).unwrap_or_default());
        s.set_spectrum(self.spectrum.clone().into());
        s.set_lufs_history(self.history.clone().into());
        let (area, line) = loudness_outline(self.history.iter());
        s.set_lufs_area(area.into());
        s.set_lufs_line(line.into());
        self.push_engine(&s);

        macro_rules! on {
            ($setter:ident, |$a:ident $(, $arg:ident)*| $body:expr) => {{
                let me = self.me.clone();
                s.$setter(move |$($arg),*| {
                    if let Some($a) = me.upgrade() {
                        $body;
                    }
                });
            }};
        }
        on!(on_select, |a, id| a.select(&id));
        on!(on_move_selection, |a, dx, dy| a.move_selection(dx, dy));
        on!(on_set_gain, |a, id, v| a.set_gain_pos(&id, v));
        on!(on_configure_application_strip, |a, id, name, keep| a.configure_application_strip(&id, &name, keep));
        on!(on_nudge_gain, |a, id, db| a.nudge_gain(&id, db));
        on!(on_toggle_mute, |a, id| a.toggle_mute(&id));
        on!(on_toggle_solo, |a, id| a.toggle_solo(&id));
        on!(on_toggle_device, |a, id| a.toggle_device(&id));
        on!(on_move_device, |a, d| a.move_device(d));
        on!(on_move_preset, |a, d| a.move_preset(d));
        on!(on_apply_preset, |a, id| a.apply_preset(&id));
        on!(on_toggle_through_patch, |a, id| a.toggle_through_patch(&id));
        on!(on_connect, |a, from, to| a.connect(&from, &to));
        on!(on_set_bus_gain, |a, id, pos| a.set_bus_gain(&id, pos));
        on!(on_toggle_bus_mute, |a, id| a.toggle_bus_mute(&id));
        on!(on_set_bus_param, |a, id, v| a.set_bus_param(&id, v));
        on!(on_open_bus_targets, |a, id| a.open_bus_targets(&id));
        on!(on_set_knob, |a, id, control, pos| a.set_knob(&id, &control, pos));
        on!(on_fit_console, |a, inner, width, patch_style| a.fit_console(inner, width, patch_style));
        on!(on_pick_output, |a, i| {
            let o = a.outputs.row_data(i.max(0) as usize);
            if let Some(o) = o.filter(|o| !o.current) {
                a.connect("output", &o.id);
            }
        });
        on!(on_add_mic, |a| a.add_mic());
        on!(on_add_remote, |a, role| a.add_remote(&role));
        on!(on_mic_source, |a, ch, src| a.mic_source(&ch, &src));
        on!(on_mic_remove, |a, ch| a.mic_remove(&ch));
        on!(on_mic_step, |a, direction| a.mic_step(direction));
        on!(on_mic_record, |a, processed| a.mic_record(processed));
        on!(on_mic_stop_record, |a| a.queue_record(serde_json::json!({"op":"stop"})));
        on!(on_mic_compare, |a, path| a.mic_compare(&path));
        on!(on_mic_listen, |a, path| {
            if a.rec_snapshot.borrow()["path"].as_str() == Some(path.as_str()) {
                *a.toast.borrow_mut() = "Stop and save the recording before listening".into(); a.push_toast();
            } else { a.player.send(crate::player::Action::Load(path.to_string())); }
        });
        on!(on_mic_analyze, |a, path| a.mic_analyze(&path));
        on!(on_mic_apply_calibration, |a| a.mic_apply_calibration());
        on!(on_mic_profile, |a, ch, action, name| a.mic_profile(&ch, &action, &name));
        on!(on_voice_set, |a, ch, key, on, amount| a
            .voice_set(&ch, &key, on, amount));
        on!(on_drop_at, |a, from, col, row| a.drop_at(&from, col, row));
        on!(on_restore_routing, |a| a.restore_routing());
        on!(on_reset_loudness, |a| a
            .provider
            .borrow_mut()
            .reset_loudness());
        on!(on_toggle_reference, |a| a.toggle_reference());
        on!(on_trim_sync, |a, ms| {
            let _ = a.provider.borrow_mut().trim_sync(ms as f64);
        });
    }

    /// Push every scalar into a freshly created window.
    fn sync_window(&self, w: &PatchWindow) {
        let t0 = Instant::now();
        let log = std::env::var_os("PATCH_STARTUP_LOG").is_some();
        let mut last = t0;
        let mut mark = |what: &str| {
            if log {
                eprintln!("  sync {what}: {} ms", last.elapsed().as_millis());
            }
            last = Instant::now();
        };
        self.windows.borrow_mut().push(w.as_weak());
        // The broadcast surface shows its form and sources at once.
        self.push_broadcast();
        self.seq_publish(false);
        self.keys.opened.set(Some(Instant::now()));
        let shown = Rc::new(Cell::new(Instant::now()));
        self.presented
            .borrow_mut()
            .insert(w.window() as *const _ as usize, shown.clone());
        watch_frames(
            w,
            shown,
            std::env::var_os("PATCH_TRACE_RENDER").map(Into::into),
        );
        self.rebuild_graph();
        mark("graph");
        self.rebuild_strips();
        mark("strips");
        self.rebuild_devices();
        mark("devices");
        self.rebuild_presets();
        self.rebuild_selection();
        mark("presets+selection");
        self.poll_dj();
        mark("dj");
        if w.get_surface() == "recorder" {
            self.poll_recorder();
        }
        mark("recorder");
        // A new window needs every readout now, not at the next 5 Hz update.
        self.last_text.set(f64::NEG_INFINITY);
        self.tick(if self.frozen {
            self.last_t.get()
        } else {
            self.start.elapsed().as_secs_f64() + 10.0
        });
        mark("first tick");
    }
}

/// Persist finished analyses in the library database (a worker thread).
fn store_analyses(done: Vec<(String, patch_analysis::Analysis)>) {
    let Ok(db) = patch_library::open_default() else {
        return;
    };
    for (path, a) in done {
        if let Some(track) = db.track_by_path(&path)
            && let Err(e) = patch_engine::librarian::store(&db, track, &a)
        {
            eprintln!("library: analysis of {path} not saved: {e}");
        }
    }
}

/// "Deck A", "Deck A and Deck B".
fn decks_text(live: &[String]) -> String {
    let names: Vec<String> = live
        .iter()
        .map(|d| format!("Deck {}", d.trim_start_matches("deck-").to_uppercase()))
        .collect();
    names.join(" and ")
}

/// Save a new setup now (the controller shows it at once) and apply what
/// changed in the background (the bar reloads, the service installs):
/// `omarchy-patch setup apply <switches>`.
fn change_setup(before: patch_core::setup::Setup, after: patch_core::setup::Setup) {
    if patch_core::setup::save(&after).is_err() {
        return;
    }
    let changed: Vec<&str> = patch_core::setup::SWITCHES
        .iter()
        .map(|s| s.0)
        .filter(|id| before.get(id) != after.get(id))
        .collect();
    if changed.is_empty() {
        return;
    }
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new("setsid")
            .arg("-f")
            .arg(exe)
            .args(["setup", "apply"])
            .args(&changed)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// The loudness history as path commands, a column a unit wide and 0 (top)
/// … 1 (bottom) high: a smooth line through the measurements (at the
/// columns' centres, curved between them) and the area under it, closed.
/// (Steps drew a hard edge every 250 ms: harsh, distracting.)
fn loudness_outline(values: impl Iterator<Item = f32>) -> (String, String) {
    use std::fmt::Write;
    let pts: Vec<(f32, f32)> = values
        .enumerate()
        .map(|(i, v)| (i as f32 + 0.5, 1.0 - v.clamp(0.0, 1.0)))
        .collect();
    let Some(&(x0, y0)) = pts.first() else {
        return (String::new(), String::new());
    };
    // Flat past both ends, so the curve reaches the edges while it scrolls
    // (and over the few pixels left free by whole-pixel columns).
    let n = pts.len() as f32;
    let mut line = format!("M {} {y0:.4} L {x0} {y0:.4}", -n);
    // Catmull-Rom through the points as cubic Béziers: smooth, and never
    // overshooting much between two measurements.
    for i in 0..pts.len() - 1 {
        let p0 = pts[i.saturating_sub(1)];
        let (p1, p2) = (pts[i], pts[i + 1]);
        let p3 = pts[(i + 2).min(pts.len() - 1)];
        let c1 = (
            p1.0 + (p2.0 - p0.0) / 6.0,
            (p1.1 + (p2.1 - p0.1) / 6.0).clamp(0.0, 1.0),
        );
        let c2 = (
            p2.0 - (p3.0 - p1.0) / 6.0,
            (p2.1 - (p3.1 - p1.1) / 6.0).clamp(0.0, 1.0),
        );
        let _ = write!(
            line,
            " C {:.3} {:.4} {:.3} {:.4} {} {:.4}",
            c1.0, c1.1, c2.0, c2.1, p2.0, p2.1
        );
    }
    let yn = pts[pts.len() - 1].1;
    let _ = write!(line, " L {} {yn:.4}", n + 1.0);
    let area = format!("{line} L {} 1 L {} 1 Z", n + 1.0, -n);
    (area, line)
}

/// Make a folder a collection, and say how it went (off the UI thread).
fn add_folder(dir: &str) {
    let name = std::path::Path::new(dir)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| dir.to_string());
    if let Ok(mut n) = FOLDER_NOTE.lock() {
        *n = Some(format!("Adding “{name}”: it shows under Collections and fills as it is scanned…"));
    }
    // The librarian writes all the time; a busy library is a reason to wait
    // and try again (each step is safe to repeat), not to give up silently.
    let mut tries = 0;
    let result = loop {
        let r = patch_library::open_default()
            .and_then(|db| db.collection_from_folder(&name, std::path::Path::new(dir)));
        match r {
            Err(e) if tries < 5 && (e.contains("locked") || e.contains("busy")) => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
            r => break r,
        }
    };
    let m = match result {
        Ok((n, _)) => format!("Added collection “{name}”: {n} tracks, analysed in the background."),
        Err(e) => format!("Couldn't add {dir}: {e}"),
    };
    notify("Patch Library", &m);
    if let Ok(mut n) = FOLDER_NOTE.lock() {
        *n = Some(m);
    }
}

/// A folder the chooser handed back that is far more than one collection: a
/// drive, home, Music or `/`. The system chooser answers with the folder it
/// shows when one inside it was only highlighted: "NL Top 40" became the
/// whole drive (2026-11-12).
fn too_wide(dir: &str) -> bool {
    let path = std::path::Path::new(dir);
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let user = std::env::var("USER").unwrap_or_default();
    let drive = path.parent().is_some_and(|p| {
        p == std::path::Path::new(&format!("/run/media/{user}"))
            || p == std::path::Path::new(&format!("/media/{user}"))
    });
    drive
        || path == std::path::Path::new("/")
        || home.as_deref() == Some(path)
        || dj::music_dir() == path
}

/// What adding a folder came to, from its thread to the Library's status line.
static FOLDER_NOTE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// A desktop notification (the Patch window may be gone already).
fn notify(title: &str, body: &str) {
    if let Ok(mut child) = std::process::Command::new("notify-send")
        .args(["-a", "Patch", "-i", "omarchy-patch", title, body])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        // Collected when it ends, so no finished process is left behind.
        std::thread::spawn(move || child.wait());
    }
}

/// Deck tempo/key settings survive UI restarts (not reboots), like the decks.
fn pitch_path() -> std::path::PathBuf {
    patch_core::ipc::runtime_dir().join("dj-pitch")
}

fn save_pitch(p: &[dj::Pitch; 4]) {
    let line = |p: &dj::Pitch| {
        format!(
            "{} {} {} {}",
            p.tempo, p.range, p.keylock as u8, p.key_shift
        )
    };
    let _ = std::fs::write(pitch_path(), p.iter().map(line).collect::<Vec<_>>().join("\n"));
}

fn load_pitch() -> Option<[dj::Pitch; 4]> {
    let s = std::fs::read_to_string(pitch_path()).ok()?;
    let parse = |l: &str| -> Option<dj::Pitch> {
        let w: Vec<&str> = l.split_whitespace().collect();
        Some(dj::Pitch {
            tempo: w.first()?.parse().ok()?,
            range: w.get(1)?.parse().ok()?,
            keylock: w.get(2)? == &"1",
            key_shift: w.get(3)?.parse().ok()?,
            bend: 0.0,
        })
    };
    let mut lines = s.lines();
    Some([parse(lines.next()?)?, parse(lines.next()?)?, lines.next().and_then(parse).unwrap_or_default(), lines.next().and_then(parse).unwrap_or_default()])
}

/// EQ knob (−1 … 1) → dB: −1 is a kill, the left half −26 … 0 dB, the right
/// half 0 … +6 dB (the classic DJ-mixer range).
fn knob_db(v: f32) -> Option<f32> {
    if v <= -0.97 {
        None
    } else if v < 0.0 {
        Some(v * 26.0)
    } else {
        Some(v * 6.0)
    }
}

fn eq_text(v: f32) -> String {
    match knob_db(v) {
        None => "KILL".into(),
        Some(d) if d.abs() < 0.05 => "0 dB".into(),
        Some(d) => format!("{} dB", crate::num::signed(d, 1)),
    }
}

/// The filter knob as the engine sweeps it (see `patch_engine::eq`).
fn filter_text(v: f32) -> String {
    let hz = |f: f64| {
        if f >= 1000.0 {
            format!("{:.1}k", f / 1000.0)
        } else {
            format!("{f:.0}")
        }
    };
    if v.abs() < 0.02 {
        "OFF".into()
    } else if v < 0.0 {
        format!(
            "LPF {}",
            hz(20_000.0 * (60.0f64 / 20_000.0).powf(-v as f64))
        )
    } else {
        format!("HPF {}", hz(20.0 * (8_000.0f64 / 20.0).powf(v as f64)))
    }
}

/// Headphone level knob (−1 … 1) → −30 … +6 dB (0 dB at the centre).
fn cue_level_db(v: f32) -> f32 {
    if v < 0.0 { v * 30.0 } else { v * 6.0 }
}

/// Engine dB → the bipolar headphone knob, preserving unity at zero.
fn cue_level_knob(db: f32) -> f32 {
    (if db < 0.0 { db / 30.0 } else { db / 6.0 }).clamp(-1.0, 1.0)
}

fn cue_mix_text(v: f32) -> String {
    let m = (v + 1.0) / 2.0;
    if m < 0.02 {
        "CUE".into()
    } else if m > 0.98 {
        "MASTER".into()
    } else {
        format!("{:.0}% master", m * 100.0)
    }
}

/// "4 beats", "1/2 beat".
fn beats_text(b: f64) -> String {
    if b <= 0.0 {
        String::new()
    } else if b < 1.0 {
        format!("1/{:.0} beat", 1.0 / b)
    } else if (b - 1.0).abs() < 1e-6 {
        "1 beat".into()
    } else {
        format!("{} beats", (b * 100.0).round() / 100.0)
    }
}

/// One-key DJ actions (space plays, o loops, c cues…) only from a DJ's
/// hands: not in the first moments after a Patch window opened (it may have
/// taken the keyboard from a terminal you were typing in: typed text once
/// looped and stopped the decks), and not from different keys coming at
/// typing speed. The same key repeating (held, or tapped twice) and special
/// keys (arrows, Escape…) always pass.
#[derive(Default)]
struct KeyGuard {
    opened: Cell<Option<Instant>>,
    last: RefCell<Option<(Instant, String)>>,
}

impl KeyGuard {
    fn allow(&self, text: &str) -> bool {
        // Slint's special keys are control or private-use characters.
        let printable = text
            .chars()
            .next()
            .is_some_and(|c| c >= ' ' && !('\u{e000}'..='\u{f8ff}').contains(&c));
        // Headless shots type `PATCH_SHOT_KEYS` on their second frame,
        // well inside the opening guard.
        if !printable
            || (patch_host::shot::active()
                && std::env::var_os("PATCH_SHOT_KEYS").is_some())
        {
            return true;
        }
        let now = Instant::now();
        let prev = self.last.replace(Some((now, text.to_string())));
        if self
            .opened
            .get()
            .is_some_and(|t| now - t < Duration::from_millis(1500))
        {
            return false;
        }
        !prev.is_some_and(|(t, k)| k != text && now - t < Duration::from_millis(250))
    }
}

/// The update loop's per-second figures.
struct FrameLoop {
    second: Cell<Instant>,
    worst: Cell<Duration>,
    total: Cell<Duration>,
    // Spread the expensive text-rate polling work over three display frames.
    // None means there is no deferred phase; 0..=2 are the next phases.
    text_phase: Cell<Option<u8>>,
}

impl Default for FrameLoop {
    fn default() -> Self {
        FrameLoop {
            second: Cell::new(Instant::now()),
            worst: Cell::new(Duration::ZERO),
            total: Cell::new(Duration::ZERO),
            text_phase: Cell::new(None),
        }
    }
}

/// Note when the window presents a frame. With PATCH_TRACE_RENDER=<file>,
/// also append per window, every 10 s, how long each frame took to render
/// and how evenly frames were presented (renderer A/B).
fn watch_frames(w: &PatchWindow, shown: Rc<Cell<Instant>>, out: Option<std::path::PathBuf>) {
    struct T {
        began: Option<Instant>,
        last: Option<Instant>,
        since: Instant,
        render: Vec<f64>,
        gaps: Vec<f64>,
    }
    let surface = w.get_surface().to_string();
    let cell = RefCell::new(T {
        began: None,
        last: None,
        since: Instant::now(),
        render: vec![],
        gaps: vec![],
    });
    let win_key = crate::visual_gl::window_key(w.window());
    let weak = w.as_weak();
    let _ = w.window().set_rendering_notifier(move |state, api| {
        // The window's other hooks (the Visualiser's GPU drawing) first.
        crate::visual_gl::run_hook(win_key, state.clone(), api);
        // Then, drawn and not yet shown: the broadcast's picture, when this
        // window is it.
        crate::visual_gl::capture::run(win_key, &state, api, || {
            weak.upgrade().map(|w| {
                let s = w.window().size();
                (s.width, s.height)
            })
        });
        let now = Instant::now();
        let mut t = cell.borrow_mut();
        match state {
            slint::RenderingState::BeforeRendering => {
                if let Some(w) = weak.upgrade() {
                    glide(&w);
                }
                if loop_trace() {
                    eprintln!("L {:.3} draw {surface}", LOOP_T0.with(|t| now.duration_since(*t)).as_secs_f64() * 1e3);
                }
                t.began = Some(now);
            }
            slint::RenderingState::AfterRendering => {
                if loop_trace() {
                    eprintln!("L {:.3} drawn {surface}", LOOP_T0.with(|t| now.duration_since(*t)).as_secs_f64() * 1e3);
                }
                shown.set(now);
                let Some(out) = &out else { return };
                if let Some(b) = t.began.take() {
                    t.render.push((now - b).as_secs_f64() * 1e3);
                }
                if let Some(l) = t.last.replace(now) {
                    t.gaps.push((now - l).as_secs_f64() * 1e3);
                }
                if t.since.elapsed() >= Duration::from_secs(10) {
                    let secs = t.since.elapsed().as_secs_f64();
                    let pct = |v: &mut Vec<f64>, q: f64| {
                        v.sort_by(f64::total_cmp);
                        v.get(((v.len() as f64 - 1.0) * q).round() as usize).copied().unwrap_or(0.0)
                    };
                    let n = t.render.len();
                    let avg = t.render.iter().sum::<f64>() / n.max(1) as f64;
                    let (r50, r99, rmax) = (pct(&mut t.render, 0.5), pct(&mut t.render, 0.99), pct(&mut t.render, 1.0));
                    let (g50, g99, gmax) = (pct(&mut t.gaps, 0.5), pct(&mut t.gaps, 0.99), pct(&mut t.gaps, 1.0));
                    // A frame presented later than 1.5 frame periods: a visible hitch.
                    let late = t.gaps.iter().filter(|g| **g > g50 * 1.5).count();
                    let line = format!(
                        "{surface} fps {:.1} render avg {avg:.2} p50 {r50:.2} p99 {r99:.2} max {rmax:.2} gap p50 {g50:.2} p99 {g99:.2} max {gmax:.2} late {late}\n",
                        n as f64 / secs
                    );
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(out) {
                        use std::io::Write;
                        let _ = f.write_all(line.as_bytes());
                    }
                    t.render.clear();
                    t.gaps.clear();
                    t.since = now;
                }
            }
            _ => {}
        }
    });
}

thread_local! {
    static LOOP_T0: Instant = Instant::now();
    static GLIDE: RefCell<[Glide; 4]> = RefCell::default();
    /// The pre-listen's playhead for the Library's HUD and strip: the last
    /// reading (seconds, when read, track length) and what was last shown.
    pub(crate) static HUD_GLIDE: RefCell<HudGlide> = RefCell::default();
}

#[derive(Default)]
pub(crate) struct HudGlide {
    pub read: Option<(f64, Instant, f64)>,
    shown: Option<(f64, Instant)>,
}

/// The pre-listen's playhead, drawn on the display's frame like a deck's
/// strip: on from what was last shown at playing speed, eased toward each
/// new reading (they come a Library poll apart, ~0.6 s, and each differs
/// a little: set as read, the playhead hopped at every poll — the HUD
/// looked like it refreshed); a seek or a cue is taken at once.
fn glide_hud(w: &PatchWindow) {
    let d = w.global::<DjState>();
    if d.get_hud_state() != "playing" && d.get_prev_title().is_empty() {
        return;
    }
    let now = Instant::now();
    let shown = HUD_GLIDE.with(|g| {
        let mut g = g.borrow_mut();
        let (pos, at, span) = g.read?;
        if span <= 0.0 {
            return None;
        }
        // At most a second on: a stalled poll holds instead of running away.
        let read = pos + now.duration_since(at).as_secs_f64().min(1.0);
        let t = match g.shown {
            Some((s, when)) => {
                let dt = now.duration_since(when).as_secs_f64();
                let ahead = s + dt;
                let off = read - ahead;
                if off.abs() > 0.5 { read } else { ahead + off * (1.0 - (-dt / 0.5).exp()) }
            }
            None => read,
        };
        let t = t.clamp(0.0, span);
        g.shown = Some((t, now));
        Some((t / span) as f32)
    });
    if let Some(p) = shown {
        trace_hud(format_args!("F {p:.6}"));
        d.set_hud_progress(p);
        d.set_prev_progress(p);
        w.window().request_redraw();
    }
}

/// PATCH_TRACE_HUD=<file>: one line per HUD frame ("F ms progress") and
/// per change of what the HUD sees ("S ms what"), to check its motion and
/// any flicker frame by frame.
pub(crate) fn trace_hud(line: std::fmt::Arguments) {
    use std::io::Write;
    static OUT: std::sync::OnceLock<Option<std::sync::Mutex<std::fs::File>>> = std::sync::OnceLock::new();
    let Some(out) = OUT.get_or_init(|| {
        let path = std::env::var_os("PATCH_TRACE_HUD")?;
        std::fs::File::create(path).ok().map(std::sync::Mutex::new)
    }) else {
        return;
    };
    let ms = LOOP_T0.with(|t| t.elapsed()).as_secs_f64() * 1e3;
    let _ = writeln!(out.lock().unwrap(), "{ms:.1} {line}");
}

/// The reading the HUD's playhead eases toward; None when nothing plays
/// (the playhead then stays where it was set).
pub(crate) fn hud_reading(read: Option<(f64, Instant, f64)>) {
    HUD_GLIDE.with(|g| {
        let mut g = g.borrow_mut();
        if read.is_none() {
            g.shown = None;
        }
        g.read = read;
    });
}

/// Each deck's playhead as last read (seconds, speed, when), its beat grid
/// (bpm, offset), the time its scrolling strip is drawn around, whether
/// its window may draw every display frame (the governor at full rate), and
/// the playhead last shown (seconds, when).
#[derive(Clone, Copy, Default)]
struct Glide {
    head: Option<(f64, f64, Instant)>,
    grid: Option<(f64, f64)>,
    anchor: Option<f64>,
    every_frame: bool,
    shown: Option<(f64, Instant)>,
}

/// Place a Deck window's scrolling strip and beat for the moment it draws.
/// The update timer runs at its own 62.5 Hz against the display's 60 (or
/// more): positions taken at the tick were shown up to a frame early or
/// late, and every few hundred milliseconds one frame moved twice as far,
/// a visible stutter. Moved on from the last reading here, every drawn
/// frame advances by exactly the time that passed.
///
/// While the deck moves, the window also asks for its next frame here. A
/// redraw asked for by the timer started wherever the tick fell in the
/// display's frame, so the strip was sampled up to a frame early or late
/// (a few pixels of judder at 8 s across), and a tick that slipped past a
/// frame callback skipped that frame (deck windows: 1–3 late frames every
/// 4 s). Asked for here, the next draw starts on the compositor's frame
/// callback: one draw per refresh, each at the same point in it.
fn glide(w: &PatchWindow) {
    if patch_host::shot::active() {
        return;
    }
    if w.get_surface() == "library" {
        glide_hud(w);
        return;
    }
    if w.get_surface() == "dj-club" {
        let state = w.global::<crate::ui::BoothControls>();
        for id in [state.get_left_deck(), state.get_right_deck()] { glide_deck(w, AudioApp::deck_index(&id)); }
        return;
    }
    if w.get_surface() == "dj-mini" {
        glide_deck(w, 0);
        glide_deck(w, 1);
        return;
    }
    let i = match w.get_surface().as_str() {
        "deck-a" => 0,
        "deck-b" => 1,
        "deck-c" => 2,
        "deck-d" => 3,
        _ => return,
    };
    glide_deck(w, i);
}

fn glide_deck(w: &PatchWindow, i: usize) {
    let g = GLIDE.with(|g| g.borrow()[i]);
    let Some((t, speed, when)) = g.head else {
        return;
    };
    // At most 100 ms on: a stalled update holds instead of running away.
    let read = t + speed * when.elapsed().as_secs_f64().min(0.1);
    // The engine stamps a block when its callback runs, which wanders by a
    // millisecond or more (more when a cycle runs late): taken as read, the
    // strip trembled. Carried on from what was last shown at the deck's
    // speed and pulled toward the reading by a share of the difference
    // (gone in about a quarter second); a jump (seek, cue, load, a stall)
    // is taken at once.
    let now = Instant::now();
    let t = match g.shown {
        Some((s, at)) if speed != 0.0 => {
            let dt = now.duration_since(at).as_secs_f64();
            let ahead = s + speed * dt;
            let off = read - ahead;
            if off.abs() > 0.05 {
                read
            } else {
                ahead + off * (1.0 - (-dt / 0.25).exp())
            }
        }
        _ => read,
    };
    GLIDE.with(|g| g.borrow_mut()[i].shown = Some((t, now)));
    let d = w.global::<DjState>();
    if let Some(anchor) = g.anchor {
        let shift = ((anchor - t) / SCOPE_STRIP_S) as f32;
        match i { 0 => d.set_scope_a_shift(shift), 1 => d.set_scope_b_shift(shift), 2 => d.set_scope_c_shift(shift), _ => d.set_scope_d_shift(shift) }
        trace_glide(w, i, now, read, t, anchor);
    }
    if let Some((bpm, offset)) = g.grid {
        let phase = ((t - offset) / (60.0 / bpm)) as f32;
        match i { 0 => d.set_phase_a(phase), 1 => d.set_phase_b(phase), 2 => d.set_phase_c(phase), _ => d.set_phase_d(phase) }
    }
    // The Mini Booth's records: the beat from this smoothed playhead (or
    // two a second without a grid), written here only.
    if w.get_surface() == "dj-mini" {
        let spin = g.grid.map_or(t * 2.0, |(bpm, offset)| (t - offset) / (60.0 / bpm)) as f32;
        match i { 0 => d.set_spin_a(spin), 1 => d.set_spin_b(spin), 2 => d.set_spin_c(spin), _ => d.set_spin_d(spin) }
    }
    if g.every_frame && speed != 0.0 {
        w.window().request_redraw();
    }
}

/// PATCH_TRACE_GLIDE=<file>: every strip placement (ms, window, deck,
/// playhead read, shown, anchor), to check its motion frame by frame.
fn trace_glide(w: &PatchWindow, i: usize, now: Instant, read: f64, shown: f64, anchor: f64) {
    static OUT: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    let Some(out) = OUT.get_or_init(|| std::env::var_os("PATCH_TRACE_GLIDE").map(Into::into)) else {
        return;
    };
    thread_local!(static BUF: RefCell<String> = const { RefCell::new(String::new()) });
    BUF.with(|b| {
        let mut b = b.borrow_mut();
        let ms = LOOP_T0.with(|t| now.duration_since(*t)).as_secs_f64() * 1e3;
        let _ = std::fmt::Write::write_fmt(
            &mut *b,
            format_args!("{ms:.3} {} {i} {read:.6} {shown:.6} {anchor:.6}\n", w.get_surface()),
        );
        if b.len() > 32 << 10 {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(out) {
                use std::io::Write;
                let _ = f.write_all(b.as_bytes());
            }
            b.clear();
        }
    });
}

fn loop_trace() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("PATCH_TRACE_LOOP").is_some())
}

#[cfg(test)]
mod key_guard_tests {
    use super::*;

    #[test]
    fn typing_is_not_djing() {
        let g = KeyGuard::default();
        assert!(g.allow(" "));
        // "o" right after the space: typing.
        assert!(!g.allow("o"));
        // Held, or tapped again: passes.
        assert!(g.allow("o"));
        // Arrows always pass.
        assert!(g.allow("\u{f700}"));
        std::thread::sleep(Duration::from_millis(260));
        assert!(g.allow("c"));
        g.opened.set(Some(Instant::now()));
        assert!(!g.allow("c"));
    }
}

#[cfg(test)]
mod mixer_selection_tests {
    use super::{selection_exists, strip_meter_kind};
    #[test]
    fn microphone_selection_survives_playback_stream_churn_until_removed() {
        assert!(selection_exists(
            "engine.mic-1",
            ["app:player"],
            ["app:player", "engine.mic-1"]
        ));
        assert!(selection_exists(
            "engine.mic-1",
            ["app:new-player"],
            ["app:new-player", "engine.mic-1"]
        ));
        assert!(!selection_exists(
            "engine.mic-1",
            ["app:player"],
            ["app:player"]
        ));
        assert!(selection_exists("master", [], []));
        assert!(selection_exists("output", [], []));
        assert!(selection_exists("app:player", ["app:player"], []));
        assert!(!selection_exists(
            "app:gone",
            ["app:player"],
            ["engine.mic-1"]
        ));
    }

    #[test]
    fn every_mixer_strip_uses_absolute_system_meter_scale() {
        use patch_ui::meter::MeterKind;
        assert_eq!(
            strip_meter_kind("engine.dj-mixer", false),
            MeterKind::System
        );
        assert_eq!(strip_meter_kind("engine.deck-a", false), MeterKind::System);
        assert_eq!(strip_meter_kind("engine.deck-b", false), MeterKind::System);
        assert_eq!(strip_meter_kind("app:player", false), MeterKind::System);
    }
}

#[cfg(test)]
mod output_strip_tests {
    use super::{
        app_volume_text, fader_db, fader_db_with_headroom, fader_pos,
        fader_pos_with_headroom, main_destination_label, output_db, output_pos, output_text,
        strip_gain_text, strip_has_headroom,
    };

    /// The Mixer's Output strip and the bar's OUTPUT slider are one control on
    /// one scale: the percentage both show *is* the fader position, 0 is
    /// silence, and a trip through the sink lands back on the same position.
    #[test]
    fn the_output_strip_is_the_bars_slider() {
        // The bar's numbers: 100 % is 0 dB, 25 % is −36.1 dB, and Pulse's own
        // 60 dB cubic law puts −60 dB at 10 %.
        assert_eq!(output_text(output_pos(0.0)), "100%");
        assert_eq!(output_text(output_pos(-60.0)), "10%");
        assert_eq!(output_text(output_pos(60.0 * 0.25f32.log10())), "25%");
        // The floor is silence, not the mixer's −60 dB = 10 %: this is what
        // could never read 0 while the strip used the mixer's dB law.
        assert_eq!(output_text(output_pos(-120.0)), "0%");
        assert!(output_pos(f32::NEG_INFINITY) == 0.0);
        // Dragging settles where the bar's slider would: same position, and
        // the sink's volume the bar shows for it.
        for pos in [0.02, 0.05, 0.25, 0.5, 0.909, 1.0] {
            let round_trip = output_pos(output_db(pos));
            assert!((round_trip - pos).abs() < 1e-4, "{pos} → {round_trip}");
        }
        assert_eq!(output_db(0.0), -120.0);
    }

    #[test]
    fn app_volume_readouts_follow_the_stream_scale_and_available_headroom() {
        assert_eq!(app_volume_text(0.0, true), "100%");
        assert_eq!(app_volume_text(-6.0, true), "50%");
        assert_eq!(app_volume_text(-20.0, true), "10%");
        assert_eq!(app_volume_text(6.0, true), "200%");
        assert_eq!(app_volume_text(6.0, false), "100%");
        assert_eq!(app_volume_text(60.0 * 0.5f32.log10(), false), "50%");
        assert_eq!(app_volume_text(60.0 * 0.25f32.log10(), false), "25%");
        assert_eq!(app_volume_text(f32::NEG_INFINITY, true), "0%");
    }

    #[test]
    fn mixer_refresh_keeps_application_faders_on_the_percentage_scale() {
        assert_eq!(
            strip_gain_text("application:chromium", true, false, 0.0),
            "100%"
        );
        assert_eq!(
            strip_gain_text("application:atlas", true, false, 60.0 * 0.5f32.log10()),
            "50%"
        );
        assert_eq!(
            strip_gain_text("application:atlas", true, false, -120.0),
            "0%"
        );
        assert_eq!(strip_gain_text("system-sounds", true, false, 0.0), "100%");
        assert_eq!(
            strip_gain_text("engine.mic-1", true, true, -6.0),
            "−6.0 dB"
        );
    }

    #[test]
    fn main_destination_names_are_short_but_specific() {
        assert_eq!(
            main_destination_label("Device · Focusrite Scarlett 2i2 2nd Gen Analog Stereo"),
            "Focusrite Scarlett"
        );
        assert_eq!(main_destination_label("Left · 800 Series Playback"), "Left");
        assert_eq!(main_destination_label("Right + Rear · Headphones"), "Right + Rear");
    }

    #[test]
    fn the_top_of_the_fader_is_what_the_strip_can_do() {
        for id in ["master", "engine.dj-mixer", "engine.mic-1", "engine.pattern", "engine.sequencer"] {
            assert!(strip_has_headroom(id, true), "{id}");
            assert!((fader_db(id, true, 1.0) - 6.0).abs() < 1e-3, "{id}");
            assert!((fader_pos(id, true, 0.0) - 60.0 / 66.0).abs() < 1e-4, "{id}");
            // The engine is down: the program is a bus volume and stops at 0 dB.
            assert!(!strip_has_headroom(id, false));
            assert!(fader_db(id, false, 1.0).abs() < 1e-3, "{id}");
        }
        let id = "application:browser";
        assert!(!strip_has_headroom(id, true));
        assert!(fader_db(id, true, 1.0).abs() < 1e-3, "{id}");
        assert!((fader_pos(id, true, 0.0) - 1.0).abs() < 1e-4, "{id}");
        // Application volume follows Pulse's cubic percentage scale too.
        assert!((fader_db(id, true, 0.5) - 60.0 * 0.5f32.log10()).abs() < 1e-3, "{id}");
        // System sounds is a sink-volume control like MAIN. Its dB value
        // maps through Pulse's cubic percentage scale.
        assert!(!strip_has_headroom("system-sounds", true));
        assert!((fader_db("system-sounds", true, 1.0)).abs() < 1e-4);
        for pos in [0.0, 0.02, 0.25, 0.5, 0.909, 1.0] {
            let db = fader_db("system-sounds", true, pos);
            assert!((fader_pos("system-sounds", true, db) - pos).abs() < 1e-4);
        }
        assert_eq!(output_text(fader_pos("system-sounds", true, -120.0)), "0%");
        assert_eq!(output_text(fader_pos("system-sounds", true, 0.0)), "100%");
        // Main stays on the bar's amplitude scale.
        assert!((fader_db("output", true, 1.0) - output_db(1.0)).abs() < 1e-4);
        assert!((fader_pos("output", true, 0.0) - 1.0).abs() < 1e-4);
        assert!((fader_pos("output", true, -60.0) - output_pos(-60.0)).abs() < 1e-4);
    }

    #[test]
    fn an_app_channel_and_a_direct_stream_have_different_fader_caps() {
        let chromium = "application:chromium";
        // A routed Patch channel has +6 dB available; unity is 60/66 of
        // travel, and every displayed value maps back to the same gain.
        assert!((fader_pos_with_headroom(chromium, true, true, 0.0) - 60.0 / 66.0).abs() < 1e-4);
        assert!((fader_db_with_headroom(chromium, true, true, 1.0) - 6.0).abs() < 1e-4);
        for pos in [0.0, 0.25, 60.0 / 66.0, 0.95, 1.0] {
            let db = fader_db_with_headroom(chromium, true, true, pos);
            assert!((fader_pos_with_headroom(chromium, true, true, db) - pos).abs() < 1e-4);
        }

        // A direct PipeWire stream follows Pulse's cubic slider law.
        assert_eq!(fader_db_with_headroom(chromium, true, false, 1.0), 0.0);
        assert!((fader_db_with_headroom(chromium, true, false, 0.5) - 60.0 * 0.5f32.log10()).abs() < 1e-4);
        assert!((fader_pos_with_headroom(chromium, true, false, 60.0 * 0.5f32.log10()) - 0.5).abs() < 1e-4);
        // A stale headroom flag cannot promise gain the stopped engine cannot apply.
        assert_eq!(fader_db_with_headroom(chromium, false, true, 1.0), 0.0);
        assert!((fader_pos_with_headroom(chromium, false, true, 60.0 * 0.5f32.log10()) - 0.5).abs() < 1e-4);
    }
}

#[path = "mic_view.rs"]
mod mic_view;

#[test]
fn headphone_level_round_trips_and_mix_reaches_both_sources() {
    for db in [-30.0, -15.0, 0.0, 3.0, 6.0] {
        assert!((cue_level_db(cue_level_knob(db)) - db).abs() < 1e-6);
    }
    assert_eq!(cue_mix_text(-1.0), "CUE");
    assert_eq!(cue_mix_text(0.0), "50% master");
    assert_eq!(cue_mix_text(1.0), "MASTER");
}
