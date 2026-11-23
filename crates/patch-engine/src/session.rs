//! The DJ session, owned by the Patch service (ADR-002, ADR-003).
//!
//! Everything a DJ surface shows or changes lives here, not in a window:
//! the crossfader, the deck faders, auto-gain and its trims. The DJ Booth,
//! Light DJ, the CLI and MIDI all read and change this one state; closing
//! every window changes nothing. The engine turns it into channel gains
//! (ramped per block on the RT thread, so moves never click).
//!
//! Control plane only: runs on the session thread and the control socket,
//! persists to `~/.local/state/omarchy-patch/dj-session.json`.

use serde::{Deserialize, Serialize};

/// DJ crossfader law: a deck stays at unity from its side to the centre.
/// The other half fades it to silence; both decks are unity at the centre.
pub fn crossfade(x: f32) -> (f32, f32) {
    let x = x.clamp(0.0, 1.0) as f64;
    let a = ((x - 0.5).max(0.0) * std::f64::consts::PI).cos();
    let b = (x.min(0.5) * std::f64::consts::PI).sin();
    // Make the fully closed deck exactly silent.
    let snap = |g: f64| if g < 1e-6 { 0.0 } else { g as f32 };
    (snap(a), snap(b))
}

/// Which side of the crossfader a channel plays on (a club mixer's
/// assign switch): A, B, or THRU (the crossfader leaves it alone).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum XfAssign {
    A,
    Thru,
    B,
}

impl XfAssign {
    pub fn id(self) -> &'static str {
        match self {
            XfAssign::A => "a",
            XfAssign::Thru => "thru",
            XfAssign::B => "b",
        }
    }
    pub fn parse(id: &str) -> Option<XfAssign> {
        [XfAssign::A, XfAssign::Thru, XfAssign::B].into_iter().find(|a| a.id() == id)
    }
}

/// The crossfader curve. `Mix` is the tested law ([`crossfade`]); the AI
/// DJ's transitions were tuned and listened to with it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum XfCurve {
    /// Constant power over the whole throw: −3 dB each at the centre.
    Smooth,
    /// Each side at unity up to the centre, then a constant-power fade out.
    #[default]
    Mix,
    /// Scratch: open almost at once, the last few percent of the throw cuts.
    Cut,
}

impl XfCurve {
    pub const ALL: [XfCurve; 3] = [XfCurve::Smooth, XfCurve::Mix, XfCurve::Cut];
    pub fn id(self) -> &'static str {
        match self {
            XfCurve::Smooth => "smooth",
            XfCurve::Mix => "mix",
            XfCurve::Cut => "cut",
        }
    }
    pub fn parse(id: &str) -> Option<XfCurve> {
        XfCurve::ALL.into_iter().find(|c| c.id() == id)
    }
    /// (A side, B side) gain at position `x`.
    pub fn gains(self, x: f32) -> (f32, f32) {
        let x = x.clamp(0.0, 1.0);
        match self {
            XfCurve::Mix => crossfade(x),
            XfCurve::Smooth => {
                let t = x as f64 * std::f64::consts::FRAC_PI_2;
                let snap = |g: f64| if g < 1e-6 { 0.0 } else { g as f32 };
                (snap(t.cos()), snap(t.sin()))
            }
            XfCurve::Cut => {
                // Full within 4 % of the far end, silent at it.
                const W: f32 = 0.04;
                (((1.0 - x) / W).min(1.0), (x / W).min(1.0))
            }
        }
    }
}

/// The A/B controls as the AI DJ means them (its crossfader and the two
/// faders), kept so they can be played on the channel faders when the
/// mixer is not set up for its crossfader (see [`Mixer::ai_on_faders`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AiControls {
    pub xfader: f32,
    pub faders: [f32; 2],
}

impl Default for AiControls {
    fn default() -> Self {
        AiControls { xfader: 0.5, faders: [UNITY; 2] }
    }
}

impl AiControls {
    /// The channel fader positions that give A and B the gain these
    /// controls give through the tested crossfader law.
    pub fn as_faders(self) -> [f32; 2] {
        let (xa, xb) = crossfade(self.xfader);
        let compose = |f: f32, x: f32| if f <= 0.0 || x <= 0.0 { 0.0 } else { fader_pos(fader_db(f) + 20.0 * x.log10()) };
        [compose(self.faders[0], xa), compose(self.faders[1], xb)]
    }
}

/// Channel fader law, shared with Patch's mixer: position 0..1 is linear in
/// dB from −60 to +6 dB (unity at 60/66 ≈ 0.909); the bottom is silence.
pub fn fader_db(pos: f32) -> f32 {
    (pos.clamp(0.0, 1.0) * 66.0 - 60.0).clamp(-60.0, 6.0)
}

/// Fader position for a gain in dB (inverse of [`fader_db`]).
pub fn fader_pos(db: f32) -> f32 {
    ((db + 60.0) / 66.0).clamp(0.0, 1.0)
}

pub const UNITY: f32 = 60.0 / 66.0;

/// How loud the DJ Booth plays: the loudness auto-gain matches every deck
/// to (quiet ones lifted, true peak kept under −2.5 dBTP and at most
/// +6 dB; loud ones turned down). The master limiter holds −1 dBFS in
/// every mode. At −9 hot house masters kept the deck meters in the red
/// (2026-10-13): balanced is the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Levels {
    /// −9 LUFS: club-master loud, the limiter works on the peaks.
    Loud,
    /// −11 LUFS: loud, with room for overlaps and EQ boosts.
    #[default]
    Balanced,
    /// −14 LUFS: the streaming target, the most headroom.
    Streaming,
}

impl Levels {
    pub const ALL: [Levels; 3] = [Levels::Loud, Levels::Balanced, Levels::Streaming];

    pub fn lufs(self) -> f64 {
        match self {
            Levels::Loud => -9.0,
            Levels::Balanced => -11.0,
            Levels::Streaming => -14.0,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Levels::Loud => "loud",
            Levels::Balanced => "balanced",
            Levels::Streaming => "streaming",
        }
    }

    pub fn about(self) -> &'static str {
        match self {
            Levels::Loud => "−9 LUFS: club-master loud, the limiter works on the peaks",
            Levels::Balanced => "−11 LUFS: loud, with room for overlaps and EQ boosts",
            Levels::Streaming => "−14 LUFS: the streaming target, the most headroom",
        }
    }

    pub fn parse(id: &str) -> Option<Levels> {
        Levels::ALL.into_iter().find(|l| l.id() == id)
    }
}

static LEVELS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);

/// The levels in use (the session's, once loaded): live decks and
/// rehearsals match the same loudness.
pub fn levels() -> Levels {
    Levels::ALL
        .get(LEVELS.load(std::sync::atomic::Ordering::Relaxed) as usize)
        .copied()
        .unwrap_or_default()
}

pub fn set_levels(l: Levels) {
    let i = Levels::ALL.iter().position(|x| *x == l).unwrap_or(1);
    LEVELS.store(i as u8, std::sync::atomic::Ordering::Relaxed);
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mixer {
    /// Channel fader positions, deck A and B.
    pub faders: [f32; 2],
    /// Additional manual decks C/D. Old sessions keep these closed.
    pub extra_faders: [f32; 2],
    /// Crossfader: 0 = deck A, 1 = deck B.
    pub xfader: f32,
    pub auto_gain: bool,
    /// Auto-gain per deck, dB (0 when off or not analysed). Derived.
    #[serde(skip)]
    pub trims_db: [f32; 2],
    #[serde(skip)]
    pub extra_trims_db: [f32; 2],
    /// The DJ Booth as one channel on the main mix: its fader (dB) and mute.
    #[serde(default)]
    pub group_db: f32,
    #[serde(default)]
    pub group_mute: bool,
    /// Each channel's crossfader side, A B C D. C plays on A's side and D
    /// on B's, as before the switch existed.
    pub assign: [XfAssign; 4],
    pub curve: XfCurve,
    /// The AI DJ's own crossfader and faders (not saved).
    #[serde(skip)]
    pub ai: AiControls,
}

impl Default for Mixer {
    fn default() -> Self {
        Mixer {
            faders: [UNITY; 2],
            extra_faders: [0.0; 2],
            xfader: 0.5,
            auto_gain: true,
            trims_db: [0.0; 2],
            extra_trims_db: [0.0; 2],
            group_db: 0.0,
            group_mute: false,
            assign: [XfAssign::A, XfAssign::B, XfAssign::A, XfAssign::B],
            curve: XfCurve::Mix,
            ai: AiControls::default(),
        }
    }
}

impl Mixer {
    pub fn faders4(&self) -> [f32; 4] {
        [self.faders[0], self.faders[1], self.extra_faders[0], self.extra_faders[1]]
    }
    pub fn trims4(&self) -> [f32; 4] {
        [self.trims_db[0], self.trims_db[1], self.extra_trims_db[0], self.extra_trims_db[1]]
    }
    /// The crossfader's gain on channel `i` (A B C D): its side's, or
    /// unity on THRU.
    pub fn xf_gain(&self, i: usize) -> f32 {
        let (xa, xb) = self.curve.gains(self.xfader);
        match self.assign[i] {
            XfAssign::A => xa,
            XfAssign::B => xb,
            XfAssign::Thru => 1.0,
        }
    }

    /// Total gain per deck in dB, A B C D: fader × crossfader × trim
    /// (−120 = silent).
    pub fn gains4_db(&self) -> [f32; 4] {
        let (faders, trims) = (self.faders4(), self.trims4());
        std::array::from_fn(|i| {
            let x = self.xf_gain(i);
            if faders[i] > 0.0 && x > 0.0 {
                let trim = if self.auto_gain { trims[i] } else { 0.0 };
                (fader_db(faders[i]) + 20.0 * x.log10() + trim).max(-120.0)
            } else {
                -120.0
            }
        })
    }

    /// Decks A and B's total gain in dB.
    pub fn gains_db(&self) -> [f32; 2] {
        let g = self.gains4_db();
        [g[0], g[1]]
    }

    /// The AI DJ blends on the channel faders unless A is on the A side,
    /// B on the B side and the curve is the one it was tuned with: its
    /// crossfader moves would otherwise do nothing (THRU) or sound
    /// different (another curve). On the faders they give the same gains.
    pub fn ai_on_faders(&self) -> bool {
        !(self.assign[0] == XfAssign::A && self.assign[1] == XfAssign::B && self.curve == XfCurve::Mix)
    }

    /// The AI's controls as they stand: on the crossfader, the mixer's
    /// own (so a move changes only what it names, as before the switch);
    /// on the faders, the ones it last asked for.
    pub fn ai_controls(&self) -> AiControls {
        if self.ai_on_faders() {
            self.ai
        } else {
            AiControls { xfader: self.xfader, faders: self.faders }
        }
    }

    /// Play the AI's controls: on the crossfader and faders as they are,
    /// or as fader positions alone (the crossfader stays where it is).
    pub fn apply_ai(&mut self, ai: AiControls) {
        self.ai = ai;
        if self.ai_on_faders() {
            self.faders = ai.as_faders();
        } else {
            self.xfader = ai.xfader;
            self.faders = ai.faders;
        }
    }
}

/// DJ EQ knob (−1 … 1) → band dB: kill at the bottom, −26 dB … 0 below
/// the centre, 0 … +6 dB above (None = kill).
pub fn knob_db(v: f32) -> Option<f32> {
    if v <= -0.97 {
        None
    } else if v < 0.0 {
        Some(v * 26.0)
    } else {
        Some(v * 6.0)
    }
}

/// The whole persisted session.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub mixer: Mixer,
    /// Manual input trim before DJ EQ, independent of loudness normalization.
    pub input_trim_db: [f32; 4],
    /// EQ knobs per deck: [high, mid, low, filter], each −1 … 1.
    pub eq: [[f32; 4]; 2],
    pub extra_eq: [[f32; 4]; 2],
    /// Tracks to play next (paths), in order.
    pub queue: Vec<String>,
    /// Deterministic auto-mix, switched on by a human (= `ai_mode` Act).
    pub automix: bool,
    /// How far the AI DJ may go: observe, suggest, prepare, act.
    pub ai_mode: patch_core::aivis::Mode,
    pub direction: patch_core::intent::Direction,
    /// Tracks played this session (newest last, at most 200).
    pub history: Vec<String>,
    /// The AI DJ's set, when one runs.
    pub set: Set,
    /// Techniques of the last transitions, newest first (at most 4): the
    /// AI varies them, across restarts and engine upgrades too.
    pub techniques: Vec<String>,
    /// A desktop notification for each new song in a set (off unless the
    /// DJ switches it on in the controller).
    pub notify_tracks: bool,
    /// How loud the decks play (auto-gain's loudness).
    pub levels: Levels,
    /// How adventurous the AI DJ's transitions are, and how much of each
    /// track plays (see `patch_core::mix_style`). Kept with the session so
    /// it survives restarts and engine upgrades.
    pub mix_style: patch_core::mix_style::MixStyle,
    /// Where the AI DJ takes the set's tempo (see `patch_core::tempo_plan`).
    pub tempo: patch_core::tempo_plan::TempoPlan,
}

/// A set the AI DJ plays: an energy arc over `minutes`, steered by the DJ.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Set {
    pub on: bool,
    pub id: String,
    pub name: String,
    pub collections: Vec<i64>,
    pub paused: bool,
    pub paused_at: u64,
    pub paused_seconds: u64,
    pub resume_decks: [bool; 2],
    pub saved_set_id: String,
    pub saved_set_revision: u64,
    pub ended: bool,
    pub transitions: Vec<serde_json::Value>,
    pub takes: Vec<serde_json::Value>,
    /// Unix seconds when it started.
    pub started: u64,
    pub minutes: f64,
    /// "journey" (warm-up, build, peak, release), "warmup", "peak".
    pub arc: String,
    /// Tempo corridor (lo, hi) BPM, when asked for.
    pub bpm: Option<(f64, f64)>,
    /// Play only from this folder (a crate), when set.
    pub crate_dir: Option<String>,
    /// Restrict automated picks to this library collection.
    pub collection: Option<i64>,
    /// A direction the DJ asked for, for this many more tracks.
    pub steer: Option<(patch_core::intent::Direction, u32)>,
    /// One-shot preferences for the next pick.
    pub surprise: bool,
    pub similar: bool,
    /// The AI may play drum swaps (another track's drums under the incoming
    /// track for a phrase) where one rehearses clean. Off unless asked for.
    pub drum_swaps: bool,
    /// Temporary fast-cycle testing, counted from transition completion.
    pub test_bars: Option<u32>,
    /// Playing path and beat at the start of its solo passage.
    pub test_since: Option<(String, f64)>,
    pub log: Vec<SetEntry>,
    /// Started without a length: it plays until stopped (its arc comes
    /// round again), rather than ending after `minutes`.
    pub open: bool,
    /// Seconds the arc was set back (an open set going round again); the
    /// start time stays what it was.
    pub arc_back: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SetEntry {
    /// Seconds into the set.
    pub at: f64,
    pub path: String,
    pub title: String,
    pub artist: String,
    pub bpm: f64,
    pub key: String,
    /// How it came in: the transition's technique ("opener" for the first).
    pub style: String,
    pub reasons: Vec<String>,
    /// The track it was mixed out of, and where in it (seconds).
    pub from: String,
    pub mix_out: f64,
    /// Where in this track it became audible, seconds (the play-length
    /// setting counts from here; 0 for an opener).
    pub mix_in: f64,
    /// Where the set was going: build, hold or release.
    pub direction: String,
}

/// Every AI DJ set is kept: its tracks in order and how each came in (not
/// the audio), one file per set in `sets/` under the state directory,
/// rewritten as it goes, so a set that ends any way is there.
pub fn archive_set(set: &Set, ended: Option<u64>) {
    if set.started == 0 {
        return;
    }
    // One writer at a time: callers no longer all hold the session lock,
    // and two writes interleaving on the same temporary file could rename
    // a torn one. A leaf lock: nothing is taken under it.
    static ARCHIVE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = ARCHIVE.lock().unwrap_or_else(|e| e.into_inner());
    let dir = state_dir().join("sets");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let identity = if set.id.is_empty() {
        set.started.to_string()
    } else {
        set.id.clone()
    };
    let path = dir.join(format!("{identity}.json"));
    let prior_end = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|v| v["ended"].as_u64());
    let ended = if set.ended {
        prior_end.or(ended)
    } else {
        ended
    };
    let mut v = serde_json::to_value(set).unwrap_or_default();
    v["ended"] = serde_json::json!(ended);
    v["tracks"] = serde_json::json!(set.log.len());
    if let Ok(text) = serde_json::to_string_pretty(&v) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }
}

impl Set {
    /// Change only pacing. Keep the set identity, collection, arc and log.
    pub fn apply_fast_mode(&mut self, set_id: &str, on: bool, anchor: Option<(String, f64)>) -> Result<bool, String> {
        if self.id != set_id {
            return Ok(false); // An old request cannot change a replacement set.
        }
        if !self.on || self.ended {
            return Err("Start a set before changing Fast mode".into());
        }
        let bars = on.then_some(8);
        if self.test_bars == bars {
            return Ok(false);
        }
        self.test_bars = bars;
        if on && self.test_since.is_none() {
            self.test_since = anchor;
        }
        Ok(true)
    }

    /// Where in `path` it became audible (seconds), as the set logged it
    /// when it came in; None for an opener, a track put on by hand or an
    /// older set (the play-length setting then counts from its start).
    pub fn entered_at(&self, path: &str) -> Option<f64> {
        self.on
            .then(|| self.log.iter().rev().find(|e| e.path == path))
            .flatten()
            .map(|e| e.mix_in)
            .filter(|at| *at > 0.0)
    }

    pub fn elapsed(&self, now: u64) -> u64 {
        let until = if self.paused { self.paused_at } else { now };
        until
            .saturating_sub(self.started)
            .saturating_sub(self.paused_seconds)
    }
    pub fn source_collections(&self) -> Vec<i64> {
        if self.collections.is_empty() {
            self.collection.into_iter().collect()
        } else {
            self.collections.clone()
        }
    }
    /// Where in the arc the set is (0 … 1).
    pub fn phase(&self, now: u64) -> f64 {
        if self.minutes <= 0.0 {
            return 0.0;
        }
        (self.elapsed(now).saturating_sub(self.arc_back) as f64 / 60.0 / self.minutes)
            .clamp(0.0, 1.0)
    }

    /// The arc's section name and its direction at `phase`.
    pub fn section(&self, phase: f64) -> (&'static str, patch_core::intent::Direction) {
        use patch_core::intent::Direction::*;
        match self.arc.as_str() {
            "peak" => ("peak", if phase < 0.9 { Hold } else { Release }),
            "warmup" => (
                if phase < 0.8 { "warm-up" } else { "handover" },
                if phase < 0.8 { Hold } else { Build },
            ),
            _ => match phase {
                p if p < 0.15 => ("warm-up", Hold),
                p if p < 0.6 => ("build", Build),
                p if p < 0.85 => ("peak", Hold),
                _ => ("release", Release),
            },
        }
    }

    /// The direction for the next pick: the DJ's steer, else the arc.
    pub fn direction(&self, now: u64) -> patch_core::intent::Direction {
        match self.steer {
            Some((d, n)) if n > 0 => d,
            _ => self.section(self.phase(now)).1,
        }
    }
}

pub fn now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Teach the planner from the DJ's reports (`reports/*.json`): the moves
/// of transitions reported bad are picked less (patch_ai `learned_penalty`).
/// Returns how many reports counted. The engine calls it at start and after
/// each report; a few dozen small files, read off the audio thread.
pub fn learn_from_reports() -> usize {
    let now = now_s() as f64;
    let reports: Vec<patch_ai::mix::Reported> = std::fs::read_dir(state_dir().join("reports"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read(e.path()).ok())
        .filter_map(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .map(|v| patch_ai::mix::Reported {
            age_days: (now - v["reported"].as_f64().unwrap_or(0.0)) / 86_400.0,
            why: v["why"].as_str().unwrap_or("").to_string(),
            technique: v["transition"]["technique"].as_str().unwrap_or("").to_string(),
        })
        .collect();
    patch_ai::mix::set_learned(&reports)
}

pub fn state_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join(".local/state")
        })
        .join("omarchy-patch")
}

pub fn path() -> std::path::PathBuf {
    // Tests keep their own: reading the DJ's real session pushed its style
    // into the planner's process-wide setting while planner tests ran in
    // parallel (a flaky `cache_eviction_keeps_the_same_long_layered_plan`),
    // and a test's save would have overwritten it.
    if cfg!(test) {
        return std::env::temp_dir().join(format!("patch-test-dj-session-{}.json", std::process::id()));
    }
    state_dir().join("dj-session.json")
}

impl Session {
    /// Change the transition style (see `patch_core::mix_style`); whether
    /// anything changed. A bad request changes nothing.
    pub fn change_mix_style(&mut self, req: &serde_json::Value) -> Result<bool, String> {
        let before = self.mix_style;
        self.mix_style.apply(req)?;
        Ok(self.mix_style != before)
    }

    /// The saved session; else the faders the UI used to keep (migrated
    /// once, then the service owns them); else defaults.
    pub fn load() -> Session {
        if let Some(mut s) = std::fs::read_to_string(path())
            .ok()
            .and_then(|t| serde_json::from_str::<Session>(&t).ok())
        {
            // Sessions from before AI modes: auto-mix on means Act.
            if s.automix && s.ai_mode == patch_core::aivis::Mode::Observe {
                s.ai_mode = patch_core::aivis::Mode::Act;
            }
            set_levels(s.levels);
            patch_ai::mix::set_mix_style(s.mix_style);
            return s;
        }
        let mut s = Session::default();
        // The UI used to keep the EQ knobs too.
        if let Ok(t) = std::fs::read_to_string(patch_core::ipc::runtime_dir().join("dj-eq")) {
            let rows: Vec<Vec<f32>> = t
                .lines()
                .map(|l| {
                    l.split_whitespace()
                        .filter_map(|w| w.parse().ok())
                        .collect()
                })
                .collect();
            if rows.len() >= 2 && rows[0].len() == 4 && rows[1].len() == 4 {
                for d in 0..2 {
                    for k in 0..4 {
                        s.eq[d][k] = rows[d][k].clamp(-1.0, 1.0);
                    }
                }
            }
        }
        let legacy = patch_core::ipc::runtime_dir().join("dj-faders");
        if let Ok(t) = std::fs::read_to_string(legacy) {
            let v: Vec<f32> = t
                .split_whitespace()
                .filter_map(|w| w.parse().ok())
                .collect();
            if v.len() == 3 {
                s.mixer.faders = [v[0].clamp(0.0, 1.0), v[1].clamp(0.0, 1.0)];
                s.mixer.xfader = v[2].clamp(0.0, 1.0);
            }
        }
        s
    }

    /// Persist the session. Callers hold the session lock (the beat syncer
    /// and the AI planner wait on it), and a crossfader move saves: only
    /// the text is made here; one writer thread puts the latest on disk.
    pub fn save(&self) {
        let Ok(text) = serde_json::to_string_pretty(self) else {
            return;
        };
        if cfg!(test) {
            let _ = write_file(&text);
            return;
        }
        SAVES.post(text);
    }
}

/// Saves on their way to disk: only the newest is kept (a burst of moves
/// writes once), one thread writes it, and a failed write is retried with
/// a growing pause and reported by [`save_error`] until one succeeds.
struct Saves {
    state: std::sync::Mutex<SaveState>,
    changed: std::sync::Condvar,
    write: fn(&str) -> Result<(), String>,
}

#[derive(Default)]
struct SaveState {
    /// The newest text not yet on disk.
    pending: Option<String>,
    /// Being written now (taken from `pending`).
    writing: bool,
    error: Option<String>,
    started: bool,
}

static SAVES: Saves = Saves {
    state: std::sync::Mutex::new(SaveState {
        pending: None,
        writing: false,
        error: None,
        started: false,
    }),
    changed: std::sync::Condvar::new(),
    write: write_file,
};

impl Saves {
    fn post(&'static self, text: String) {
        let mut st = self.state.lock().unwrap();
        st.pending = Some(text);
        if !st.started {
            st.started = std::thread::Builder::new()
                .name("patch-session-save".into())
                .spawn(move || self.run())
                .is_ok();
        }
        if !st.started {
            // No writer thread: written here, as the last resort.
            let text = st.pending.take().unwrap_or_default();
            st.error = (self.write)(&text).err();
            return;
        }
        self.changed.notify_all();
    }

    fn run(&self) {
        let mut retry = std::time::Duration::from_millis(250);
        let mut st = self.state.lock().unwrap();
        loop {
            while st.pending.is_none() {
                st = self.changed.wait(st).unwrap();
            }
            let text = st.pending.take().unwrap();
            st.writing = true;
            drop(st);
            let written = (self.write)(&text);
            st = self.state.lock().unwrap();
            st.writing = false;
            match written {
                Ok(()) => {
                    st.error = None;
                    retry = std::time::Duration::from_millis(250);
                }
                Err(e) => {
                    eprintln!("session: not saved: {e}");
                    st.error = Some(e);
                    // Retried unless a newer save came meanwhile.
                    st.pending.get_or_insert(text);
                    st = self.changed.wait_timeout(st, retry).unwrap().0;
                    retry = (retry * 2).min(std::time::Duration::from_secs(30));
                }
            }
            self.changed.notify_all();
        }
    }
}

/// Why the session could not be saved, while that lasts.
pub fn save_error() -> Option<String> {
    SAVES.state.lock().unwrap().error.clone()
}

/// Wait (at most `limit`) until every save so far is on disk. Err: what is
/// still unsaved, and why.
pub fn flush(limit: std::time::Duration) -> Result<(), String> {
    SAVES.flush(limit)
}

impl Saves {
    fn flush(&self, limit: std::time::Duration) -> Result<(), String> {
        let deadline = std::time::Instant::now() + limit;
        let mut st = self.state.lock().unwrap();
        while st.pending.is_some() || st.writing {
            let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) else {
                return Err(st
                    .error
                    .clone()
                    .unwrap_or_else(|| "the session save is still being written".into()));
            };
            st = self.changed.wait_timeout(st, left).unwrap().0;
        }
        Ok(())
    }
}

/// The session file, replaced whole: written aside under a name of this
/// process's own (two engines meet during a handover), then renamed.
fn write_file(text: &str) -> Result<(), String> {
    let p = path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let tmp = p.with_extension(format!("{}.tmp", std::process::id()));
    let written = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, &p));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.map_err(|e| e.to_string())
}

/// Auto-gain trim for a track (dB), from its cached analysis. Match loud
/// tracks to the LUFS target; for quiet tracks, only lift as far as measured
/// true-peak headroom allows. The deck and master guards handle sample peaks.
pub fn trim_for(path: &str) -> f32 {
    if path.is_empty() {
        return 0.0;
    }
    patch_analysis::cached(std::path::Path::new(path))
        .map(|a| trim_of(&a, levels().lufs()))
        .unwrap_or(0.0)
}

pub const PEAK_CEILING_DBTP: f64 = -2.5;

fn trim_of(a: &patch_analysis::Analysis, target: f64) -> f32 {
    if !a.lufs.is_finite() || a.lufs < -70.0 {
        return 0.0;
    }
    let loudness_match = target - a.lufs;
    if loudness_match < 0.0 {
        loudness_match as f32
    } else {
        a.match_gain(target, PEAK_CEILING_DBTP, 6.0).max(0.0) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_two_deck_sessions_keep_their_mix_and_close_the_new_decks() {
        let session: Session = serde_json::from_str(r#"{"mixer":{"faders":[0.7,0.4],"xfader":0.25},"eq":[[0.1,0.2,0.3,0.4],[0,0,0,0]]}"#).unwrap();
        assert_eq!(session.mixer.faders4(), [0.7, 0.4, 0.0, 0.0]);
        assert_eq!(session.mixer.gains4_db()[..2], session.mixer.gains_db());
        assert_eq!(session.mixer.gains4_db()[2..], [-120.0; 2]);
        assert_eq!(session.eq[0], [0.1, 0.2, 0.3, 0.4]);
        assert_eq!(session.extra_eq, [[0.0; 4]; 2]);
        assert_eq!(session.input_trim_db, [0.0; 4]);
    }

    #[test]
    fn all_four_deck_controls_survive_a_session_round_trip() {
        let mut session = Session::default();
        session.mixer.extra_faders = [0.65, 0.83];
        session.input_trim_db = [-6.0, 3.0, 12.0, -24.0];
        session.extra_eq = [[-1.0, 0.2, -0.4, 0.7], [0.3, -0.6, 0.1, -0.8]];
        let restored: Session = serde_json::from_str(&serde_json::to_string(&session).unwrap()).unwrap();
        assert_eq!(session, restored);
        let mut mixer = restored.mixer;
        mixer.extra_faders = [UNITY; 2];
        mixer.auto_gain = true;
        mixer.extra_trims_db = [2.0, -3.0];
        mixer.xfader = 0.0;
        assert!((mixer.gains4_db()[2] - 2.0).abs() < 1e-4);
        assert_eq!(mixer.gains4_db()[3], -120.0);
        mixer.xfader = 1.0;
        assert_eq!(mixer.gains4_db()[2], -120.0);
        assert!((mixer.gains4_db()[3] + 3.0).abs() < 1e-4);
    }

    #[test]
    fn the_default_assign_and_curve_keep_the_tested_mix() {
        // Old sessions (no assign, no curve) load as A/B on the Mix law.
        let old: Session = serde_json::from_str(r#"{"mixer":{"faders":[0.7,0.4],"xfader":0.3}}"#).unwrap();
        assert_eq!(old.mixer.assign, [XfAssign::A, XfAssign::B, XfAssign::A, XfAssign::B]);
        assert_eq!(old.mixer.curve, XfCurve::Mix);
        assert!(!old.mixer.ai_on_faders());
        for i in 0..=20 {
            let x = i as f32 / 20.0;
            assert_eq!(XfCurve::Mix.gains(x), crossfade(x));
        }
    }

    #[test]
    fn thru_ignores_the_crossfader_and_the_curves_close_their_side() {
        let mut m = Mixer { auto_gain: false, extra_faders: [UNITY; 2], ..Mixer::default() };
        m.assign = [XfAssign::Thru, XfAssign::B, XfAssign::Thru, XfAssign::A];
        m.xfader = 1.0;
        let g = m.gains4_db();
        assert!(g[0].abs() < 1e-4, "A on THRU at unity: {g:?}");
        assert!(g[1].abs() < 1e-4);
        assert!(g[2].abs() < 1e-4);
        assert_eq!(g[3], -120.0, "D on the A side, crossfader at B");
        for c in XfCurve::ALL {
            assert_eq!(c.gains(0.0), (1.0, 0.0), "{c:?}");
            assert_eq!(c.gains(1.0), (0.0, 1.0), "{c:?}");
        }
        let (a, b) = XfCurve::Smooth.gains(0.5);
        let equal_power = std::f32::consts::FRAC_1_SQRT_2;
        assert!((a - equal_power).abs() < 1e-3 && (b - equal_power).abs() < 1e-3);
        assert_eq!(XfCurve::Cut.gains(0.1), (1.0, 1.0));
    }

    #[test]
    fn the_ai_on_the_faders_gives_the_gains_its_crossfader_would() {
        let base = Mixer { auto_gain: false, ..Mixer::default() };
        for &(x, fa, fb) in &[(0.5, UNITY, UNITY), (0.8, UNITY, 1.0), (0.25, 0.7, UNITY), (1.0, UNITY, UNITY), (0.0, 1.0, 0.6)] {
            let ai = AiControls { xfader: x, faders: [fa, fb] };
            let mut on_xf = base.clone();
            on_xf.apply_ai(ai);
            let mut thru = base.clone();
            thru.assign[0] = XfAssign::Thru;
            thru.assign[1] = XfAssign::Thru;
            thru.xfader = 0.13;
            thru.apply_ai(ai);
            assert_eq!(thru.xfader, 0.13, "the crossfader is the DJ's");
            let (want, got) = (on_xf.gains_db(), thru.gains_db());
            for d in 0..2 {
                // Below −60 dB the fader closes (the gain is silence then).
                if want[d] > -59.0 {
                    assert!((want[d] - got[d]).abs() < 1e-3, "x {x}: {want:?} vs {got:?}");
                } else {
                    assert!(got[d] <= -59.0, "x {x}: {want:?} vs {got:?}");
                }
            }
        }
    }

    #[test]
    fn a_failed_session_save_is_reported_retried_and_flushed() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;
        static FAILS_LEFT: AtomicUsize = AtomicUsize::new(2);
        static LAST: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
        fn disk(text: &str) -> Result<(), String> {
            if FAILS_LEFT
                .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
                .is_ok()
            {
                return Err("No space left on device".into());
            }
            *LAST.lock().unwrap() = text.to_string();
            Ok(())
        }
        let saves: &'static Saves = Box::leak(Box::new(Saves {
            state: std::sync::Mutex::new(SaveState::default()),
            changed: std::sync::Condvar::new(),
            write: disk,
        }));
        saves.post("first".into());
        // Failing: still unsaved after a short wait, and why is known.
        let err = saves.flush(Duration::from_millis(100)).unwrap_err();
        assert!(err.contains("No space"), "{err}");
        // A newer save replaces the failed one; retries then succeed.
        saves.post("second".into());
        saves.flush(Duration::from_secs(5)).unwrap();
        assert_eq!(*LAST.lock().unwrap(), "second");
        assert!(saves.state.lock().unwrap().error.is_none());
    }

    #[test]
    fn auto_gain_matches_loudness_and_limits_boost_to_peak_headroom() {
        let mut a = patch_analysis::Analysis {
            version: 0,
            analyzers: patch_analysis::AnalyzerVersions::CURRENT,
            human_grid: None,
            source: None,
            duration: 1.0,
            bpm: None,
            beat_offset: 0.0,
            bpm_confidence: 0.0,
            key: None,
            key_confidence: 0.0,
            lufs: -14.0,
            lra: 0.0,
            true_peak: -6.0,
            waveform: vec![],
            downbeat: None,
            downbeat_confidence: 0.0,
            energy: vec![],
            key_source: Default::default(),
            key_detected: None,
            key_detected_confidence: 0.0,
            tag_bpm: None,
            grid: None,
            rhythm: None,
            deep_regions: vec![],
            structure: None,
            harmony: None,
            vocals: None,
        };
        assert!(
            (trim_of(&a, Levels::Balanced.lufs()) - 3.0).abs() < 1e-6,
            "quiet: up to its headroom"
        );
        a.lufs = -7.0;
        a.true_peak = -3.0;
        assert!(
            (trim_of(&a, Levels::Balanced.lufs()) + 4.0).abs() < 1e-6,
            "loud track matched by LUFS"
        );
        a.lufs = -10.5;
        a.true_peak = 0.8;
        assert!(
            (trim_of(&a, Levels::Balanced.lufs()) + 0.5).abs() < 1e-6,
            "a track half a LU above target gets a half dB trim"
        );
        a.lufs = -7.0;
        a.true_peak = 7.0;
        assert!(
            (trim_of(&a, Levels::Balanced.lufs()) + 4.0).abs() < 1e-6,
            "very hot peaks do not override the loudness match"
        );
        a.lufs = -7.0;
        a.true_peak = -3.0;
        assert!((trim_of(&a, Levels::Loud.lufs()) + 2.0).abs() < 1e-6, "loud: −9 LUFS");
        assert!((trim_of(&a, Levels::Streaming.lufs()) + 7.0).abs() < 1e-6, "streaming: −14 LUFS");
        assert_eq!(Levels::parse("streaming"), Some(Levels::Streaming));
        let back: Session = serde_json::from_str(r#"{"levels":"loud"}"#).unwrap();
        assert_eq!(back.levels, Levels::Loud);
        assert_eq!(Session::default().levels, Levels::Balanced);
    }

    #[test]
    fn laws() {
        assert_eq!(crossfade(0.5), (1.0, 1.0));
        for step in 0..=100 {
            let x = step as f32 / 100.0;
            let (a, b) = crossfade(x);
            assert!((0.0..=1.0).contains(&a) && (0.0..=1.0).contains(&b));
            if x <= 0.5 {
                assert_eq!(a, 1.0, "A stays full through the centre");
            }
            if x >= 0.5 {
                assert_eq!(b, 1.0, "B stays full through the centre");
            }
            let (reverse_a, reverse_b) = crossfade(1.0 - x);
            assert!((a - reverse_b).abs() < 1e-6 && (b - reverse_a).abs() < 1e-6);
        }
        assert_eq!(crossfade(0.0), (1.0, 0.0));
        assert_eq!(crossfade(1.0), (0.0, 1.0));
        assert!((fader_db(UNITY)).abs() < 1e-5, "unity");
        assert!((fader_pos(fader_db(0.3)) - 0.3).abs() < 1e-6);
    }

    #[test]
    fn gains_combine_fader_crossfader_and_trim() {
        let mut m = Mixer {
            faders: [UNITY, UNITY],
            xfader: 0.0,
            auto_gain: true,
            trims_db: [2.5, 0.0],
            ..Mixer::default()
        };
        let g = m.gains_db();
        assert!((g[0] - 2.5).abs() < 1e-4, "A at unity plus its trim: {g:?}");
        assert_eq!(g[1], -120.0, "B cut by the crossfader");
        m.auto_gain = false;
        assert!(m.gains_db()[0].abs() < 1e-4, "trim off");
        m.faders[0] = 0.0;
        assert_eq!(m.gains_db()[0], -120.0, "fader down is silence");
    }

    #[test]
    fn the_transition_style_is_part_of_the_saved_session() {
        use patch_core::mix_style::Style;
        let mut ses = Session::default();
        assert_eq!(ses.mix_style, patch_core::mix_style::MixStyle::default());
        assert_eq!(
            ses.change_mix_style(&serde_json::json!({"style": "adventurous", "play_percent": 40})),
            Ok(true)
        );
        // The same request again changes nothing.
        assert_eq!(
            ses.change_mix_style(&serde_json::json!({"style": "adventurous", "play_percent": 40})),
            Ok(false)
        );
        // A bad request is refused whole.
        let kept = ses.mix_style;
        assert!(ses.change_mix_style(&serde_json::json!({"loops": false, "play_percent": 5})).is_err());
        assert_eq!(ses.mix_style, kept);
        // It survives the session file (restarts, engine upgrades)...
        let text = serde_json::to_string(&ses).unwrap();
        let back: Session = serde_json::from_str(&text).unwrap();
        assert_eq!(back.mix_style.style, Style::Adventurous);
        assert_eq!(back.mix_style.play_percent, 40);
        // ...and a session saved before the setting existed plans as today.
        let old: Session = serde_json::from_str(r#"{"automix": true, "notify_tracks": false}"#).unwrap();
        assert_eq!(old.mix_style, patch_core::mix_style::MixStyle::default());
        assert!(!old.mix_style.any_flourish());
    }

    #[test]
    fn a_track_counts_its_play_length_from_where_it_came_in() {
        let entry = |path: &str, mix_in: f64| SetEntry { path: path.into(), mix_in, ..Default::default() };
        let set = Set {
            on: true,
            log: vec![entry("opener", 0.0), entry("a", 31.5), entry("b", 15.0), entry("a", 47.0)],
            ..Default::default()
        };
        // The newest entry of a track that came in twice wins.
        assert_eq!(set.entered_at("a"), Some(47.0));
        assert_eq!(set.entered_at("b"), Some(15.0));
        // An opener started at 0, an unknown track, a set that is off: from its start.
        assert_eq!(set.entered_at("opener"), None);
        assert_eq!(set.entered_at("stranger"), None);
        let off = Set { on: false, ..set };
        assert_eq!(off.entered_at("a"), None);
    }

    #[test]
    fn fast_mode_toggle_preserves_current_set_sources_and_solo_anchor() {
        let before = Set {
            on: true,
            id: "existing-set".into(),
            collections: vec![17],
            started: 1000,
            minutes: 90.0,
            arc: "peak".into(),
            test_since: Some(("current-track".into(), 32.5)),
            log: vec![SetEntry { path: "current-track".into(), ..Default::default() }],
            ..Default::default()
        };
        let mut set = before.clone();
        assert_eq!(set.apply_fast_mode("existing-set", true, Some(("current-track".into(), 96.0))), Ok(true));
        assert_eq!(set.test_bars, Some(8));
        assert_eq!(set.test_since, before.test_since, "count from the transition, not the button press");
        assert_eq!(set.apply_fast_mode("existing-set", true, None), Ok(false));
        assert_eq!(set.apply_fast_mode("existing-set", false, None), Ok(true));
        assert_eq!(set, before, "identity, sources, arc and history remain unchanged");
    }

    #[test]
    fn fast_mode_requests_cannot_leak_into_a_replacement_set() {
        let mut replacement = Set { on: true, id: "replacement".into(), ..Default::default() };
        let before = replacement.clone();
        assert_eq!(replacement.apply_fast_mode("previous-set", true, None), Ok(false));
        assert_eq!(replacement, before);
    }

    #[test]
    fn fast_mode_needs_an_active_set_and_can_change_while_paused() {
        let mut set = Set::default();
        assert!(set.apply_fast_mode("", true, None).is_err());
        assert_eq!(set, Set::default());
        set.on = true;
        set.paused = true;
        let anchor = Some(("opening-track".into(), 64.0));
        assert_eq!(set.apply_fast_mode("", true, anchor.clone()), Ok(true));
        assert_eq!(set.test_since, anchor);
        assert!(set.paused);
        set.ended = true;
        assert!(set.apply_fast_mode("", false, None).is_err());
    }

    #[test]
    fn the_journey_arc_warms_up_builds_peaks_and_releases() {
        use patch_core::intent::Direction::*;
        let set = Set {
            on: true,
            started: 1_000,
            minutes: 60.0,
            arc: "journey".into(),
            ..Default::default()
        };
        let at = |min: u64| set.direction(1_000 + min * 60);
        assert_eq!(
            (at(2), at(20), at(45), at(58)),
            (Hold, Build, Hold, Release)
        );
        let steered = Set {
            steer: Some((Release, 2)),
            ..set.clone()
        };
        assert_eq!(
            steered.direction(1_000 + 20 * 60),
            Release,
            "the DJ's word beats the arc"
        );
    }

    #[test]
    fn session_round_trips_as_json() {
        let mut s = Session::default();
        s.mixer.xfader = 0.8;
        s.mixer.trims_db = [1.0, 2.0];
        let back: Session = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.mixer.xfader, 0.8);
        assert_eq!(
            back.mixer.trims_db,
            [0.0, 0.0],
            "trims are derived, not stored"
        );
    }
}

#[cfg(test)]
mod workspace_tests {
    use super::*;
    #[test]
    fn legacy_sources_migrate_and_pause_does_not_advance_the_arc() {
        let legacy: Set = serde_json::from_value(
            serde_json::json!({"collection":7,"on":true,"started":100,"minutes":60}),
        )
        .unwrap();
        assert_eq!(legacy.source_collections(), vec![7]);
        let mut set = legacy;
        set.collections = vec![7, 8];
        assert_eq!(set.source_collections(), vec![7, 8]);
        set.paused = true;
        set.paused_at = 160;
        assert_eq!(set.elapsed(1000), 60);
        set.paused = false;
        set.paused_seconds = 840;
        assert_eq!(set.elapsed(1010), 70);
    }
}
