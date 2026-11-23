//! Independent playback transport. All process and socket I/O stays on a
//! bounded worker; closing a surface leaves the detached player running.
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

pub mod carts;
pub mod history;
pub mod marks;
pub mod wave;

#[derive(Clone, Default, serde::Serialize)]
pub struct Snapshot {
    pub path: String,
    pub title: String,
    pub ready: bool,
    pub loading: bool,
    pub idle: bool,
    pub audio: String,
    pub playing: bool,
    pub position: f64,
    pub duration: f64,
    pub volume: f64,
    pub looping: bool,
    pub message: String,
    /// Saved cue point (seconds). Cue jumps here when pressed again.
    pub cue_point: f64,
    /// Pre-Fader Listen is active (audio routed to cue/headphone output).
    pub pfl: bool,
    /// The pre-listen only: where it plays ("main", "cue" or "aux1").
    pub route: String,
    /// The pre-listen on AUX 1, and AUX 1 has an output of its own.
    pub aux_heard: bool,
    /// Played to its end (mpv keeps the last moment open).
    pub ended: bool,
    /// The file's artist tag, when it has one.
    pub artist: String,
    /// Playback speed (1: as recorded); the pitch stays.
    pub speed: f64,
    /// The A-B loop's ends in seconds (negative: not set).
    pub ab_a: f64,
    pub ab_b: f64,
    /// The file's own chapters, in seconds, and their titles.
    pub chapters: Vec<f64>,
    pub chapter_titles: Vec<String>,
    /// Gain on top of the volume, dB: below 0 while a cart ducks it.
    pub gain_db: f64,
    /// The queue: every source in order, and which one plays (-1: none).
    pub queue: Vec<String>,
    pub queue_at: i64,
    /// The whole queue plays round.
    pub repeat_all: bool,
    /// When `position` was read: a view adds the time since while it plays
    /// (snapshots come a poll apart, ~0.6 s).
    #[serde(skip)]
    pub read_at: Option<Instant>,
}

impl Snapshot {
    /// Where it plays now: the position read, plus the time since while it
    /// plays.
    pub fn position_now(&self) -> f64 {
        let since = match (self.playing, self.read_at) {
            (true, Some(at)) => at.elapsed().as_secs_f64(),
            _ => 0.0,
        };
        let pos = self.position + since;
        if self.duration > 0.0 { pos.min(self.duration) } else { pos }
    }
}

pub enum Action {
    Notice(String),
    Poll,
    Load(String),
    Play(bool),
    Quit,
    Toggle,
    Stop,
    Seek(f64),
    /// Jump by seconds from where it plays (back with a negative number).
    Skip(f64),
    Volume(f64),
    Loop(bool),
    /// The carts: start or stop a pad's sound (`carts`).
    Cart(carts::Do),
    /// Jump to the saved cue point, or set one at the current position.
    Cue,
    /// Pre-Fader Listen: route audio to the cue/headphone output.
    Pfl(bool),
    /// Add sources to the end of the queue (the first is made ready when
    /// nothing was loaded).
    Enqueue(Vec<String>),
    /// Play the queue's entry at this place.
    Jump(usize),
    /// The next entry of the queue.
    Next,
    /// Back to the start of this one; from its first seconds, the entry before.
    Previous,
    /// Take an entry out of the queue.
    Remove(usize),
    /// Move an entry one place up or down.
    Move { index: usize, up: bool },
    /// Empty the queue but for what plays.
    ClearQueue,
    /// Put the queue in a new random order.
    Shuffle,
    /// Repeat: 0 off, 1 this one, 2 the whole queue.
    Repeat(u8),
    /// Playback speed, 0.5 to 2 (the pitch stays).
    Speed(f64),
    /// The A-B loop: set A, then B, then clear.
    AbLoop,
    /// The next (1) or the previous (-1) chapter of the file.
    Chapter(i32),
    /// Go to a place, in seconds.
    Goto(f64),
    /// A cart that ducks started (true) or ended (false; the voice that
    /// ended, which no longer counts): turn the transport down under it,
    /// or bring it back when no other ducking cart plays.
    Duck(bool, Option<usize>),
    /// Bring back the queue as it last stood and play this entry, going on
    /// where a long file was left.
    Restore(Vec<String>, usize),
    /// The Library's pre-listen (its own stream, `Player::preview`): open a
    /// track at a moment (seconds) and play it. While a deck or the Player
    /// plays it goes to AUX 1, kept off main; else to the headphones when a
    /// cue output is chosen, else to main.
    Preview(String, f64),
    /// Jump the preview to a moment (seconds) and play.
    PreviewAt(f64),
    /// Stop pre-listening: pause, and give its channel back to main.
    PreviewEnd,
}

#[derive(Default)]
struct Mailbox {
    commands: VecDeque<Action>,
    poll: bool,
    snapshot: Snapshot,
    /// The carts that play now.
    carts: Vec<carts::Playing>,
}
struct Worker {
    wake: mpsc::SyncSender<()>,
    state: Arc<Mutex<Mailbox>>,
}
#[derive(Default)]
pub struct Player {
    worker: RefCell<Option<Worker>>,
    /// The Library's pre-listen: a second mpv ("Patch Preview") with its
    /// own socket and mixer channel, so the Player keeps what it plays.
    preview: bool,
    /// The last snapshot read: what `snapshot` gives while the worker holds
    /// the mailbox.
    last: RefCell<Option<Snapshot>>,
    /// The carts last read, as `last`.
    last_carts: RefCell<Vec<carts::Playing>>,
}

thread_local! {
    /// This worker drives the pre-listen's mpv, not the Player's.
    static PREVIEW: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn is_preview() -> bool {
    PREVIEW.with(|p| p.get())
}

/// The mpv stream's name, which the engine gives a mixer channel.
fn client() -> &'static str {
    if is_preview() { "Patch Preview" } else { "Patch Player" }
}

impl Player {
    pub fn preview() -> Self {
        Self { preview: true, ..Default::default() }
    }

    /// Start a worker only when the Player is actually used. Polls coalesce;
    /// queued slider edits keep the latest value, and transport goes first.
    pub fn send(&self, action: Action) {
        let mut worker = self.worker.borrow_mut();
        if worker.is_none() {
            let state = Arc::new(Mutex::new(Mailbox::default()));
            let shared = state.clone();
            let (wake, rx) = mpsc::sync_channel(1);
            let preview = self.preview;
            if std::thread::Builder::new()
                .name(if preview { "patch-preview" } else { "patch-player-control" }.into())
                .spawn(move || {
                    PREVIEW.with(|p| p.set(preview));
                    let mut route_at: Option<Instant> = None;
                    let mut message = String::new();
                    let mut pending_load: Option<Instant> = None;
                    let mut pfl = false;
                    let mut pfl_at: Option<Instant> = None;
                    let mut keeper = Keeper::default();
                    while rx.recv().is_ok() {
                        loop {
                            let next = {
                                let mut s = shared.lock().unwrap();
                                if let Some(cmd) = s.commands.pop_front() {
                                    s.poll = false;
                                    Some(cmd)
                                } else if s.poll {
                                    s.poll = false;
                                    Some(Action::Poll)
                                } else {
                                    None
                                }
                            };
                            let Some(action) = next else {
                                break;
                            };
                            if matches!(action, Action::Pfl(_)) {
                                pfl_at = None;
                            }
                            if matches!(action, Action::Load(_) | Action::Preview(..) | Action::Restore(..)) {
                                pending_load = Some(Instant::now());
                            }
                            if matches!(action, Action::Stop | Action::Quit) {
                                pending_load = None;
                            }
                            if let Action::Notice(note) = action {
                                message = note;
                            } else if !matches!(action, Action::Poll) {
                                if matches!(action, Action::Load(_) | Action::Restore(..)) {
                                    shared.lock().unwrap().snapshot.message = "Opening audio…".into();
                                }
                                match execute(action) {
                                    // A long file goes on where it was left: say so.
                                    Ok(reply) => {
                                        message = reply["resumed"].as_f64().map_or_else(String::new, |at| {
                                            format!("Going on from {}. Stop starts it over.", crate::dj::clock(at))
                                        });
                                    }
                                    Err(error) => {
                                        message = error;
                                        pending_load = None;
                                    }
                                }
                            }
                            let mut snapshot = read_snapshot().unwrap_or(Snapshot {
                                volume: 100.0,
                                speed: 1.0,
                                queue_at: -1,
                                ..Snapshot::default()
                            });
                            if !preview {
                                keeper.observe(&snapshot);
                            }
                            // The engine owns cue routing, including edits from the Mixer.
                            // Poll off the UI thread, at most twice per second.
                            if preview {
                                // A deck or the Player starting or stopping
                                // moves the pre-listen, twice a second.
                                if snapshot.playing
                                    && route_at.is_none_or(|at| at.elapsed() >= Duration::from_millis(500))
                                {
                                    route_at = Some(Instant::now());
                                    if let Err(e) = route_preview() {
                                        message = e;
                                    }
                                }
                                let r = ROUTE.lock().unwrap();
                                snapshot.route = r.as_ref().map_or("main", |r| r.route.word()).into();
                                snapshot.aux_heard = r.as_ref().is_some_and(|r| r.aux_heard);
                            } else if pfl_at.is_none_or(|at| at.elapsed() >= Duration::from_millis(500)) {
                                pfl = player_pfl(None).unwrap_or(false);
                                pfl_at = Some(Instant::now());
                            }
                            snapshot.pfl = pfl;
                            if let Some(at) = pending_load {
                                if snapshot.ready {
                                    pending_load = None;
                                } else if snapshot.idle && at.elapsed() > Duration::from_millis(300)
                                {
                                    message =
                                        "Could not open audio. Check the file or stream address."
                                            .into();
                                    pending_load = None;
                                }
                            }
                            snapshot.loading |= pending_load.is_some();
                            snapshot.message = if snapshot.loading && message.is_empty() {
                                "Opening audio…".into()
                            } else {
                                message.clone()
                            };
                            // The carts' voices answer for themselves; no
                            // voice, nothing to ask.
                            let playing = if !preview && carts::any() { carts::status() } else { Vec::new() };
                            let mut s = shared.lock().unwrap();
                            s.snapshot = snapshot;
                            s.carts = playing;
                        }
                    }
                })
                .is_err()
            {
                return;
            }
            *worker = Some(Worker { wake, state });
        }
        let worker = worker.as_ref().unwrap();
        // The mailbox is held only for small in-memory updates, never I/O.
        // Preserve transport clicks even when the worker publishes a snapshot.
        let mut s = worker.state.lock().unwrap();
        if matches!(action, Action::Poll) {
            s.poll = true;
        } else {
            let replacement = s.commands.iter().position(|queued| {
                matches!(
                    (queued, &action),
                    (Action::Seek(_), Action::Seek(_))
                        | (Action::PreviewAt(_), Action::PreviewAt(_))
                        | (Action::Volume(_), Action::Volume(_))
                        | (Action::Loop(_), Action::Loop(_))
                        | (Action::Speed(_), Action::Speed(_))
                        | (Action::Cart(carts::Do::Gain { .. }), Action::Cart(carts::Do::Gain { .. }))
                )
            });
            if let Some(i) = replacement {
                s.commands.remove(i);
            }
            if s.commands.len() >= 16 {
                s.snapshot.message = "Player busy; try again shortly.".into();
                return;
            }
            s.commands.push_back(action);
        }
        drop(s);
        let _ = worker.wake.try_send(());
    }
    /// What plays, never waiting on the worker: while it holds the mailbox,
    /// the last snapshot read. (None there made the Library's pre-listen
    /// look stopped for a poll now and then: the strip and HUD flashed off
    /// and the playhead jumped — "the HUD refreshes", 2026-11-13.)
    pub fn snapshot(&self) -> Option<Snapshot> {
        let fresh = self
            .worker
            .borrow()
            .as_ref()?
            .state
            .try_lock()
            .ok()
            .map(|s| s.snapshot.clone());
        match fresh {
            Some(s) => {
                *self.last.borrow_mut() = Some(s.clone());
                Some(s)
            }
            None => self.last.borrow().clone(),
        }
    }

    /// The carts that play, never waiting on the worker (as `snapshot`).
    pub fn carts(&self) -> Vec<carts::Playing> {
        let fresh = self
            .worker
            .borrow()
            .as_ref()
            .and_then(|w| w.state.try_lock().ok().map(|s| s.carts.clone()));
        match fresh {
            Some(playing) => {
                *self.last_carts.borrow_mut() = playing.clone();
                playing
            }
            None => self.last_carts.borrow().clone(),
        }
    }

}

/// Keeps the Player's history and queue files as it plays: a long file's
/// place every `KEEP_EVERY` and whenever it stops, the queue whenever it
/// changes. Runs on the worker; a poll with nothing new writes nothing.
#[derive(Default)]
struct Keeper {
    path: String,
    playing: bool,
    kept_at: Option<Instant>,
    queue: (Vec<String>, i64),
}

const KEEP_EVERY: Duration = Duration::from_secs(20);

impl Keeper {
    fn observe(&mut self, s: &Snapshot) {
        if s.path.is_empty() || !s.ready {
            self.playing = false;
            return;
        }
        let new = s.path != self.path;
        let stopped = self.playing && !s.playing;
        let due = s.playing && self.kept_at.is_none_or(|at| at.elapsed() >= KEEP_EVERY);
        if new || stopped || due {
            let mut history = history::History::load();
            let mut changed = false;
            // A queue entry that came up by itself is a source played too.
            if new && history.entries.first().is_none_or(|e| e.path != s.path) {
                history.opened(&s.path);
                changed = true;
            }
            let position = if s.ended { s.duration } else { s.position };
            changed |= history.note(&s.path, &s.title, position, s.duration);
            if changed {
                history.save();
            }
            self.kept_at = Some(Instant::now());
            self.path = s.path.clone();
        }
        self.playing = s.playing;
        if !s.queue.is_empty() && (self.queue.1 != s.queue_at || self.queue.0 != s.queue) {
            self.queue = (s.queue.clone(), s.queue_at);
            history::Queue { items: s.queue.clone(), current: s.queue_at.max(0) as usize }.save();
        }
    }
}

/// Keep where the file is now (a pause or a quit from the CLI, where no
/// worker watches).
fn keep_place() {
    let Ok(mut ipc) = Ipc::connect(socket()) else { return };
    let mut get = |name: &str| ipc.command(json!(["get_property", name])).unwrap_or(Value::Null);
    let path = get("path").as_str().unwrap_or_default().to_owned();
    let (position, duration) = (get("time-pos").as_f64().unwrap_or(0.0), get("duration").as_f64().unwrap_or(0.0));
    let title = get("media-title").as_str().unwrap_or_default().to_owned();
    if path.is_empty() {
        return;
    }
    let mut history = history::History::load();
    if history.note(&path, &title, position, duration) {
        history.save();
    }
}

/// The gain a duck moves to, one step of the way: straight in dB, which is
/// even to the ear.
fn duck_level(from: f64, to: f64, t: f64) -> f64 {
    from + (to - from) * t.clamp(0.0, 1.0)
}

/// Turn the transport down under a ducking cart, or bring it back: down
/// quickly, back gently. On mpv's `volume-gain`, so the volume the user set
/// is never touched. One ramp at a time (a lock), and the last request
/// decides where it ends.
fn duck(on: bool, ended: Option<usize>) -> Result<Value, String> {
    let dir = patch_core::ipc::runtime_dir();
    let lock = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("player-duck.lock"))
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    // Coming back: only when no other ducking cart still plays.
    let held = !on && carts::status().iter().any(|p| p.duck && Some(p.voice) != ended);
    let target = if on || held { carts::DUCK_DB } else { 0.0 };
    let from = property("volume-gain").and_then(|g| g.as_f64()).ok_or("The Player is not running.")?;
    if (from - target).abs() < 0.1 {
        return Ok(Value::Null);
    }
    let over = Duration::from_millis(if target < from { 220 } else { 900 });
    let since = Instant::now();
    loop {
        let t = since.elapsed().as_secs_f64() / over.as_secs_f64();
        call(json!(["set_property", "volume-gain", duck_level(from, target, t)]))?;
        if t >= 1.0 {
            return Ok(Value::Null);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `~/…` as the shell would give it.
fn expand(path: &str) -> String {
    let path = path.trim();
    match path.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest).to_string_lossy().into_owned(),
        None => path.to_owned(),
    }
}

/// Wait (a little) until the file that was asked for is open.
fn wait_ready() -> bool {
    let since = Instant::now();
    while since.elapsed() < Duration::from_millis(1500) {
        if property("audio-params").is_some_and(|a| a.is_object()) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// Most a queue holds: its whole list is read at every poll.
pub const QUEUE_MOST: usize = 500;

pub fn command(action: Action) -> Result<(), String> {
    execute(action).map(|_| ())
}

/// What plays, without the headphone-cue lookup (which asks the engine):
/// for the bar, which reads it every two seconds.
pub fn snapshot() -> Option<Snapshot> {
    read_snapshot()
}

pub fn status() -> Option<Snapshot> {
    let mut snapshot = read_snapshot()?;
    snapshot.pfl = player_pfl(None).unwrap_or(false);
    Some(snapshot)
}

fn engine(req: Value) -> Result<Value, String> {
    patch_engine::client::call(&req)
}

/// Where the pre-listen plays.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Route {
    /// On main, as any app.
    Main,
    /// In the headphones only: PFL on, muted on main.
    Cue,
    /// On AUX 1 only: a pre-fader send at 0 dB, its fader down on main.
    Aux,
}

impl Route {
    fn word(self) -> &'static str {
        match self {
            Route::Main => "main",
            Route::Cue => "cue",
            Route::Aux => "aux1",
        }
    }
}

struct Routed {
    channel: String,
    route: Route,
    aux_heard: bool,
}

/// The pre-listen's channel and how it is routed now.
static ROUTE: Mutex<Option<Routed>> = Mutex::new(None);

/// Where the pre-listen goes: AUX 1 while a deck or the Player plays (kept
/// off what is heard on main), else the headphones when a cue output is
/// chosen, else main. With AUX 1 going nowhere and headphones there, the
/// headphones.
fn route_for(busy: bool, cue: bool, aux_heard: bool) -> Route {
    match (busy, cue) {
        (true, true) if !aux_heard => Route::Cue,
        (true, _) => Route::Aux,
        (false, true) => Route::Cue,
        (false, false) => Route::Main,
    }
}

/// Whether the Player (not the pre-listen) is playing.
fn player_playing() -> bool {
    let Ok(mut ipc) = Ipc::connect(patch_core::ipc::runtime_dir().join("player.sock")) else {
        return false;
    };
    let mut get = |name: &str| ipc.command(json!(["get_property", name])).unwrap_or(Value::Null);
    get("pause") == false && get("eof-reached") != true && get("path").as_str().is_some_and(|p| !p.is_empty())
}

/// What a route sets on the pre-listen's channel. Every field each time:
/// the channel is the pre-listen's own, and a crash midway heals.
fn route_requests(channel: &str, route: Route) -> Vec<Value> {
    let aux = route == Route::Aux;
    vec![
        json!({"op":"send","channel":channel,"bus":"aux1","db": if aux { json!(0.0) } else { Value::Null },"pre":true}),
        json!({"op":"gain","channel":channel,"db": if aux { -120.0 } else { 0.0 }}),
        json!({"op":"pfl","channel":channel,"on": route == Route::Cue}),
        json!({"op":"mute","channel":channel,"muted": route == Route::Cue}),
    ]
}

/// Route the pre-listen for what plays now (see `route_for`).
fn route_preview() -> Result<(), String> {
    let status = engine(json!({"op":"status"}))?;
    let cue = status["cue_output"].as_str().is_some_and(|c| !c.is_empty());
    let aux = &status["aux_routes"]["aux1"];
    let aux_heard = aux.is_object() && aux["silent"] != true;
    let dj = engine(json!({"op":"dj"})).unwrap_or(Value::Null);
    let decks = dj["decks"].as_array().into_iter().flatten().any(|d| d["playing"] == true);
    let route = route_for(decks || player_playing(), cue, aux_heard);
    let mut current = ROUTE.lock().unwrap();
    if let Some(r) = current.as_mut()
        && r.route == route
    {
        r.aux_heard = aux_heard;
        return Ok(());
    }
    // The channel exists once audio is open; PFL needs one too.
    let channel = match current.as_ref() {
        Some(r) => r.channel.clone(),
        None => {
            let t = Instant::now();
            while property("audio-params").is_none_or(|a| !a.is_object())
                && t.elapsed() < Duration::from_millis(1500)
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            match preview_channel() {
                Some(c) => c,
                // Not through Patch: only main, never over a deck there.
                None if route == Route::Main => return Ok(()),
                None => {
                    return Err("Something plays on main and the pre-listen can't leave it: make Patch the system output (Settings) to pre-listen on AUX 1.".into());
                }
            }
        }
    };
    for req in route_requests(&channel, route) {
        engine(req)?;
    }
    *current = Some(Routed { channel, route, aux_heard });
    Ok(())
}

/// The pre-listen's mixer channel, given one when it has none.
fn preview_channel() -> Option<String> {
    let assigned = |e: Value| e["assigned"][client()].as_str().map(str::to_owned);
    if let Some(c) = engine(json!({"op":"eq_apps"})).ok().and_then(assigned) {
        return Some(c);
    }
    let command = patch_core::command::Command::EqApp { app: client().into(), on: true };
    engine(json!({"op":"command","origin":"user","command":command})).ok()?;
    engine(json!({"op":"eq_apps"})).ok().and_then(assigned)
}

/// The pre-listen's channel back on main as any app.
fn preview_restore() {
    if let Some(r) = ROUTE.lock().unwrap().take() {
        for req in route_requests(&r.channel, Route::Main) {
            let _ = engine(req);
        }
    }
}

fn preview(path: &str, at: f64) -> Result<Value, String> {
    validate_source(path)?;
    ensure()?;
    call(json!(["set_property", "pause", true]))?;
    let at = if at.is_finite() { at.max(0.0) } else { 0.0 };
    call(json!(["loadfile", path, "replace", -1, format!("start={at:.3}")]))?;
    route_preview()?;
    call(json!(["set_property", "pause", false]))
}

/// End the pre-listen now, on this thread (the window process is
/// quitting: its worker may not run again).
pub fn end_preview_now() {
    PREVIEW.with(|p| p.set(true));
    if property("idle-active").is_some() {
        let _ = execute(Action::PreviewEnd);
    }
    PREVIEW.with(|p| p.set(false));
}

fn execute(action: Action) -> Result<Value, String> {
    match action {
        Action::Preview(path, at) => preview(&path, at),
        Action::PreviewAt(at) if at.is_finite() => {
            call(json!(["seek", at.max(0.0), "absolute+exact"]))?;
            call(json!(["set_property", "pause", false]))
        }
        Action::PreviewEnd => {
            let r = call(json!(["set_property", "pause", true]));
            preview_restore();
            r
        }
        Action::Notice(_) | Action::Poll => Ok(Value::Null),
        Action::Load(path) => {
            let path = expand(&path);
            validate_source(&path)?;
            ensure()?;
            call(json!(["set_property", "user-data/patch-cue", Value::Null]))?;
            let mut history = history::History::load();
            let resume = history.resume(&path);
            history.opened(&path);
            history.save();
            // A queue that was built stays: this plays now, the queue after
            // it. With no queue it simply replaces what was loaded.
            let queued = property("playlist-count").and_then(|n| n.as_u64()).unwrap_or(0) > 1;
            let how = if queued { "insert-next-play" } else { "replace" };
            match resume {
                Some(at) => call(json!(["loadfile", path, how, -1, format!("start={at:.3}")]))?,
                None => call(json!(["loadfile", path, how]))?,
            };
            call(json!(["set_property", "pause", false]))?;
            Ok(json!({"resumed": resume}))
        }
        Action::Enqueue(paths) => {
            let paths: Vec<String> = paths.iter().map(|p| expand(p)).filter(|p| validate_source(p).is_ok()).collect();
            if paths.is_empty() {
                return Err("Nothing to add: choose audio files or HTTP(S) addresses.".into());
            }
            ensure()?;
            let held = property("playlist-count").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
            if held + paths.len() > QUEUE_MOST {
                return Err(format!("The queue holds {QUEUE_MOST} at most."));
            }
            if held == 0 {
                // Nothing was loaded: the first is made ready, not played.
                call(json!(["set_property", "pause", true]))?;
            }
            for path in &paths {
                call(json!(["loadfile", path, "append"]))?;
            }
            if held == 0 {
                call(json!(["set_property", "playlist-pos", 0]))?;
            }
            Ok(Value::Null)
        }
        Action::Restore(items, current) => {
            let items: Vec<String> = items.into_iter().filter(|p| validate_source(p).is_ok()).take(QUEUE_MOST).collect();
            if items.is_empty() {
                return Err("The queue's files are no longer there.".into());
            }
            ensure()?;
            call(json!(["set_property", "pause", true]))?;
            call(json!(["stop"]))?;
            call(json!(["playlist-clear"]))?;
            for path in &items {
                call(json!(["loadfile", path, "append"]))?;
            }
            let current = current.min(items.len() - 1);
            call(json!(["set_property", "playlist-pos", current]))?;
            let resume = history::History::load().resume(&items[current]);
            if let Some(at) = resume
                && wait_ready()
            {
                call(json!(["seek", at, "absolute+exact"]))?;
            }
            call(json!(["set_property", "pause", false]))?;
            Ok(json!({"resumed": resume}))
        }
        Action::Jump(index) => {
            call(json!(["playlist-play-index", index]))?;
            call(json!(["set_property", "pause", false]))
        }
        Action::Next => call(json!(["playlist-next", "weak"])),
        Action::Previous => {
            // As a CD player: first back to the start, then the one before.
            if property("time-pos").and_then(|t| t.as_f64()).unwrap_or(0.0) > 3.0
                || property("playlist-pos").and_then(|p| p.as_i64()).unwrap_or(0) <= 0
            {
                call(json!(["seek", 0, "absolute+exact"]))
            } else {
                call(json!(["playlist-prev", "weak"]))
            }
        }
        Action::Remove(index) => call(json!(["playlist-remove", index])),
        // mpv moves an entry to *before* the target index.
        Action::Move { index, up: true } if index > 0 => call(json!(["playlist-move", index, index - 1])),
        Action::Move { index, up: false } => call(json!(["playlist-move", index, index + 2])),
        Action::Move { .. } => Ok(Value::Null),
        Action::ClearQueue => call(json!(["playlist-clear"])),
        Action::Shuffle => call(json!(["playlist-shuffle"])),
        Action::Repeat(mode) => {
            call(json!(["set_property", "loop-file", if mode == 1 { "inf" } else { "no" }]))?;
            call(json!(["set_property", "loop-playlist", if mode == 2 { "inf" } else { "no" }]))
        }
        Action::Speed(speed) if speed.is_finite() => call(json!(["set_property", "speed", speed.clamp(0.5, 2.0)])),
        Action::AbLoop => call(json!(["ab-loop"])),
        Action::Chapter(step) => call(json!(["add", "chapter", step])),
        Action::Goto(seconds) if seconds.is_finite() => call(json!(["seek", seconds.max(0.0), "absolute+exact"])),
        Action::Duck(on, ended) => duck(on, ended),
        Action::Quit => {
            // The Player's carts end with it.
            if !is_preview() {
                let _ = carts::run(carts::Do::StopAll { fade: false });
                let _ = carts::run(carts::Do::Rest);
                keep_place();
            }
            call(json!(["quit"]))
        }
        Action::Cart(action) => carts::run(action).map(|_| Value::Null),
        Action::Skip(seconds) if seconds.is_finite() => call(json!(["seek", seconds, "relative+exact"])),
        Action::Play(playing) => {
            if playing && property("eof-reached") == Some(Value::Bool(true)) {
                call(json!(["seek", 0, "absolute+exact"]))?;
            }
            let done = call(json!(["set_property", "pause", !playing]));
            if !playing && !is_preview() {
                keep_place();
            }
            done
        }
        Action::Toggle => {
            if property("eof-reached") == Some(Value::Bool(true)) {
                call(json!(["seek", 0, "absolute+exact"]))?;
                call(json!(["set_property", "pause", false]))
            } else {
                let done = call(json!(["cycle", "pause"]));
                if !is_preview() && property("pause") == Some(Value::Bool(true)) {
                    keep_place();
                }
                done
            }
        }
        Action::Stop if property("duration").and_then(|v| v.as_f64()).unwrap_or(0.0) <= 0.0 => {
            call(json!(["stop"]))
        }
        Action::Stop => {
            call(json!(["set_property", "pause", true]))?;
            // Stop is "from the start next time", for a long file too.
            if !is_preview()
                && let Some(path) = property("path").as_ref().and_then(|p| p.as_str())
            {
                let mut history = history::History::load();
                if history.forget_place(path) {
                    history.save();
                }
            }
            call(json!(["seek", 0, "absolute+exact"]))
        }
        Action::Seek(v) if v.is_finite() => call(json!([
            "seek",
            v.clamp(0.0, 100.0),
            "absolute-percent+exact"
        ])),
        Action::Volume(v) if v.is_finite() => {
            call(json!(["set_property", "volume", v.clamp(0.0, 100.0)]))
        }
        Action::Loop(v) => call(json!([
            "set_property",
            "loop-file",
            if v { "inf" } else { "no" }
        ])),
        Action::Cue => cue(&mut Ipc::connect(socket())?),
        Action::Pfl(on) => {
            if on && property("audio-params").is_none_or(|audio| !audio.is_object()) {
                return Err("Open audio before using Player PFL.".into());
            }
            player_pfl(Some(on)).map(|_| Value::Null)
        }
        _ => Err("Invalid transport value.".into()),
    }
}

/// mpv owns this data so every UI/CLI sees the same cue while it lives.
fn saved_cue(value: &Value, path: &str) -> Option<f64> {
    if value["path"].as_str() != Some(path) {
        return None;
    }
    value["seconds"].as_f64().filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
}

fn cue(ipc: &mut Ipc) -> Result<Value, String> {
    let path = ipc.command(json!(["get_property", "path"]))?;
    let path = path.as_str().filter(|p| !p.is_empty()).ok_or("Open a file before setting a cue.")?;
    let pos = ipc.command(json!(["get_property", "time-pos"]))?
        .as_f64().filter(|p| p.is_finite() && *p >= 0.0)
        .ok_or("Player has no cueable position.")?;
    let saved = ipc.command(json!(["get_property", "user-data/patch-cue"]))
        .unwrap_or(Value::Null);
    ipc.command(json!(["set_property", "pause", true]))?;
    match saved_cue(&saved, path) {
        Some(seconds) if (pos - seconds).abs() < 0.5 => {
            ipc.command(json!(["seek", 0, "absolute+exact"]))?;
            ipc.command(json!(["set_property", "user-data/patch-cue", Value::Null]))
        }
        Some(seconds) => ipc.command(json!(["seek", seconds, "absolute+exact"])),
        None => ipc.command(json!(["set_property", "user-data/patch-cue", {"path":path,"seconds":pos}])),
    }
}

fn player_pfl(on: Option<bool>) -> Result<bool, String> {
    player_pfl_with(on, &mut patch_engine::client::call)
}

/// Use the same application channel and cue bus as the Mixer. Never change
/// mpv's volume or create another capture/loopback to imitate headphone routing.
fn player_pfl_with(
    on: Option<bool>,
    engine: &mut impl FnMut(&Value) -> Result<Value, String>,
) -> Result<bool, String> {
    let app = client();
    let assigned = engine(&json!({"op":"eq_apps"}))?;
    let mut channel = assigned["assigned"][app].as_str().map(str::to_owned);
    // Turning off an unassigned player must not allocate a channel.
    if channel.is_none() && on != Some(true) {
        return Ok(false);
    }
    let status = engine(&json!({"op":"status"}))?;
    if on == Some(true) && status["cue_output"].as_str().is_none_or(str::is_empty) {
        return Err("Choose a Headphone Cue output in the Mixer before using Player PFL.".into());
    }
    if channel.is_none() {
        let command = patch_core::command::Command::EqApp { app: app.into(), on: true };
        engine(&json!({"op":"command","origin":"user","command":command}))?;
        let assigned = engine(&json!({"op":"eq_apps"}))?;
        channel = assigned["assigned"][app].as_str().map(str::to_owned);
    }
    let channel = channel.ok_or("Player has no native channel available for headphone cue.")?;
    if let Some(on) = on {
        engine(&json!({"op":"pfl","channel":channel,"on":on}))?;
        return Ok(on);
    }
    Ok(status["channels"].as_array().into_iter().flatten()
        .find(|c| c["name"] == channel).is_some_and(|c| c["pfl"] == true))
}

fn socket() -> PathBuf {
    patch_core::ipc::runtime_dir().join(if is_preview() { "preview.sock" } else { "player.sock" })
}
fn property(name: &str) -> Option<Value> {
    call(json!(["get_property", name])).ok()
}

/// One socket per snapshot, rather than one connection for every property.
fn read_snapshot() -> Option<Snapshot> {
    let mut ipc = Ipc::connect(socket()).ok()?;
    let mut get = |name: &str| {
        ipc.command(json!(["get_property", name]))
            .ok()
            .unwrap_or(Value::Null)
    };
    let path = get("path").as_str().unwrap_or_default().to_owned();
    let pause = get("pause").as_bool().unwrap_or(true);
    let eof = get("eof-reached").as_bool().unwrap_or(false);
    let audio = get("audio-params");
    let ready = audio.is_object();
    let idle = get("idle-active").as_bool().unwrap_or(true);
    let cue_point = saved_cue(&get("user-data/patch-cue"), &path).unwrap_or(0.0);
    let metadata = get("metadata");
    let artist = ["artist", "ARTIST", "Artist", "album_artist"]
        .iter()
        .find_map(|key| metadata[*key].as_str())
        .unwrap_or_default()
        .to_owned();
    let playlist = get("playlist");
    let entries = playlist.as_array().map(Vec::as_slice).unwrap_or_default();
    let chapters = get("chapter-list");
    Some(Snapshot {
        artist,
        speed: get("speed").as_f64().filter(|s| *s > 0.0).unwrap_or(1.0),
        ab_a: get("ab-loop-a").as_f64().unwrap_or(-1.0),
        ab_b: get("ab-loop-b").as_f64().unwrap_or(-1.0),
        chapters: chapters.as_array().into_iter().flatten().filter_map(|c| c["time"].as_f64()).collect(),
        chapter_titles: chapters
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["time"].is_number())
            .map(|c| c["title"].as_str().unwrap_or_default().to_owned())
            .collect(),
        gain_db: get("volume-gain").as_f64().unwrap_or(0.0),
        queue: entries.iter().map(|e| e["filename"].as_str().unwrap_or_default().to_owned()).collect(),
        queue_at: entries.iter().position(|e| e["current"] == true).map_or(-1, |i| i as i64),
        repeat_all: get("loop-playlist") == "inf",
        title: get("media-title").as_str().unwrap_or_default().to_owned(),
        playing: ready && !path.is_empty() && !pause && !eof,
        ended: eof && !path.is_empty(),
        read_at: Some(Instant::now()),
        ready,
        idle,
        loading: !idle && !ready,
        audio: if ready {
            format!(
                "{:.1} kHz · {} channels",
                audio["samplerate"].as_f64().unwrap_or_default() / 1000.0,
                audio["channel-count"].as_u64().unwrap_or_default()
            )
        } else {
            String::new()
        },
        position: get("time-pos").as_f64().unwrap_or(0.0),
        duration: get("duration").as_f64().unwrap_or(0.0),
        volume: get("volume").as_f64().unwrap_or(100.0),
        looping: get("loop-file") == "inf",
        path,
        message: String::new(),
        cue_point,
        pfl: false,
        ..Default::default()
    })
}

fn validate_source(source: &str) -> Result<(), String> {
    if source.starts_with("https://") || source.starts_with("http://") {
        if source
            .split_once("://")
            .is_some_and(|(_, host)| !host.split('/').next().unwrap_or_default().is_empty())
        {
            return Ok(());
        }
    } else if std::path::Path::new(source).is_file() {
        return Ok(());
    }
    Err("Enter an existing audio file or an HTTP(S) stream URL.".into())
}

fn call(command: Value) -> Result<Value, String> {
    request(socket(), command)
}
fn request(socket: PathBuf, command: Value) -> Result<Value, String> {
    Ipc::connect(socket)?.command(command)
}
struct Ipc {
    reader: BufReader<UnixStream>,
    serial: u64,
    deadline: Instant,
}
impl Ipc {
    fn connect(socket: PathBuf) -> Result<Self, String> {
        let stream =
            UnixStream::connect(socket).map_err(|_| "Open a file to start playback.".to_owned())?;
        stream
            .set_write_timeout(Some(Duration::from_millis(300)))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            reader: BufReader::new(stream),
            serial: 0,
            deadline: Instant::now() + Duration::from_millis(300),
        })
    }
    fn command(&mut self, command: Value) -> Result<Value, String> {
        self.serial += 1;
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .ok_or("Player response timed out.")?;
        self.reader
            .get_ref()
            .set_write_timeout(Some(remaining))
            .map_err(|e| e.to_string())?;
        writeln!(
            self.reader.get_mut(),
            "{}",
            json!({"command": command, "request_id": self.serial})
        )
        .map_err(|e| e.to_string())?;
        let deadline = self.deadline;
        for _ in 0..32 {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or("Player response timed out.")?;
            self.reader
                .get_ref()
                .set_read_timeout(Some(remaining))
                .map_err(|e| e.to_string())?;
            let mut line = String::new();
            if self
                .reader
                .read_line(&mut line)
                .map_err(|e| e.to_string())?
                == 0
            {
                break;
            }
            let response: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            if response["request_id"] != self.serial {
                continue;
            }
            return if response["error"] == "success" {
                Ok(response["data"].clone())
            } else {
                Err(format!(
                    "Player: {}",
                    response["error"].as_str().unwrap_or("command failed")
                ))
            };
        }
        Err("Player returned no command response.".into())
    }
}

fn ensure() -> Result<(), String> {
    if property("idle-active").is_some() {
        return Ok(());
    }
    let socket = socket();
    std::fs::create_dir_all(socket.parent().unwrap()).map_err(|e| e.to_string())?;
    let lock = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(socket.with_extension("lock"))
        .map_err(|e| e.to_string())?;
    lock.try_lock()
        .map_err(|e| format!("Player starting: {e}"))?;
    if property("idle-active").is_some() {
        return Ok(());
    }
    let _ = std::fs::remove_file(&socket);
    let result = Command::new("setsid")
        .args([
            "-f",
            "mpv",
            "--no-config",
            "--idle=yes",
            "--no-video",
            "--no-terminal",
            "--really-quiet",
            "--keep-open=yes",
            "--pause",
            "--ao=pipewire",
            if patch_core::setup::load().system_audio {
                "--audio-device=pipewire/patch"
            } else {
                "--audio-device=auto"
            },
            "--volume=100",
            "--replaygain=no",
        ])
        .arg(format!("--audio-client-name={}", client()))
        .arg(format!("--input-ipc-server={}", socket.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    if !result.success() {
        return Err("Could not launch mpv.".into());
    }
    for _ in 0..40 {
        if property("idle-active").is_some() {
            patch_core::ipc::private_socket(&socket);
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err("Player did not start; install mpv with PipeWire support.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn check_cue(position: f64, saved: Value, expected: Vec<Value>) {
        let path = std::env::temp_dir().join(format!(
            "patch-cue-{}-{}.sock", std::process::id(), patch_engine::now_ns()
        ));
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            for (index, (command, data)) in [
                (json!(["get_property", "path"]), json!("track.wav")),
                (json!(["get_property", "time-pos"]), json!(position)),
                (json!(["get_property", "user-data/patch-cue"]), saved),
            ].into_iter().chain(expected.into_iter().map(|v| (v, Value::Null))).enumerate() {
                let serial = index + 1;
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["command"], command);
                writeln!(stream, "{}", json!({"request_id":serial,"error":"success","data":data})).unwrap();
            }
        });
        cue(&mut Ipc::connect(path.clone()).unwrap()).unwrap();
        worker.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_duck_moves_evenly_in_decibels_and_lands_on_its_target() {
        assert_eq!(duck_level(0.0, -12.0, 0.0), 0.0);
        assert_eq!(duck_level(0.0, -12.0, 0.5), -6.0);
        assert_eq!(duck_level(0.0, -12.0, 1.7), -12.0);
        assert_eq!(duck_level(-12.0, 0.0, 1.0), 0.0);
    }

    #[test]
    fn cue_recall_uses_seconds_without_a_hundred_second_cap() {
        check_cue(180.0, json!({"path":"track.wav","seconds":135.0}), vec![
            json!(["set_property", "pause", true]),
            json!(["seek", 135.0, "absolute+exact"]),
        ]);
    }

    #[test]
    fn cue_at_zero_is_recalled_and_a_different_track_gets_its_own_cue() {
        check_cue(10.0, json!({"path":"track.wav","seconds":0.0}), vec![
            json!(["set_property", "pause", true]),
            json!(["seek", 0.0, "absolute+exact"]),
        ]);
        check_cue(145.0, json!({"path":"old.wav","seconds":60.0}), vec![
            json!(["set_property", "pause", true]),
            json!(["set_property", "user-data/patch-cue", {"path":"track.wav","seconds":145.0}]),
        ]);
        check_cue(145.0, json!({"path":"track.wav","seconds":145.0}), vec![
            json!(["set_property", "pause", true]),
            json!(["seek", 0, "absolute+exact"]),
            json!(["set_property", "user-data/patch-cue", Value::Null]),
        ]);
    }

    #[test]
    fn pfl_uses_the_engine_channel_and_never_changes_player_volume() {
        for on in [true, false] {
            let mut requests = Vec::new();
            let result = player_pfl_with(Some(on), &mut |req| {
                requests.push(req.clone());
                Ok(match req["op"].as_str().unwrap() {
                    "eq_apps" => json!({"assigned":{"Patch Player":"app-2"}}),
                    "status" => json!({"cue_output":"headphones"}),
                    "pfl" => json!("ok"),
                    other => panic!("unexpected operation: {other}"),
                })
            }).unwrap();
            assert_eq!(result, on);
            assert_eq!(requests.last().unwrap(), &json!({"op":"pfl","channel":"app-2","on":on}));
        }
        let state = player_pfl_with(None, &mut |req| Ok(match req["op"].as_str().unwrap() {
            "eq_apps" => json!({"assigned":{"Patch Player":"app-2"}}),
            "status" => json!({"channels":[{"name":"app-2","pfl":true}]}),
            other => panic!("readback must not write: {other}"),
        })).unwrap();
        assert!(state, "a fresh UI reads the engine's PFL state");
    }

    #[test]
    fn pfl_requires_headphones_and_only_allocates_for_an_explicit_enable() {
        let mut allocated = false;
        let mut toggled = false;
        let mut engine = |req: &Value| Ok(match req["op"].as_str().unwrap() {
            "eq_apps" if allocated => json!({"assigned":{"Patch Player":"app-3"}}),
            "eq_apps" => json!({"assigned":{}}),
            "status" => json!({"cue_output":"headphones"}),
            "command" => { allocated = true; json!("ok") },
            "pfl" => { toggled = true; json!("ok") },
            other => panic!("unexpected operation: {other}"),
        });
        assert!(!player_pfl_with(None, &mut engine).unwrap());
        assert!(!player_pfl_with(Some(false), &mut engine).unwrap());
        assert!(player_pfl_with(Some(true), &mut engine).unwrap());
        assert!(allocated && toggled);
        let err = player_pfl_with(Some(true), &mut |req| Ok(match req["op"].as_str().unwrap() {
            "eq_apps" => json!({"assigned":{}}),
            "status" => json!({"cue_output":null}),
            other => panic!("missing headphones must not alter the engine: {other}"),
        })).unwrap_err();
        assert!(err.contains("Headphone Cue"));
    }

    #[test]
    fn preview_goes_to_aux_1_while_something_plays() {
        // (busy, cue output, AUX 1 heard) → route
        assert_eq!(route_for(false, false, false), Route::Main);
        assert_eq!(route_for(false, true, true), Route::Cue);
        assert_eq!(route_for(true, false, false), Route::Aux, "AUX 1 even unheard: never over main");
        assert_eq!(route_for(true, false, true), Route::Aux);
        assert_eq!(route_for(true, true, true), Route::Aux);
        assert_eq!(route_for(true, true, false), Route::Cue, "AUX 1 goes nowhere: the headphones");
    }

    #[test]
    fn aux_route_keeps_the_preview_off_main_and_sends_pre_fader() {
        let r = route_requests("app-4", Route::Aux);
        assert!(r.contains(&json!({"op":"send","channel":"app-4","bus":"aux1","db":0.0,"pre":true})));
        assert!(r.contains(&json!({"op":"gain","channel":"app-4","db":-120.0})));
        // A muted channel sends nothing: never muted on AUX 1.
        assert!(r.contains(&json!({"op":"mute","channel":"app-4","muted":false})));
        let main = route_requests("app-4", Route::Main);
        assert!(main.contains(&json!({"op":"send","channel":"app-4","bus":"aux1","db":null,"pre":true})));
        assert!(main.contains(&json!({"op":"gain","channel":"app-4","db":0.0})));
        assert!(main.contains(&json!({"op":"pfl","channel":"app-4","on":false})));
    }

    #[test]
    fn one_connection_shares_a_response_deadline() {
        let path =
            std::env::temp_dir().join(format!("patch-player-deadline-{}.sock", std::process::id()));
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            for serial in 1..=2 {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                std::thread::sleep(Duration::from_millis(180));
                let _ = writeln!(
                    stream,
                    "{}",
                    json!({"request_id":serial,"error":"success","data":serial})
                );
            }
        });
        let mut ipc = Ipc::connect(path.clone()).unwrap();
        assert_eq!(ipc.command(json!(["get_property", "path"])).unwrap(), 1);
        assert!(ipc.command(json!(["get_property", "duration"])).is_err());
        worker.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn slider_and_poll_floods_preserve_transport_and_bound_the_mailbox() {
        let (wake, _rx) = mpsc::sync_channel(1);
        let state = Arc::new(Mutex::new(Mailbox::default()));
        let player = Player {
            worker: RefCell::new(Some(Worker {
                wake,
                state: state.clone(),
            })),
            ..Default::default()
        };
        for i in 0..200 {
            player.send(Action::Volume(i as f64));
            player.send(Action::Poll);
        }
        player.send(Action::Toggle);
        player.send(Action::Stop);
        let mut mailbox = state.lock().unwrap();
        assert!(mailbox.poll);
        assert_eq!(mailbox.commands.len(), 3);
        assert!(matches!(
            mailbox.commands.pop_front(),
            Some(Action::Volume(199.0))
        ));
        assert!(matches!(mailbox.commands.pop_front(), Some(Action::Toggle)));
        assert!(matches!(mailbox.commands.pop_front(), Some(Action::Stop)));
        drop(mailbox);
        for _ in 0..32 {
            player.send(Action::Toggle);
        }
        assert_eq!(state.lock().unwrap().commands.len(), 16);
    }

    #[test]
    fn ipc_skips_events_and_preserves_command_arguments() {
        let path =
            std::env::temp_dir().join(format!("patch-player-test-{}.sock", std::process::id()));
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let req: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(req["command"][1], "file with \"quotes\".wav");
            writeln!(stream, "{}", json!({"event":"idle"})).unwrap();
            writeln!(
                stream,
                "{}",
                json!({"request_id":1,"error":"success","data":42})
            )
            .unwrap();
        });
        assert_eq!(
            request(
                path.clone(),
                json!(["loadfile", "file with \"quotes\".wav", "replace"])
            )
            .unwrap(),
            42
        );
        worker.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn invalid_sources_are_rejected_before_launch() {
        assert!(validate_source("").is_err());
        assert!(validate_source("https://").is_err());
        assert!(validate_source("https://example.test/stream").is_ok());
        assert!(validate_source("/does/not/exist.wav").is_err());
    }
}
