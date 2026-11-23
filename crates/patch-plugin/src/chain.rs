//! The insert chains: the rack's active plugins, in order, at their places
//! in the mix (the master, a channel, a bus), changed while audio plays, in
//! a process of their own.
//!
//! Plugin code never runs in the engine. The engine keeps two small parts:
//!
//! * the graph callback ([`Bridge`]) hands each insert point's block to
//!   the plugin host through shared memory ([`crate::shm`]) and plays the
//!   one processed a block earlier (dry if it is late). A point with no
//!   plugin passes audio straight through, without the latency;
//! * the supervisor (`patch-plugins`) starts the host (`omarchy-patch
//!   plugins host`) when the rack has a plugin on, tells it the rack and
//!   each live parameter, and watches it. If the host dies or stops
//!   answering, the master goes straight through at once, the plugin that
//!   was running is taken out of the chain and the host starts again with
//!   the others.
//!
//! The host runs what used to run here:
//!
//! * the worker (`patch-vst3`, real-time) runs the chain on each block and
//!   fades a plugin in or out over one block when it joins or leaves;
//! * the manager (the host's main thread) loads and tears down plugins
//!   (VST3 wants both on one thread), queues parameter changes and reports
//!   the status.
//!
//! [`ChainControl`] talks to the supervisor from any thread.

use crate::editor::{self, HostOpen, In, Out, Session};
use crate::shm::Mapping;
use crate::{MAX_SLOTS, POINTS, Place, Processor, RackConfig, SavedParameter};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use vst3_host::RtControl;

/// The environment variable that tells a host where the engine's audio is.
pub const HOST_AUDIO: &str = "PATCH_PLUGIN_AUDIO";
/// And where to serve its plugins' editors (unset: it serves none).
pub const HOST_EDITORS: &str = "PATCH_PLUGIN_EDITORS";
/// A host whose audio worker is silent this long while engaged is stuck.
const STUCK: Duration = Duration::from_secs(1);
/// Crashes no plugin can be blamed for, within a minute, before giving up.
const CRASHES: usize = 3;
/// Engine channels a point can be given to (the engine's MAX_CHANNELS).
pub const CHANNELS: usize = 32;

/// Each engine channel's insert point (0: none), written by the
/// supervisor, read by the graph callback.
type ChannelPoints = [AtomicU8; CHANNELS];

/// Levels and cost of one running plugin, written by the worker.
#[derive(Default)]
pub struct SlotMeter {
    /// Peaks (f32 bits), released over about 300 ms.
    input: AtomicU32,
    output: AtomicU32,
    /// Smoothed processing time per block, in ns, and the block length it
    /// was measured on (frames), for a share of the block's time.
    cost_ns: AtomicU64,
    frames: AtomicU32,
    /// Blocks the plugin answered with non-finite samples (replaced by dry).
    faults: AtomicU64,
}

fn load_f32(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

impl SlotMeter {
    fn hold(a: &AtomicU32, block: f32, frames: usize) {
        // ~300 ms to fall 60 dB at 48 kHz.
        let fall = 0.001f32.powf(frames as f32 / 14_400.0);
        let next = block.max(load_f32(a) * fall);
        a.store(next.to_bits(), Ordering::Relaxed);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SlotState {
    Loading,
    Running,
    Failed(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SlotStatus {
    pub id: u32,
    pub name: String,
    pub state: SlotState,
    pub input_peak: f32,
    pub output_peak: f32,
    /// Share of the block's real time the plugin took (0–1).
    pub load: f32,
    pub faults: u64,
}

impl SlotStatus {
    fn new(id: u32, name: &str, state: SlotState) -> Self {
        Self {
            id,
            name: name.to_string(),
            state,
            input_peak: 0.0,
            output_peak: 0.0,
            load: 0.0,
            faults: 0,
        }
    }
}

/// What the engine tells the host, one JSON line each on its stdin.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Message {
    Sync(RackConfig),
    Set { slot: u32, id: u32, value: f64 },
}

/// What the host tells the engine, on its stdout.
#[derive(Serialize, Deserialize)]
struct Report {
    status: Vec<SlotStatus>,
}

/// A linear crossfade over the block from `from` to `to`, written to `to`.
fn crossfade(from: &[f32], to: &mut [f32]) {
    let step = 1.0 / to.len().max(1) as f32;
    for (i, (a, b)) in from.iter().zip(to.iter_mut()).enumerate() {
        let g = (i + 1) as f32 * step;
        *b = *a + (*b - *a) * g;
    }
}

/// The same, written to `from`.
fn crossfade_into_from(from: &mut [f32], to: &[f32]) {
    let step = 1.0 / from.len().max(1) as f32;
    for (i, (a, b)) in from.iter_mut().zip(to).enumerate() {
        let g = (i + 1) as f32 * step;
        *a += (*b - *a) * g;
    }
}

// ---------------------------------------------------------------------------
// The engine's side.

/// One insert point's state on the engine's side.
struct Point {
    dry_left: Box<[f32]>,
    dry_right: Box<[f32]>,
    capture_left: Box<[f32]>,
    capture_right: Box<[f32]>,
    dry_frames: usize,
    /// The block the dry copy is of.
    dry_sequence: u64,
    /// Routed through the host last block.
    on: bool,
}

/// The graph callback's end of the chains. Lock-free, allocation-free and
/// free of plugin code: it only copies to and from shared memory.
pub struct Bridge {
    audio: Arc<Mapping>,
    points: Box<[Point]>,
    channels: Arc<ChannelPoints>,
    /// The points a plugin is wanted at (bit per point), set by the
    /// supervisor once their memory is in: the others are not touched.
    armed: Arc<AtomicU32>,
    /// This block's number (0 means none in the shared memory).
    sequence: u64,
    /// A point handed the host this block's audio.
    submitted: bool,
}

impl Bridge {
    /// Process one insert point's block in place (at most once a block;
    /// [`Bridge::end_block`] after the last). Engaged: submit it without
    /// waiting and play the previous block's result (or the previous dry
    /// block if the host is late). Joining or leaving the host's path
    /// crossfades over one block.
    pub fn process(&mut self, point: usize, left: &mut [f32], right: &mut [f32]) {
        let Some(at) = self.points.get_mut(point) else { return };
        // No plugin wanted here: nothing to do, not even a copy.
        if !at.on && self.armed.load(Ordering::Relaxed) & 1 << point == 0 {
            return;
        }
        let frames = left.len().min(right.len()).min(at.dry_left.len());
        if frames == 0 {
            return;
        }
        let sequence = self.sequence;
        let want = self.audio.engaged_at(point);
        at.capture_left[..frames].copy_from_slice(&left[..frames]);
        at.capture_right[..frames].copy_from_slice(&right[..frames]);
        if !want && !at.on {
            // Straight through; keep the dry block for a click-free join.
            at.dry_left[..frames].copy_from_slice(&left[..frames]);
            at.dry_right[..frames].copy_from_slice(&right[..frames]);
            at.dry_frames = frames;
            at.dry_sequence = sequence;
            return;
        }
        if want {
            self.audio.submit(point, sequence, &left[..frames], &right[..frames]);
            self.submitted = true;
        }
        let wet = sequence > 1
            && self.audio.processed(point, sequence - 1, &mut left[..frames], &mut right[..frames])
                == Some(frames);
        if !wet && at.dry_frames == frames && at.dry_sequence + 1 == sequence {
            left[..frames].copy_from_slice(&at.dry_left[..frames]);
            right[..frames].copy_from_slice(&at.dry_right[..frames]);
        } else if !wet {
            // No block before this one here (the point was not passed):
            // the input as it is.
            left[..frames].copy_from_slice(&at.capture_left[..frames]);
            right[..frames].copy_from_slice(&at.capture_right[..frames]);
        }
        if want && !at.on {
            // Joining: from the direct signal to the delayed one.
            crossfade(&at.capture_left[..frames], &mut left[..frames]);
            crossfade(&at.capture_right[..frames], &mut right[..frames]);
        } else if !want && at.on {
            // Leaving: from the delayed signal back to the direct one.
            crossfade_into_from(&mut left[..frames], &at.capture_left[..frames]);
            crossfade_into_from(&mut right[..frames], &at.capture_right[..frames]);
        }
        at.on = want;
        // The input, for a click-free fallback if the host misses the next
        // block.
        at.dry_left[..frames].copy_from_slice(&at.capture_left[..frames]);
        at.dry_right[..frames].copy_from_slice(&at.capture_right[..frames]);
        at.dry_frames = frames;
        at.dry_sequence = sequence;
    }

    /// Every point is through for this block: the host may take it.
    pub fn end_block(&mut self) {
        if self.submitted {
            self.audio.publish(self.sequence);
            self.submitted = false;
        }
        self.sequence += 1;
    }

    /// The insert point an engine channel runs through, if it has one.
    pub fn channel_point(&self, channel: usize) -> Option<usize> {
        let point = self.channels.get(channel)?.load(Ordering::Relaxed) as usize;
        (point != 0).then_some(point)
    }

    /// Plugins are in the path somewhere (one block of latency there).
    pub fn engaged(&self) -> bool {
        self.points.iter().any(|p| p.on)
    }

    /// Plugins are in the path at `point` (it must be passed every block).
    pub fn engaged_at(&self, point: usize) -> bool {
        self.points.get(point).is_some_and(|p| p.on) || self.audio.engaged_at(point)
    }
}

/// What the supervisor knows, for [`ChainControl::status`].
#[derive(Default)]
struct View {
    /// The rack's active slots, in order.
    order: Vec<(u32, String)>,
    /// The host's last report.
    host: Vec<SlotStatus>,
    /// Slots taken out of the chain, and why.
    held: HashMap<u32, String>,
    /// Why nothing runs (the host could not start, or kept crashing).
    down: Option<String>,
    /// Plugins with no place to run (a channel this engine lacks, too many
    /// channels), and why.
    refused: HashMap<u32, String>,
}

/// Talk to the chain from any thread: what to run and how it is set.
pub struct ChainControl {
    tx: mpsc::Sender<Message>,
    view: Arc<Mutex<View>>,
}

static HOST: OnceLock<Vec<OsString>> = OnceLock::new();

/// The command that runs a plugin host (tests run one from their own
/// binary). Default: this executable's `plugins host`.
pub fn set_host_command(command: Vec<OsString>) {
    let _ = HOST.set(command);
}

static EDITORS: Mutex<Option<Option<PathBuf>>> = Mutex::new(None);

/// Where a host serves its plugins' editors (tests: elsewhere, or none).
/// Default: [`editor::socket_path`].
pub fn set_editor_socket(path: Option<PathBuf>) {
    *EDITORS.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
}

fn editor_socket() -> Option<PathBuf> {
    EDITORS.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_else(editor::socket_path)
}

fn host_command() -> Vec<OsString> {
    HOST.get().cloned().unwrap_or_else(|| {
        let exe = std::env::current_exe().map(PathBuf::into_os_string).unwrap_or_else(|_| "omarchy-patch".into());
        vec![exe, "plugins".into(), "host".into()]
    })
}

/// Start the chains' supervisor (no host until a plugin is on), for an
/// engine with these channels (by index).
pub fn start(max_frames: usize, channels: &[&str]) -> Result<(Bridge, ChainControl)> {
    let audio = Arc::new(Mapping::create(max_frames)?);
    let (tx, rx) = mpsc::channel();
    let view = Arc::new(Mutex::new(View::default()));
    let points: Arc<ChannelPoints> = Arc::new(std::array::from_fn(|_| AtomicU8::new(0)));
    let armed = Arc::new(AtomicU32::new(0));
    let supervisor = Supervisor {
        armed: Arc::clone(&armed),
        channel_of: [None; POINTS],
        releasing: HashMap::new(),
        rx,
        audio: Arc::clone(&audio),
        view: Arc::clone(&view),
        names: channels.iter().take(CHANNELS).map(|c| c.to_string()).collect(),
        points: Arc::clone(&points),
        rack: RackConfig::default(),
        host: None,
        crashes: VecDeque::new(),
        gave_up: None,
    };
    thread::Builder::new()
        .name("patch-plugins".into())
        .spawn(move || supervisor.run())
        .context("start Patch plugin supervisor")?;
    Ok((
        Bridge {
            audio,
            points: (0..POINTS)
                .map(|_| Point {
                    dry_left: vec![0.0; max_frames].into_boxed_slice(),
                    dry_right: vec![0.0; max_frames].into_boxed_slice(),
                    capture_left: vec![0.0; max_frames].into_boxed_slice(),
                    capture_right: vec![0.0; max_frames].into_boxed_slice(),
                    dry_frames: 0,
                    dry_sequence: 0,
                    on: false,
                })
                .collect(),
            channels: points,
            armed,
            sequence: 1,
            submitted: false,
        },
        ChainControl { tx, view },
    ))
}

struct Host {
    child: Child,
    stdin: ChildStdin,
    beat: u64,
    beat_at: Instant,
}

struct Supervisor {
    rx: mpsc::Receiver<Message>,
    audio: Arc<Mapping>,
    view: Arc<Mutex<View>>,
    /// The engine's channels, by index.
    names: Vec<String>,
    points: Arc<ChannelPoints>,
    /// The points the bridge passes (see [`Bridge`]), the engine channel
    /// each channel point is on, and the points no longer wanted: since
    /// when, and whether the bridge has let go (their memory goes next).
    armed: Arc<AtomicU32>,
    channel_of: [Option<usize>; POINTS],
    releasing: HashMap<usize, (Instant, bool)>,
    rack: RackConfig,
    host: Option<Host>,
    /// Recent crashes no plugin could be blamed for.
    crashes: VecDeque<Instant>,
    gave_up: Option<String>,
}

fn active_set(rack: &RackConfig) -> Vec<(u32, PathBuf, Place)> {
    rack.active().map(|s| (s.id, s.plugin.clone(), s.place.clone())).collect()
}

impl Supervisor {
    fn view(&self) -> std::sync::MutexGuard<'_, View> {
        self.view.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn run(mut self) {
        loop {
            // Nothing running and nothing to give back: asleep until told.
            let message = if self.host.is_none() && self.releasing.is_empty() {
                self.rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected)
            } else {
                self.rx.recv_timeout(Duration::from_millis(50))
            };
            match message {
                Ok(Message::Sync(rack)) => self.sync(rack),
                Ok(Message::Set { slot, id, value }) => {
                    // Kept for a restarted host.
                    self.rack.set_parameter(slot, id, value);
                    self.send(&Message::Set { slot, id, value });
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            self.watch();
            self.release();
        }
        self.stop_host();
    }

    /// Arm the points `run` wants plugins at (their memory first, so the
    /// audio thread never faults on it), and mark the others for release.
    fn arm(&mut self, run: &RackConfig) {
        let mut want = 0u32;
        let mut channel_of = [None; POINTS];
        for (id, point) in run.points() {
            let Ok(point) = point else { continue };
            want |= 1 << point;
            if let Some(Place::Channel(name)) = run.slot(id).map(|s| &s.place) {
                channel_of[point] = self.names.iter().position(|n| n == name);
            }
        }
        let armed = self.armed.load(Ordering::Relaxed);
        for point in 0..POINTS {
            let bit = 1u32 << point;
            if want & bit != 0 {
                if armed & bit == 0 {
                    self.audio.prepare(point, true);
                }
                self.releasing.remove(&point);
                self.channel_of[point] = channel_of[point];
            } else if armed & bit != 0 {
                self.releasing.entry(point).or_insert((Instant::now(), false));
            }
        }
        self.armed.store(armed | want, Ordering::Release);
        self.write_channels();
    }

    /// Each engine channel's point, for the armed channel points.
    fn write_channels(&self) {
        let armed = self.armed.load(Ordering::Relaxed);
        let mut table = [0u8; CHANNELS];
        for (point, channel) in self.channel_of.iter().enumerate() {
            if let Some(c) = channel.filter(|c| *c < CHANNELS)
                && armed & 1 << point != 0
            {
                table[c] = point as u8;
            }
        }
        for (cell, point) in self.points.iter().zip(table) {
            cell.store(point, Ordering::Relaxed);
        }
    }

    /// Points no plugin is wanted at any more: once the host has faded out
    /// there (a second without it), the bridge lets go; a moment later
    /// their memory goes back to the system.
    fn release(&mut self) {
        let mut done = Vec::new();
        let mut changed = false;
        for (&point, (since, let_go)) in &mut self.releasing {
            if !*let_go {
                if since.elapsed() > Duration::from_secs(1) && !self.audio.engaged_at(point) {
                    self.armed.fetch_and(!(1 << point), Ordering::Release);
                    self.channel_of[point] = None;
                    *since = Instant::now();
                    *let_go = true;
                    changed = true;
                }
            } else if since.elapsed() > Duration::from_millis(200) {
                self.audio.prepare(point, false);
                done.push(point);
            }
        }
        for point in done {
            self.releasing.remove(&point);
        }
        if changed {
            self.write_channels();
        }
    }

    fn sync(&mut self, rack: RackConfig) {
        let before = active_set(&self.rack);
        let after = active_set(&rack);
        {
            let mut view = self.view();
            // A held plugin comes back when it is switched off (or replaced).
            view.held.retain(|id, _| {
                let was = before.iter().find(|(s, ..)| s == id);
                was.is_some() && was == after.iter().find(|(s, ..)| s == id)
            });
            view.order = rack.active().map(|s| (s.id, s.name.clone())).collect();
        }
        if before != after {
            self.crashes.clear();
            self.gave_up = None;
        }
        self.rack = rack;
        self.apply();
    }

    /// The rack minus the plugins held out of it and those with no place
    /// to run; each channel with plugins gets its point.
    fn wanted(&self) -> RackConfig {
        let held: Vec<u32> = self.view().held.keys().copied().collect();
        let mut rack = self.rack.clone();
        for slot in &mut rack.slots {
            if held.contains(&slot.id) {
                slot.active = false;
            }
        }
        let mut refused = HashMap::new();
        for slot in &rack.slots {
            if let (true, Place::Channel(name)) = (slot.active, &slot.place)
                && !self.names.iter().any(|n| n == name)
            {
                refused.insert(slot.id, format!("this engine has no channel {name}"));
            }
        }
        for (id, point) in rack.points() {
            if let Err(why) = point {
                refused.insert(id, why);
            }
        }
        for slot in &mut rack.slots {
            if refused.contains_key(&slot.id) {
                slot.active = false;
            }
        }
        // Dropping the refused leaves the others' points as they were: the
        // host works them out from the same rack.
        self.view().refused = refused;
        rack
    }

    fn apply(&mut self) {
        let run = self.wanted();
        self.arm(&run);
        self.view().down = self.gave_up.clone();
        if run.active().next().is_none() || self.gave_up.is_some() {
            self.stop_host();
            return;
        }
        if self.host.is_none()
            && let Err(error) = self.spawn()
        {
            eprintln!("Plugins Unit: {error:#}");
            self.view().down = Some(format!("the plugin host did not start: {error:#}"));
            return;
        }
        self.send(&Message::Sync(run));
    }

    fn spawn(&mut self) -> Result<()> {
        let path = self.audio.path().context("no plugin audio memory")?;
        let command = host_command();
        let (program, args) = command.split_first().context("no plugin host command")?;
        self.audio.disengage();
        // Spawned plainly (no pre_exec): a fork of the engine would stall
        // its audio thread.
        let mut command = Command::new(program);
        match editor_socket() {
            Some(editors) => command.env(HOST_EDITORS, editors),
            None => command.env_remove(HOST_EDITORS),
        };
        let mut child = command
            .args(args)
            .env(HOST_AUDIO, &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("start {}", program.to_string_lossy()))?;
        let stdin = child.stdin.take().context("plugin host stdin")?;
        let stdout = child.stdout.take().context("plugin host stdout")?;
        let view = Arc::clone(&self.view);
        thread::Builder::new()
            .name("patch-plugins-in".into())
            .spawn(move || {
                // Lines that are not reports (a test harness's) are skipped.
                for line in std::io::BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    let json = line.find('{').map_or("", |at| &line[at..]);
                    if let Ok(report) = serde_json::from_str::<Report>(json) {
                        view.lock().unwrap_or_else(|e| e.into_inner()).host = report.status;
                    }
                }
            })
            .context("read the plugin host")?;
        self.host = Some(Host {
            child,
            stdin,
            beat: self.audio.beat(),
            beat_at: Instant::now(),
        });
        Ok(())
    }

    fn send(&mut self, message: &Message) {
        let Some(host) = &mut self.host else { return };
        let Ok(line) = serde_json::to_string(message) else { return };
        // A host that cannot take it is dead: watch() finds out why.
        let _ = writeln!(host.stdin, "{line}").and_then(|()| host.stdin.flush());
    }

    /// Notice a host that died or hangs.
    fn watch(&mut self) {
        let Some(host) = &mut self.host else { return };
        let reason = match host.child.try_wait() {
            Ok(Some(status)) => format!("{status}"),
            Ok(None) => {
                let beat = self.audio.beat();
                if beat != host.beat {
                    host.beat = beat;
                    host.beat_at = Instant::now();
                    return;
                }
                if !self.audio.engaged() || host.beat_at.elapsed() < STUCK {
                    return;
                }
                let _ = host.child.kill();
                let _ = host.child.wait();
                "it stopped answering".to_string()
            }
            Err(_) => return,
        };
        let busy = self.audio.busy();
        self.host = None;
        self.crashed(&reason, busy);
    }

    /// The host is gone: straight through now, take out the plugin that
    /// was running (if one was), start again with the rest.
    fn crashed(&mut self, reason: &str, (audio, loader): (u32, u32)) {
        self.audio.disengage();
        let culprit = [audio, loader].into_iter().find(|id| *id != 0);
        {
            let mut view = self.view();
            view.host.clear();
            if let Some(id) = culprit {
                let name = view.order.iter().find(|(s, _)| *s == id).map(|(_, n)| n.clone()).unwrap_or_default();
                eprintln!("Plugins Unit: {name} (slot {id}) took the plugin host down ({reason}); held out of the chain");
                view.held.insert(id, format!("crashed ({reason}); switch it off and on to retry"));
            }
        }
        if culprit.is_none() {
            eprintln!("Plugins Unit: the plugin host stopped ({reason}); starting it again");
            let now = Instant::now();
            self.crashes.push_back(now);
            while self.crashes.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60)) {
                self.crashes.pop_front();
            }
            if self.crashes.len() >= CRASHES {
                self.gave_up = Some(format!(
                    "the plugin host crashed {} times in a minute ({reason}); switch a plugin off and on to retry",
                    self.crashes.len()
                ));
            }
        }
        self.apply();
    }

    /// Ask the host to leave (stdin closes), then make sure it has.
    fn stop_host(&mut self) {
        if let Some(Host { mut child, stdin, .. }) = self.host.take() {
            drop(stdin);
            let deadline = Instant::now() + Duration::from_secs(1);
            while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
                thread::sleep(Duration::from_millis(10));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        self.audio.disengage();
        self.view().host.clear();
    }
}

impl ChainControl {
    /// Run what the rack says: load, unload, reorder, re-set.
    pub fn sync(&self, rack: &RackConfig) {
        let _ = self.tx.send(Message::Sync(rack.clone()));
    }

    /// One control, live (a knob being turned).
    pub fn set_parameter(&self, slot: u32, id: u32, value: f64) -> bool {
        value.is_finite()
            && (0.0..=1.0).contains(&value)
            && self.tx.send(Message::Set { slot, id, value }).is_ok()
    }

    /// The rack's active plugins, in chain order, with their meters now.
    pub fn status(&self) -> Vec<SlotStatus> {
        let view = self.view.lock().unwrap_or_else(|e| e.into_inner());
        view.order
            .iter()
            .map(|(id, name)| {
                let mut row = view
                    .host
                    .iter()
                    .find(|r| r.id == *id)
                    .cloned()
                    .unwrap_or_else(|| SlotStatus::new(*id, name, SlotState::Loading));
                row.name.clone_from(name);
                if let Some(reason) = view.held.get(id).or(view.refused.get(id)).or(view.down.as_ref()) {
                    row = SlotStatus::new(*id, name, SlotState::Failed(reason.clone()));
                }
                row
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// The host's side.

struct Block {
    frames: usize,
    left: Box<[f32]>,
    right: Box<[f32]>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fade {
    In,
    Steady,
    Out,
    Done,
}

/// A plugin in the worker's chain. `instance` tells two loads of one slot
/// apart (one fading out while its replacement fades in).
struct Running {
    id: u32,
    /// The insert point it runs at.
    point: usize,
    instance: u64,
    processor: Processor,
    meter: Arc<SlotMeter>,
    fade: Fade,
}

enum WorkerCommand {
    Insert(Box<Running>),
    Remove(u64),
    Order([u32; MAX_SLOTS], usize),
}

/// Never free on the worker: a full queue is unreachable with the bounded
/// chain, and leaking beats a free there.
fn push_or_leak<T>(queue: &mut rtrb::Producer<T>, value: T) {
    if let Err(rtrb::PushError::Full(value)) = queue.push(value) {
        std::mem::forget(value);
    }
}

/// Run a plugin host on the engine's audio memory ([`HOST_AUDIO`]) until
/// the engine closes stdin. `omarchy-patch plugins host`.
pub fn serve() -> Result<()> {
    let path = std::env::var_os(HOST_AUDIO).context("no engine audio to serve")?;
    let audio = Arc::new(Mapping::open(Path::new(&path))?);
    let max_frames = audio.max_frames();
    let (commands, commands_cons) = rtrb::RingBuffer::<WorkerCommand>::new(64);
    let (returned_prod, returned) = rtrb::RingBuffer::<Box<Running>>::new(MAX_SLOTS * 2);
    let stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let stop = Arc::clone(&stop);
        let mut w = Worker {
            audio: Arc::clone(&audio),
            commands: commands_cons,
            returned: returned_prod,
            chain: Vec::with_capacity(MAX_SLOTS * 2),
            order: [0; MAX_SLOTS],
            order_len: 0,
            block: Block {
                frames: 0,
                left: vec![0.0; max_frames].into_boxed_slice(),
                right: vec![0.0; max_frames].into_boxed_slice(),
            },
            dry_left: vec![0.0; max_frames].into_boxed_slice(),
            dry_right: vec![0.0; max_frames].into_boxed_slice(),
            last: 0,
        };
        thread::Builder::new()
            .name("patch-vst3".into())
            .spawn(move || {
                worker_priority();
                w.run(&stop);
            })
            .context("start Patch VST3 worker")?
    };
    let (tx, rx) = mpsc::channel();
    if let Some(path) = std::env::var_os(HOST_EDITORS) {
        // Without it the plugins still play; Patch opens editors itself.
        if let Err(error) = listen(Path::new(&path), tx.clone()) {
            eprintln!("plugin host: no editors here: {error:#}");
        }
    }
    thread::Builder::new()
        .name("patch-plugins-in".into())
        .spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                match serde_json::from_str::<Message>(&line) {
                    Ok(message) => {
                        if tx.send(Event::Engine(message)).is_err() {
                            return;
                        }
                    }
                    Err(error) => eprintln!("plugin host: {error}"),
                }
            }
            // The listener holds a sender too: say so, or the manager waits.
            let _ = tx.send(Event::EngineGone);
        })
        .context("read the engine")?;
    Manager {
        rx,
        editors: Vec::new(),
        cursor_scaled: false,
        commands,
        returned,
        audio,
        status: Vec::new(),
        max_frames,
        loaded: HashMap::new(),
        retiring: Vec::new(),
        next_instance: 1,
        reported: Instant::now(),
    }
    .run(stop, worker);
    Ok(())
}

struct Worker {
    audio: Arc<Mapping>,
    commands: rtrb::Consumer<WorkerCommand>,
    returned: rtrb::Producer<Box<Running>>,
    // Boxed: a plugin moves between the queues and the chain as a pointer,
    // never copied or allocated on this thread.
    #[allow(clippy::vec_box)]
    chain: Vec<Box<Running>>,
    order: [u32; MAX_SLOTS],
    order_len: usize,
    block: Block,
    dry_left: Box<[f32]>,
    dry_right: Box<[f32]>,
    /// The last block taken from the engine.
    last: u64,
}

fn peak(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .chain(right)
        .fold(0.0f32, |p, s| p.max(s.abs()))
}

impl Worker {
    fn run(&mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::Acquire) {
            self.audio.tick();
            self.commands();
            let engaged = self
                .chain
                .iter()
                .filter(|r| r.fade != Fade::Done)
                .fold(0u32, |points, r| points | 1 << r.point);
            self.audio.set_engaged(engaged);
            // Read before looking: a block completed after this wakes the
            // wait below at once.
            let seen = self.audio.kicks();
            match self.audio.next(self.last) {
                Some(sequence) => {
                    self.last = sequence;
                    for point in (0..POINTS).filter(|p| engaged & 1 << p != 0) {
                        let Some(frames) =
                            self.audio.take(point, sequence, &mut self.block.left, &mut self.block.right)
                        else {
                            continue;
                        };
                        self.block.frames = frames;
                        self.render(point);
                        self.audio.finish(
                            point,
                            sequence,
                            &self.block.left[..frames],
                            &self.block.right[..frames],
                        );
                    }
                    self.retire();
                }
                // Asleep until the engine completes a block (the timeout
                // keeps commands and the watchdog's beat going).
                None => self.audio.wait(seen, Duration::from_millis(20)),
            }
        }
        self.audio.set_engaged(0);
        for running in self.chain.drain(..) {
            push_or_leak(&mut self.returned, running);
        }
    }

    fn commands(&mut self) {
        let mut reorder = false;
        while let Ok(command) = self.commands.pop() {
            match command {
                WorkerCommand::Insert(mut running) => {
                    running.fade = Fade::In;
                    if self.chain.len() < self.chain.capacity() {
                        self.chain.push(running);
                        reorder = true;
                    } else {
                        push_or_leak(&mut self.returned, running);
                    }
                }
                WorkerCommand::Remove(instance) => {
                    for running in self.chain.iter_mut().filter(|r| r.instance == instance) {
                        running.fade = match running.fade {
                            // Never heard: leave at once.
                            Fade::In => Fade::Done,
                            _ => Fade::Out,
                        };
                    }
                }
                WorkerCommand::Order(order, len) => {
                    self.order = order;
                    self.order_len = len;
                    reorder = true;
                }
            }
        }
        if reorder {
            // In place, no allocation: an unstable sort on a total key.
            let order = self.order;
            let len = self.order_len;
            let position = |id: u32| order[..len].iter().position(|o| *o == id).unwrap_or(MAX_SLOTS);
            self.chain
                .sort_unstable_by_key(|r| (position(r.id), r.fade != Fade::Out, r.instance));
        }
        // A plugin that never got a block (no audio) leaves without one.
        self.retire();
    }

    /// One point's chain on the block, in rack order.
    fn render(&mut self, point: usize) {
        let frames = self.block.frames;
        let (left, right) = (&mut self.block.left[..frames], &mut self.block.right[..frames]);
        for running in self.chain.iter_mut() {
            if running.fade == Fade::Done || running.point != point {
                continue;
            }
            SlotMeter::hold(&running.meter.input, peak(left, right), frames);
            let (dry_l, dry_r) = (&mut self.dry_left[..frames], &mut self.dry_right[..frames]);
            dry_l.copy_from_slice(left);
            dry_r.copy_from_slice(right);
            // Should this plugin take the process down, the engine knows
            // which one it was.
            self.audio.set_busy_audio(running.id);
            let t0 = Instant::now();
            let ok = running.processor.process(left, right).is_ok();
            let cost = t0.elapsed().as_nanos() as u64;
            self.audio.set_busy_audio(0);
            let finite = left.iter().chain(right.iter()).all(|s| s.is_finite());
            if !ok || !finite {
                left.copy_from_slice(dry_l);
                right.copy_from_slice(dry_r);
                running.meter.faults.fetch_add(1, Ordering::Relaxed);
            }
            match running.fade {
                Fade::In => {
                    crossfade(dry_l, left);
                    crossfade(dry_r, right);
                    running.fade = Fade::Steady;
                }
                Fade::Out => {
                    crossfade_into_from(left, dry_l);
                    crossfade_into_from(right, dry_r);
                    running.fade = Fade::Done;
                }
                Fade::Steady | Fade::Done => {}
            }
            SlotMeter::hold(&running.meter.output, peak(left, right), frames);
            let smoothed = running.meter.cost_ns.load(Ordering::Relaxed);
            running
                .meter
                .cost_ns
                .store(smoothed - smoothed / 8 + cost / 8, Ordering::Relaxed);
            running.meter.frames.store(frames as u32, Ordering::Relaxed);
        }
    }

    /// Hand finished plugins back to the manager for teardown.
    fn retire(&mut self) {
        while let Some(at) = self.chain.iter().position(|r| r.fade == Fade::Done) {
            let running = self.chain.remove(at);
            push_or_leak(&mut self.returned, running);
        }
    }
}

/// Real-time below the audio callback (FIFO 83) and above the deck
/// renderers (FIFO 70, ~85 ms ahead): FIFO 75, else the highest nice level
/// this user may take. Each block is due back by the next callback (one
/// quantum, ~5 ms); at normal priority a busy machine made it miss. It
/// sleeps whenever no block waits.
fn worker_priority() {
    let param = libc::sched_param { sched_priority: 75 };
    // SAFETY: plain syscalls on the calling thread.
    unsafe {
        if libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param) != 0 {
            for nice in [-11, -5, -1] {
                if libc::setpriority(libc::PRIO_PROCESS, 0, nice) == 0 {
                    break;
                }
            }
        }
    }
}

struct Loaded {
    instance: u64,
    path: PathBuf,
    point: usize,
    /// Its editor (None while one is shown). Declared before `control`:
    /// it goes before the plugin can.
    editor: Option<vst3_host::EditorAccess>,
    control: RtControl,
    applied: HashMap<u32, f64>,
}

/// What reaches the host's main thread: the engine's messages, and Patch
/// windows opening, using and closing plugin editors.
enum Event {
    Engine(Message),
    EngineGone,
    Open(u64, Box<HostOpen>, UnixStream),
    Input(u64, In),
    Gone(u64),
}

/// A plugin's editor shown here, on the instance that plays.
struct Shown {
    conn: u64,
    slot: u32,
    instance: u64,
    session: Session<vst3_host::EditorAccess>,
    stream: UnixStream,
    input: Vec<In>,
}

/// One line to the Patch window that asked for an editor.
fn tell(stream: &mut UnixStream, out: &Out) -> bool {
    let Ok(line) = serde_json::to_string(out) else { return true };
    writeln!(stream, "{line}").and_then(|()| stream.flush()).is_ok()
}

/// Serve editors on `path` (owner only): each connection opens one slot's
/// editor and is its line both ways (see [`editor::Editor::connect`]).
fn listen(path: &Path, tx: mpsc::Sender<Event>) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).with_context(|| format!("listen on {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    thread::Builder::new().name("patch-editors".into()).spawn(move || {
        for (n, stream) in listener.incoming().enumerate() {
            let Ok(stream) = stream else { continue };
            let (tx, conn) = (tx.clone(), n as u64 + 1);
            let _ = thread::Builder::new().name("patch-editor-io".into()).spawn(move || {
                let Ok(reader) = stream.try_clone() else { return };
                let mut lines = std::io::BufReader::new(reader).lines();
                let Some(Ok(first)) = lines.next() else { return };
                let Ok(open) = serde_json::from_str::<HostOpen>(&first) else { return };
                let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
                if tx.send(Event::Open(conn, Box::new(open), stream)).is_err() {
                    return;
                }
                for line in lines {
                    let Ok(line) = line else { break };
                    if let Ok(message) = serde_json::from_str::<In>(&line)
                        && tx.send(Event::Input(conn, message)).is_err()
                    {
                        return;
                    }
                }
                let _ = tx.send(Event::Gone(conn));
            });
        }
    })?;
    Ok(())
}

struct Manager {
    rx: mpsc::Receiver<Event>,
    /// Editors shown on running plugins.
    editors: Vec<Shown>,
    cursor_scaled: bool,
    commands: rtrb::Producer<WorkerCommand>,
    returned: rtrb::Consumer<Box<Running>>,
    audio: Arc<Mapping>,
    status: Vec<(SlotStatus, Arc<SlotMeter>)>,
    max_frames: usize,
    loaded: HashMap<u32, Loaded>,
    /// Controls of plugins on their way out, by instance: torn down here
    /// once the worker returns the plugin.
    retiring: Vec<(u64, RtControl)>,
    next_instance: u64,
    reported: Instant,
}

impl Manager {
    fn run(mut self, stop: Arc<AtomicBool>, worker: JoinHandle<()>) {
        loop {
            // An open editor is served at its own pace (2 ms with the keys).
            let wait = self.editors.iter().map(|e| e.session.pace()).min().unwrap_or(Duration::from_millis(50));
            match self.rx.recv_timeout(wait) {
                Ok(Event::Engine(Message::Sync(rack))) => self.sync(&rack),
                Ok(Event::Engine(Message::Set { slot, id, value })) => {
                    if let Some(loaded) = self.loaded.get_mut(&slot)
                        && loaded.control.set_parameter(id, value)
                    {
                        loaded.applied.insert(id, value);
                    }
                }
                Ok(Event::Open(conn, open, stream)) => self.show_editor(conn, *open, stream),
                Ok(Event::Input(conn, message)) => {
                    if let Some(shown) = self.editors.iter_mut().find(|e| e.conn == conn) {
                        shown.input.push(message);
                    }
                }
                Ok(Event::Gone(conn)) => {
                    if let Some(at) = self.editors.iter().position(|e| e.conn == conn) {
                        let shown = self.editors.swap_remove(at);
                        self.give_back(shown);
                    }
                }
                Ok(Event::EngineGone) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            self.serve_editors();
            self.teardown();
            if self.reported.elapsed() >= Duration::from_millis(100) && !self.report() {
                // The engine is gone.
                break;
            }
        }
        // Leaving: the editors close first, then the worker hands its
        // plugins back and they go here.
        for shown in std::mem::take(&mut self.editors) {
            self.give_back(shown);
        }
        for loaded in self.loaded.values_mut() {
            loaded.editor = None;
        }
        stop.store(true, Ordering::Release);
        let _ = worker.join();
        let loaded: Vec<_> = self.loaded.drain().map(|(_, l)| (l.instance, l.control)).collect();
        self.retiring.extend(loaded);
        self.teardown();
    }

    /// Tell the engine how each plugin is doing (false: it is not there).
    fn report(&mut self) -> bool {
        self.reported = Instant::now();
        let status = self
            .status
            .iter()
            .map(|(row, meter)| {
                let frames = meter.frames.load(Ordering::Relaxed).max(1);
                let block_ns = frames as f64 * 1e9 / f64::from(crate::RATE);
                SlotStatus {
                    input_peak: load_f32(&meter.input),
                    output_peak: load_f32(&meter.output),
                    load: (meter.cost_ns.load(Ordering::Relaxed) as f64 / block_ns) as f32,
                    faults: meter.faults.load(Ordering::Relaxed),
                    ..row.clone()
                }
            })
            .collect();
        let Ok(line) = serde_json::to_string(&Report { status }) else { return true };
        let mut out = std::io::stdout().lock();
        writeln!(out, "{line}").and_then(|()| out.flush()).is_ok()
    }

    fn teardown(&mut self) {
        while let Ok(running) = self.returned.pop() {
            let instance = running.instance;
            let slot = running.id;
            self.audio.set_busy_loader(slot);
            drop(running);
            if let Some(at) = self.retiring.iter().position(|(i, _)| *i == instance) {
                let (_, mut control) = self.retiring.swap_remove(at);
                control.service_teardown();
            } else if self.loaded.get(&slot).is_some_and(|l| l.instance == instance) {
                // The worker turned it away (chain full): unload it.
                self.close_editors(slot);
                let mut loaded = self.loaded.remove(&slot).expect("present");
                loaded.editor = None;
                loaded.control.service_teardown();
                self.set_state(slot, SlotState::Failed("the chain is full".into()));
            }
            self.audio.set_busy_loader(0);
        }
    }

    /// A Patch window asks for a running plugin's editor: shown here, on
    /// the instance that plays, or refused (Patch then opens it itself).
    fn show_editor(&mut self, conn: u64, open: HostOpen, mut stream: UnixStream) {
        let slot = open.slot;
        let access = match self.loaded.get_mut(&slot) {
            None => Err("it is not running in the plugin host".to_string()),
            Some(l) if l.path != open.request.plugin => Err("the slot holds another plugin now".to_string()),
            Some(l) => l.editor.take().ok_or_else(|| "its editor is open already".to_string()),
        };
        let access = match access {
            Ok(access) => access,
            Err(why) => {
                tell(&mut stream, &Out::Refused(why));
                return;
            }
        };
        let instance = self.loaded[&slot].instance;
        self.scale_cursor(open.request.scale);
        self.audio.set_busy_loader(slot);
        let opened = Session::open(access, &open.request);
        self.audio.set_busy_loader(0);
        match opened {
            Ok((session, first)) => {
                for out in &first {
                    tell(&mut stream, out);
                }
                self.editors.push(Shown { conn, slot, instance, session, stream, input: Vec::new() });
            }
            Err((access, error)) => {
                if let Some(l) = self.loaded.get_mut(&slot) {
                    l.editor = Some(access);
                }
                tell(&mut stream, &Out::Refused(format!("{error:#}")));
            }
        }
    }

    /// Serve every editor shown here once; close those whose window or
    /// Patch window is gone.
    fn serve_editors(&mut self) {
        let mut at = 0;
        let mut out = Vec::new();
        while at < self.editors.len() {
            let shown = &mut self.editors[at];
            self.audio.set_busy_loader(shown.slot);
            let alive = shown.session.step(shown.input.drain(..), &mut out);
            self.audio.set_busy_loader(0);
            let told = out.drain(..).all(|o| tell(&mut shown.stream, &o));
            if alive && told {
                at += 1;
            } else {
                let shown = self.editors.swap_remove(at);
                self.give_back(shown);
            }
        }
    }

    /// Close an editor shown here; its plugin keeps playing, ready to show
    /// it again.
    fn give_back(&mut self, shown: Shown) {
        let Shown { slot, instance, session, stream, .. } = shown;
        self.audio.set_busy_loader(slot);
        let access = session.close();
        self.audio.set_busy_loader(0);
        let _ = stream.shutdown(std::net::Shutdown::Both);
        if let Some(l) = self.loaded.get_mut(&slot)
            && l.instance == instance
            && l.editor.is_none()
        {
            l.editor = Some(access);
        }
    }

    /// Close a slot's editors before its plugin goes.
    fn close_editors(&mut self, slot: u32) {
        while let Some(at) = self.editors.iter().position(|e| e.slot == slot) {
            let shown = self.editors.swap_remove(at);
            let Shown { session, stream, .. } = shown;
            self.audio.set_busy_loader(slot);
            session.close();
            self.audio.set_busy_loader(0);
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    /// The pointer over an editor at the screen's scale, as Patch's own
    /// editor processes have it (X windows are not scaled here).
    fn scale_cursor(&mut self, scale: f32) {
        if std::mem::replace(&mut self.cursor_scaled, true) {
            return;
        }
        let Some(size) = std::env::var("XCURSOR_SIZE").ok().and_then(|s| s.parse::<f32>().ok()) else { return };
        // SAFETY: only a variable already set is replaced, on this thread,
        // before the first window loads its cursor. glibc swaps the entry's
        // pointer in place (the array is not reallocated and the old string
        // is not freed), so a plugin's thread reading the environment
        // meanwhile still reads valid memory.
        unsafe { std::env::set_var("XCURSOR_SIZE", ((size * scale.max(1.0)).round() as u32).to_string()) };
    }

    fn set_state(&mut self, slot: u32, state: SlotState) {
        if let Some((s, _)) = self.status.iter_mut().find(|(s, _)| s.id == slot) {
            s.state = state;
        }
    }

    fn remove(&mut self, slot: u32) {
        self.close_editors(slot);
        if let Some(mut loaded) = self.loaded.remove(&slot) {
            loaded.editor = None;
            let _ = self.commands.push(WorkerCommand::Remove(loaded.instance));
            self.retiring.push((loaded.instance, loaded.control));
        }
    }

    fn sync(&mut self, rack: &RackConfig) {
        let points: HashMap<u32, usize> = rack
            .points()
            .into_iter()
            .filter_map(|(id, point)| Some((id, point.ok()?)))
            .collect();
        let wanted: Vec<_> = rack.active().filter(|s| points.contains_key(&s.id)).take(MAX_SLOTS).collect();
        // Moved elsewhere in the mix: out there, in afresh here.
        let stale: Vec<u32> = self
            .loaded
            .iter()
            .filter(|(id, l)| {
                !wanted
                    .iter()
                    .any(|s| s.id == **id && s.plugin == l.path && points.get(&s.id) == Some(&l.point))
            })
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            self.remove(id);
        }
        // Status rows for what should run, in order (meters carry over).
        let old = std::mem::take(&mut self.status);
        for slot in &wanted {
            let (mut row, meter) = old
                .iter()
                .find(|(s, _)| s.id == slot.id)
                .cloned()
                .unwrap_or_else(|| {
                    (
                        SlotStatus::new(slot.id, &slot.name, SlotState::Loading),
                        Arc::new(SlotMeter::default()),
                    )
                });
            row.name.clone_from(&slot.name);
            if !self.loaded.contains_key(&slot.id) {
                row.state = SlotState::Loading;
            }
            self.status.push((row, meter));
        }
        let mut order = [0u32; MAX_SLOTS];
        for (o, slot) in order.iter_mut().zip(&wanted) {
            *o = slot.id;
        }
        let _ = self.commands.push(WorkerCommand::Order(order, wanted.len()));
        for slot in &wanted {
            if let Some(loaded) = self.loaded.get_mut(&slot.id) {
                apply(loaded, &slot.parameters);
                continue;
            }
            self.audio.set_busy_loader(slot.id);
            let loaded = Processor::load_with_parameters(
                &slot.plugin,
                crate::RATE,
                self.max_frames,
                &slot.parameters,
            );
            self.audio.set_busy_loader(0);
            match loaded {
                Ok(mut processor) => {
                    let Some(control) = processor.take_control() else { continue };
                    // SAFETY: Loaded keeps it, and drops it (closing any
                    // editor first) before the processor or its control
                    // goes: remove, the chain-full teardown, leaving.
                    let editor = unsafe { processor.editor() };
                    let instance = self.next_instance;
                    self.next_instance += 1;
                    let meter = self
                        .status
                        .iter()
                        .find(|(s, _)| s.id == slot.id)
                        .map(|(_, m)| m.clone())
                        .unwrap_or_default();
                    let point = points.get(&slot.id).copied().unwrap_or(0);
                    let running = Box::new(Running {
                        id: slot.id,
                        point,
                        instance,
                        processor,
                        meter,
                        fade: Fade::In,
                    });
                    match self.commands.push(WorkerCommand::Insert(running)) {
                        Ok(()) => {
                            self.loaded.insert(
                                slot.id,
                                Loaded {
                                    instance,
                                    path: slot.plugin.clone(),
                                    point,
                                    editor,
                                    control,
                                    applied: slot.parameters.iter().map(|p| (p.id, p.value)).collect(),
                                },
                            );
                            self.set_state(slot.id, SlotState::Running);
                        }
                        Err(rtrb::PushError::Full(WorkerCommand::Insert(running))) => {
                            drop(running);
                            let mut control = control;
                            control.service_teardown();
                            self.set_state(slot.id, SlotState::Failed("the host is busy".into()));
                        }
                        Err(_) => {}
                    }
                }
                Err(error) => self.set_state(slot.id, SlotState::Failed(format!("{error:#}"))),
            }
        }
        // Re-send the order: inserts arrive after it in the same drain.
        let _ = self.commands.push(WorkerCommand::Order(order, wanted.len()));
        self.report();
    }
}

/// Queue the saved values that differ from what the instance has.
fn apply(loaded: &mut Loaded, saved: &[SavedParameter]) {
    for p in saved {
        let current = loaded.applied.get(&p.id).copied();
        if current.is_none_or(|v| (v - p.value).abs() > 1e-6)
            && p.value.is_finite()
            && (0.0..=1.0).contains(&p.value)
            && loaded.control.set_parameter(p.id, p.value)
        {
            loaded.applied.insert(p.id, p.value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Slot, View};

    /// Not a test: this binary is the tests' plugin host (see `host`).
    /// With `PATCH_TEST_HOST_CRASH` set to a file that is not there yet, it
    /// makes the file and, 800 ms later, once, does `PATCH_TEST_HOST_FAULT`:
    /// `abort` or `hang` (stop) between plugins, or `blame` (abort while
    /// slot 1 is busy).
    #[test]
    fn plugin_host_entry_for_chain_tests() {
        let Some(audio) = std::env::var_os(HOST_AUDIO) else { return };
        if let Some(marker) = std::env::var_os("PATCH_TEST_HOST_CRASH")
            && std::fs::OpenOptions::new().write(true).create_new(true).open(&marker).is_ok()
        {
            let fault = std::env::var("PATCH_TEST_HOST_FAULT").unwrap_or_default();
            let audio = Mapping::open(Path::new(&audio)).unwrap();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(800));
                if fault == "blame" {
                    audio.set_busy_loader(1);
                } else {
                    // Between plugins, so nobody is to blame.
                    while audio.busy() != (0, 0) {}
                }
                if fault == "hang" {
                    // SAFETY: stops this whole process, as a stuck plugin would.
                    unsafe { libc::raise(libc::SIGSTOP) };
                }
                // A crash on purpose: no core dump, so no desktop "process
                // crashed" notice from systemd-coredump.
                // SAFETY: a plain prctl on this process.
                unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) };
                std::process::abort();
            });
        }
        serve().unwrap();
        std::process::exit(0);
    }

    /// One test with a host at a time: the crash switch is process-wide.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn host() -> std::sync::MutexGuard<'static, ()> {
        let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        // Never the live socket: a test host would take it from Patch's.
        set_editor_socket(None);
        let exe = std::env::current_exe().unwrap().into_os_string();
        set_host_command(vec![
            exe,
            "plugin_host_entry_for_chain_tests".into(),
            "--nocapture".into(),
            "--test-threads=1".into(),
        ]);
        serial
    }

    fn box_of_bass() -> Option<PathBuf> {
        let path =
            crate::bundle_path(&PathBuf::from(std::env::var_os("HOME").unwrap_or_default()));
        crate::scan::binary(&path).map(|_| path).or_else(|| {
            eprintln!("Box Of Bass is not installed; skipping the chain test");
            None
        })
    }

    fn rack(path: &Path, active: bool, parameters: Vec<SavedParameter>) -> RackConfig {
        RackConfig {
            slots: vec![Slot {
                id: 1,
                plugin: path.to_path_buf(),
                name: "Box Of Bass".into(),
                vendor: String::new(),
                version: String::new(),
                active,
                view: View::Patch,
                zoom: 1.0,
                place: Place::Master,
                parameters,
            }],
        }
    }

    fn tone() -> Vec<f32> {
        (0..256)
            .map(|i| (std::f32::consts::TAU * 55.0 * i as f32 / 48_000.0).sin() * 0.02)
            .collect()
    }

    /// Feed blocks the way the graph callback does, `n` of them, one
    /// quantum apart.
    fn run(bridge: &mut Bridge, source: &[f32], n: usize) -> Vec<(Vec<f32>, Vec<f32>)> {
        (0..n)
            .map(|_| {
                let mut left = source.to_vec();
                let mut right = source.to_vec();
                bridge.process(0, &mut left, &mut right);
                bridge.end_block();
                thread::sleep(Duration::from_millis(5));
                (left, right)
            })
            .collect()
    }

    fn until(bridge: &mut Bridge, source: &[f32], secs: u64, done: impl Fn(&Bridge) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while !done(bridge) && Instant::now() < deadline {
            for (left, right) in run(bridge, source, 1) {
                assert!(left.iter().chain(&right).all(|s| s.is_finite()));
            }
        }
        done(bridge)
    }

    #[test]
    fn an_empty_chain_passes_audio_straight_through() {
        let (mut bridge, control) = start(1024, &[]).unwrap();
        let source: Vec<f32> = (0..256).map(|i| (i as f32 * 0.1).sin()).collect();
        for (left, right) in run(&mut bridge, &source, 4) {
            assert_eq!(left, source);
            assert_eq!(right, source);
        }
        assert!(!bridge.engaged());
        assert!(control.status().is_empty());
    }

    /// News from an editor until `done` says so, or `secs` pass (the audio
    /// keeps flowing meanwhile).
    fn news_until(
        bridge: &mut Bridge,
        editor: &mut crate::editor::Editor,
        secs: u64,
        done: impl Fn(&crate::editor::News) -> bool,
    ) -> crate::editor::News {
        let deadline = Instant::now() + Duration::from_secs(secs);
        let mut all = crate::editor::News::default();
        while Instant::now() < deadline {
            run(bridge, &tone(), 1);
            let news = editor.poll();
            all.opened = all.opened.or(news.opened);
            all.zoom = all.zoom.or(news.zoom);
            all.refused = all.refused.or(news.refused);
            all.closed |= news.closed;
            all.edits.extend(news.edits);
            if done(&all) {
                break;
            }
        }
        all
    }

    #[test]
    #[ignore = "opens a window on the desktop"]
    fn a_running_plugin_shows_its_editor_from_the_host_that_plays_it() {
        let Some(path) = box_of_bass() else { return };
        if std::env::var_os("DISPLAY").is_none() {
            eprintln!("no X display; skipping the host editor test");
            return;
        }
        let _serial = host();
        let dir = std::env::temp_dir().join(format!("patch-editors-{}", std::process::id()));
        let socket = dir.join("plugin-editors.sock");
        set_editor_socket(Some(socket.clone()));
        let (mut bridge, control) = start(8192, &[]).unwrap();
        let source = tone();
        control.sync(&rack(&path, true, Vec::new()));
        assert!(until(&mut bridge, &source, 10, Bridge::engaged), "the plugin never joined");
        set_editor_socket(None);
        let request = |title: &str| crate::editor::Request {
            plugin: path.clone(),
            parameters: Vec::new(),
            scale: 1.0,
            zoom: 1.0,
            title: title.into(),
        };
        // A slot the host does not run: refused, so Patch opens it itself.
        let mut stray = crate::editor::Editor::connect_at(&socket, 7, &request("stray")).unwrap();
        let news = news_until(&mut bridge, &mut stray, 5, |n| n.refused.is_some());
        assert!(news.refused.is_some_and(|why| why.contains("not running")), "not refused");
        // The running one: its window opens on the instance that plays.
        let mut editor = crate::editor::Editor::connect_at(&socket, 1, &request("host editor test")).unwrap();
        let news = news_until(&mut bridge, &mut editor, 10, |n| n.opened.is_some());
        assert!(news.opened.is_some_and(|(w, h)| w > 0 && h > 0), "the editor never opened");
        assert!(news.refused.is_none());
        // The audio plays through it the whole time.
        let blocks = run(&mut bridge, &source, 20);
        assert!(blocks.iter().any(|(l, _)| l.iter().zip(&source).any(|(a, b)| (a - b).abs() > 1e-6)));
        // Closed from Patch: the window goes, the plugin plays on, and its
        // editor can be shown again.
        drop(editor);
        thread::sleep(Duration::from_millis(300));
        assert_eq!(control.status()[0].state, SlotState::Running);
        assert!(bridge.engaged());
        let mut again = crate::editor::Editor::connect_at(&socket, 1, &request("host editor test 2")).unwrap();
        let news = news_until(&mut bridge, &mut again, 10, |n| n.opened.is_some() || n.refused.is_some());
        assert!(news.opened.is_some(), "not shown again: {:?}", news.refused);
        // The plugin leaves the chain with its editor open: the window
        // closes with it.
        control.sync(&rack(&path, false, Vec::new()));
        let news = news_until(&mut bridge, &mut again, 10, |n| n.closed);
        assert!(news.closed, "the editor outlived its plugin");
        assert!(until(&mut bridge, &source, 5, |b| !b.engaged()), "the plugin never left");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_plugin_joins_and_leaves_while_audio_plays() {
        let Some(path) = box_of_bass() else { return };
        let _serial = host();
        let (mut bridge, control) = start(8192, &[]).unwrap();
        let source = tone();
        control.sync(&rack(&path, true, Vec::new()));
        assert!(until(&mut bridge, &source, 10, Bridge::engaged), "the plugin never joined");
        let blocks = run(&mut bridge, &source, 40);
        assert!(blocks.iter().flat_map(|(l, r)| l.iter().chain(r)).all(|s| s.is_finite()));
        assert!(
            blocks
                .iter()
                .any(|(l, _)| l.iter().zip(&source).any(|(a, b)| (a - b).abs() > 1e-6)),
            "the plugin changed nothing"
        );
        thread::sleep(Duration::from_millis(200));
        let status = control.status();
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].state, SlotState::Running);
        assert!(status[0].output_peak > 0.0);
        // A live parameter change is accepted while it runs.
        assert!(control.set_parameter(1, 0, 0.5));
        control.sync(&rack(&path, false, Vec::new()));
        assert!(until(&mut bridge, &source, 5, |b| !b.engaged()), "the plugin never left");
        for (left, _) in run(&mut bridge, &source, 3) {
            assert_eq!(left, source);
        }
        assert!(control.status().is_empty());
        // Gone for good: the bridge lets go of the master point and its
        // memory goes back; the audio still passes, untouched.
        assert!(
            until(&mut bridge, &source, 4, |b| b.armed.load(Ordering::Relaxed) == 0),
            "the master point stayed armed"
        );
        for (left, _) in run(&mut bridge, &source, 3) {
            assert_eq!(left, source);
        }
    }

    /// Box Of Bass on the master, then the host fails as `fault` says:
    /// the master goes straight through again, never silent or invalid.
    fn host_fails(fault: &str) -> Option<(Bridge, ChainControl, PathBuf, PathBuf)> {
        let path = box_of_bass()?;
        let marker = std::env::temp_dir().join(format!("patch-host-{fault}-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        // SAFETY: set before this test's host starts, under SERIAL.
        unsafe {
            std::env::set_var("PATCH_TEST_HOST_CRASH", &marker);
            std::env::set_var("PATCH_TEST_HOST_FAULT", fault);
        }
        let (mut bridge, control) = start(8192, &[]).unwrap();
        let source = tone();
        control.sync(&rack(&path, true, Vec::new()));
        assert!(until(&mut bridge, &source, 10, Bridge::engaged), "the plugin never joined");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut dropped = false;
        while Instant::now() < deadline && !dropped {
            for (left, _) in run(&mut bridge, &source, 1) {
                assert!(left.iter().all(|s| s.is_finite()));
                assert!(left.iter().any(|s| s.abs() > 1e-4), "the master went silent");
            }
            dropped = !bridge.engaged();
        }
        assert!(dropped, "the host never failed ({fault})");
        Some((bridge, control, path, marker))
    }

    fn done(marker: &Path) {
        // SAFETY: under SERIAL.
        unsafe {
            std::env::remove_var("PATCH_TEST_HOST_CRASH");
            std::env::remove_var("PATCH_TEST_HOST_FAULT");
        }
        let _ = std::fs::remove_file(marker);
    }

    #[test]
    fn a_crashed_host_starts_again_with_its_plugins() {
        let _serial = host();
        let Some((mut bridge, control, _, marker)) = host_fails("abort") else { return };
        // Nobody to blame: a new host runs Box Of Bass again.
        assert!(until(&mut bridge, &tone(), 10, Bridge::engaged), "no new host");
        thread::sleep(Duration::from_millis(300));
        assert_eq!(control.status()[0].state, SlotState::Running);
        done(&marker);
    }

    #[test]
    fn the_plugin_that_crashed_the_host_is_held_until_switched_on_again() {
        let _serial = host();
        let Some((mut bridge, control, path, marker)) = host_fails("blame") else { return };
        let source = tone();
        assert!(!until(&mut bridge, &source, 1, Bridge::engaged), "a crasher came back by itself");
        let SlotState::Failed(why) = control.status()[0].state.clone() else {
            panic!("not held: {:?}", control.status())
        };
        assert!(why.contains("crashed"), "{why}");
        // Off and on again: back in the chain.
        control.sync(&rack(&path, false, Vec::new()));
        control.sync(&rack(&path, true, Vec::new()));
        assert!(until(&mut bridge, &source, 10, Bridge::engaged), "it never came back");
        done(&marker);
    }

    #[test]
    fn a_stuck_host_is_stopped_and_replaced() {
        let _serial = host();
        let Some((mut bridge, control, _, marker)) = host_fails("hang") else { return };
        assert!(until(&mut bridge, &tone(), 10, Bridge::engaged), "no new host");
        thread::sleep(Duration::from_millis(300));
        assert_eq!(control.status()[0].state, SlotState::Running);
        done(&marker);
    }

    /// A running plugin's cost, measured: Box Of Bass on the master, four
    /// seconds of 256-frame blocks paced as the graph runs them. The host
    /// sleeps between blocks and wakes once for each; every block comes
    /// back in time. Run by hand:
    /// `cargo test -p patch-plugin --release a_running_plugin -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement: four seconds of real-time blocks"]
    fn a_running_plugin_wakes_its_host_once_a_block() {
        let Some(path) = box_of_bass() else { return };
        let _serial = host();
        let (mut bridge, control) = start(8192, &[]).unwrap();
        let source = tone();
        control.sync(&rack(&path, true, Vec::new()));
        assert!(until(&mut bridge, &source, 10, Bridge::engaged), "the plugin never joined");
        // The host: this binary's child running the test host entry.
        let me = std::process::id().to_string();
        let child = std::fs::read_dir("/proc")
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
            .find(|pid| {
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
                let cmd = std::fs::read_to_string(format!("/proc/{pid}/cmdline")).unwrap_or_default();
                stat.rsplit(')').next().and_then(|r| r.split_whitespace().nth(1)) == Some(me.as_str())
                    && cmd.contains("plugin_host_entry_for_chain_tests")
            })
            .expect("no plugin host");
        // Wakeups and CPU ticks per thread name: the worker's alone, the
        // others (the manager, the plugin's own threads) apart.
        let usage = || {
            let mut rows: Vec<(String, u64, u64)> = Vec::new();
            for t in std::fs::read_dir(format!("/proc/{child}/task")).unwrap().flatten() {
                let dir = t.path();
                let comm = std::fs::read_to_string(dir.join("comm")).unwrap_or_default().trim().to_string();
                let status = std::fs::read_to_string(dir.join("status")).unwrap_or_default();
                let wakeups = status
                    .lines()
                    .find_map(|l| l.strip_prefix("voluntary_ctxt_switches:"))
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .unwrap_or(0);
                let stat = std::fs::read_to_string(dir.join("stat")).unwrap_or_default();
                let f: Vec<u64> = stat.rsplit(')').next().unwrap_or("").split_whitespace().filter_map(|v| v.parse().ok()).collect();
                let ticks = f.get(11).copied().unwrap_or(0) + f.get(12).copied().unwrap_or(0);
                rows.push((comm, wakeups, ticks));
            }
            let worker = rows.iter().filter(|r| r.0 == "patch-vst3").fold((0, 0), |a, r| (a.0 + r.1, a.1 + r.2));
            let all = rows.iter().fold((0, 0), |a, r| (a.0 + r.1, a.1 + r.2));
            (worker, all)
        };
        let blocks = 750u32;
        let period = Duration::from_nanos(256 * 1_000_000_000 / 48_000);
        let (worker0, all0) = usage();
        let late0 = bridge.audio.late();
        let started = Instant::now();
        for n in 0..blocks {
            let mut left = source.clone();
            let mut right = source.clone();
            bridge.process(0, &mut left, &mut right);
            bridge.end_block();
            if let Some(rest) = (started + period * (n + 1)).checked_duration_since(Instant::now()) {
                thread::sleep(rest);
            }
        }
        let secs = started.elapsed().as_secs_f64();
        let (worker1, all1) = usage();
        let late = bridge.audio.late() - late0;
        let per_s = |a: u64, b: u64| (b - a) as f64 / secs;
        let worker = per_s(worker0.0, worker1.0);
        let rest = per_s(all0.0, all1.0) - worker;
        eprintln!(
            "{blocks} blocks in {secs:.2} s ({:.0}/s), {late} late; host worker: {worker:.0} wakeups/s, {:.0} ms CPU/s; \
             the rest of the host (the plugin's own threads included): {rest:.0} wakeups/s, {:.0} ms CPU/s",
            f64::from(blocks) / secs,
            per_s(worker0.1, worker1.1) * 10.0,
            per_s(all0.1 - worker0.1, all1.1 - worker1.1) * 10.0,
        );
        assert!(worker < 1.5 * f64::from(blocks) / secs, "the worker polls: {worker:.0} wakeups/s");
        assert!(late <= 2, "{late} blocks came back late");
        control.sync(&rack(&path, false, Vec::new()));
    }
}
