//! The engine process. The PipeWire filter runs on the main thread; beside
//! it, a collector keeps the last seconds of stamped meter blocks, the
//! link keeper wires the graph, and a control socket
//! (`$XDG_RUNTIME_DIR/omarchy-patch/engine.sock`) takes one JSON request per
//! line (see [`crate::client`] for the requests). The socket is mode 0600:
//! same-uid peers are trusted. Request and reply reads are bounded in time
//! and size.

use crate::analysis::{self, Analysis};
use crate::links::Links;
use crate::{Clock, MAX_CHANNELS, MAX_FRAMES, MeterBlock, Params, Rt, db_to_linear, linear_to_db};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::ffi::{CString, c_char, c_void};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod info;
mod play_history;
use info::*;
mod dj_ops;
use dj_ops::*;
mod mixer_ops;
use mixer_ops::*;
mod io_ops;
use io_ops::*;
mod lifecycle;
use lifecycle::*;
mod visual_ops;
use visual_ops::*;
mod eq_settings;
use eq_settings::*;
mod mics;
use mics::*;
mod sync;
use sync::*;
mod commands;
use commands::*;
mod plan;
use plan::*;
mod evaluate;
use evaluate::*;
mod prepare;
use prepare::*;
mod final_check;
use final_check::*;
mod tick;
use tick::*;
mod sets;
use sets::*;
mod ai_view;
use ai_view::*;
mod pdeath;
mod system;
use system::*;
mod decks;
mod health;
use decks::*;
use patch_core::command::HOT_CUES;
mod stage;
use stage::*;

mod control;

unsafe extern "C" {
    fn patch_engine_new(
        names: *const c_char,
        process: extern "C" fn(*mut c_void, *const *const f32, *mut *mut f32, *const Clock),
        user: *mut c_void,
    ) -> *mut c_void;
    fn patch_engine_run(handle: *mut c_void);
    fn patch_engine_quit(handle: *mut c_void);
    fn patch_engine_set_name(handle: *mut c_void, name: *const std::ffi::c_char);
    fn patch_engine_set_always(handle: *mut c_void, on: std::ffi::c_int);
    fn patch_engine_watch_volume(
        handle: *mut c_void,
        sink: *const c_char,
        volume: extern "C" fn(*mut c_void, f32, f32, std::ffi::c_int),
        user: *mut c_void,
    ) -> std::ffi::c_int;
}

/// The `patch` sink's volume from the engine's PipeWire listener (its main
/// loop): `user` is a `Params` kept alive for the process.
extern "C" fn bus_volume_changed(user: *mut c_void, left: f32, right: f32, mute: std::ffi::c_int) {
    // SAFETY: `user` is the Arc<Params> leaked in `run`, never freed.
    let p = unsafe { &*(user as *const Params) };
    p.bus_volume_changed(left, right, mute != 0);
}

/// Engine channels: the Patch bus (apps), four native deck/source lanes, and four channels for
/// applications that have their own EQ (their sinks exist only then).
pub const CHANNELS: [&str; 60] = [
    "bus",
    "deck-a",
    "deck-b",
    "deck-c",
    "deck-d",
    "app-1",
    "app-2",
    "app-3",
    "app-4",
    "mic-1",
    "mic-2",
    "mic-3",
    "mic-4",
    "mic-5",
    "mic-6",
    "mic-7",
    "mic-8",
    "mic-9",
    "mic-10",
    "mic-11",
    "mic-12",
    "mic-13",
    "mic-14",
    "mic-15",
    "mic-16",
    // The engine's own voices, on strips of their own: the step Sequencer's
    // drums, the Pattern Unit (its drum voices and derived material), and
    // the Tidal Unit's instrument.
    "sequencer",
    "pattern",
    "tidal",
    "stream-1",
    "stream-2",
    "stream-3",
    "stream-4",
    "stream-5",
    "stream-6",
    "stream-7",
    "stream-8",
    "stream-9",
    "stream-10",
    "stream-11",
    "stream-12",
    "stream-13",
    "stream-14",
    "stream-15",
    "stream-16",
    "stream-17",
    "stream-18",
    "stream-19",
    "stream-20",
    "stream-21",
    "stream-22",
    "stream-23",
    "stream-24",
    "stream-25",
    "stream-26",
    "stream-27",
    "stream-28",
    "stream-29",
    "stream-30",
    "stream-31",
    "stream-32",
];

pub fn socket_path() -> PathBuf {
    patch_core::ipc::runtime_dir().join("engine.sock")
}

/// Where a retiring engine listens during a handover.
pub fn retiring_socket_path() -> PathBuf {
    patch_core::ipc::runtime_dir().join("engine-retiring.sock")
}

/// Node name of a retiring engine (the new one takes `patch.engine`).
pub const RETIRING: &str = "patch.engine.retiring";

extern "C" fn process(
    user: *mut c_void,
    ins: *const *const f32,
    outs: *mut *mut f32,
    clock: *const Clock,
) {
    let outs_ptr = outs;
    // SAFETY: `user` is the Rt handed to patch_engine_new, touched only here
    // on the RT thread. PipeWire gives `frames` samples per buffer; input
    // pointers are null for unconnected ports.
    let rt = unsafe { &mut *(user as *mut Rt) };
    // Denormals flushed on this thread: a decaying filter state (an EQ on a
    // paused deck, a reverb tail) never falls into the slow range.
    patch_dsp::cpu::flush_denormals();
    let clock = unsafe { *clock };
    let n = (clock.frames as usize).min(MAX_FRAMES);
    let chans = rt.params.channels();
    let mut inputs: [Option<&[f32]>; MAX_CHANNELS * 2] = [None; MAX_CHANNELS * 2];
    for (i, slot) in inputs.iter_mut().enumerate().take(chans * 2) {
        let p = unsafe { *ins.add(i) };
        if !p.is_null() {
            *slot = Some(unsafe { std::slice::from_raw_parts(p, n) });
        }
    }
    let (l, r) = unsafe { (*outs, *outs.add(1)) };
    let outs = unsafe {
        [
            std::slice::from_raw_parts_mut(l, n),
            std::slice::from_raw_parts_mut(r, n),
        ]
    };
    // The cue pair is null while no headphones are linked.
    let (cl, cr) = unsafe { (*outs_ptr.add(2), *outs_ptr.add(3)) };
    let cue = (!cl.is_null() && !cr.is_null()).then(|| unsafe {
        [
            std::slice::from_raw_parts_mut(cl, n),
            std::slice::from_raw_parts_mut(cr, n),
        ]
    });
    let mic = unsafe { *ins.add(chans * 2) };
    let mic = (!mic.is_null()).then(|| unsafe { std::slice::from_raw_parts(mic, n) });
    rt.process(&clock, &inputs[..chans * 2], outs, cue);
    for (side, audio) in rt.input_audio(n).into_iter().enumerate() {
        let port = unsafe { *outs_ptr.add(11 + side) };
        if !port.is_null() {
            // SAFETY: additional capture output ports, allocated by the C shim.
            unsafe { std::slice::from_raw_parts_mut(port, n) }.copy_from_slice(audio);
        }
    }
    // The program for recording (`patch.program`), before the system volume.
    for (side, audio) in rt.program_audio(n).into_iter().enumerate() {
        let port = unsafe { *outs_ptr.add(13 + side) };
        if !port.is_null() {
            // SAFETY: the program_FL/FR output ports, allocated by the C shim.
            unsafe { std::slice::from_raw_parts_mut(port, n) }.copy_from_slice(audio);
        }
    }
    // The Mixer's AUX 1 and 2 mixes, while linked (silence when nothing
    // is sent to one).
    for bus in 0..2 {
        let audio = rt.aux_audio(bus, n);
        for side in 0..2 {
            let port = unsafe { *outs_ptr.add(15 + bus * 2 + side) };
            if !port.is_null() {
                // SAFETY: the aux<n>_FL/FR output ports, allocated by the C shim.
                let out = unsafe { std::slice::from_raw_parts_mut(port, n) };
                match audio {
                    Some(a) => out.copy_from_slice(a[side]),
                    None => out.fill(0.0),
                }
            }
        }
    }
    // Latency measurement: the probe replaces the program for a few
    // seconds, recorded back in step with it.
    let (l, r) = unsafe { (*outs_ptr, *outs_ptr.add(1)) };
    let (l, r) = unsafe {
        (
            std::slice::from_raw_parts_mut(l, n),
            std::slice::from_raw_parts_mut(r, n),
        )
    };
    rt.params.latency.process(clock.rate, mic, l, r);
    // Surround and mono outputs, from the finished program, while linked.
    let extra = |i: usize| {
        let p = unsafe { *outs_ptr.add(4 + i) };
        // SAFETY: PipeWire's buffer for this cycle, `n` frames, or null.
        (!p.is_null()).then(|| unsafe { std::slice::from_raw_parts_mut(p, n) })
    };
    let o = crate::surround::Outs {
        fc: extra(0),
        lfe: extra(1),
        sl: extra(2),
        sr: extra(3),
        rl: extra(4),
        rr: extra(5),
        mono: extra(6),
    };
    if o.fc.is_some()
        || o.lfe.is_some()
        || o.sl.is_some()
        || o.sr.is_some()
        || o.rl.is_some()
        || o.rr.is_some()
        || o.mono.is_some()
    {
        let psd = rt.params.surround_psd.load(Ordering::Relaxed);
        rt.surround.run(clock.rate, l, r, o, psd);
    }
}

/// Beat sync of one deck (the follower) to a master deck.
#[derive(Clone, Copy, Debug, Default)]
struct SyncState {
    on: bool,
    master: Option<usize>,
    /// A manual nudge holds phase correction until sync is pressed again.
    phase_hold: bool,
    /// Latest measured phase error, ms (+ = follower behind).
    error_ms: f64,
    /// Hard correction still to do after engaging.
    engage: bool,
    /// Bars were aligned too (both downbeats known).
    bars: bool,
    last_jump: Option<std::time::Instant>,
    /// When the follower's tempo last changed: a phase jump waits until
    /// the new tempo is audible (else it lands off by the drift).
    tempo_at: Option<std::time::Instant>,
    /// Last fader target sent to the deck worker. Tempo maps send updates only
    /// after a small useful change, avoiding a command for every 5 ms poll.
    last_commanded_tempo: Option<f64>,
}

/// Human-reviewed local tempo for a loaded source. Read by the control-plane
/// sync worker only; the audio callback keeps using its bounded deck state.
#[derive(Clone, Copy, Debug)]
struct SyncTempoSegment {
    start_s: f64,
    end_s: f64,
    bpm_start: f64,
    bpm_end: f64,
}

struct Shared {
    controllers: Mutex<patch_controller::Snapshot>,
    controller_reload: std::sync::atomic::AtomicBool,
    /// Which cues each track has, by path. The library's own stamp guards it: any
    /// writer — this daemon, the DJ Booth, the CLI — moves that stamp and the
    /// cache is dropped wholesale, so it can never become a second copy of the cue
    /// table. A stale pad LED is worse than a dark one.
    cue_cache: Mutex<CueCache>,
    /// UI presence expires after a crash or a closed window; audio transport
    /// remains engine-owned and independently keeps a module visible.
    module_leases: Mutex<std::collections::BTreeMap<(String, u32), Instant>>,
    /// The last transition scheduled, in full.
    last_transition: Mutex<Option<Value>>,
    /// The last transition that played (from its start): what a DJ who
    /// heard it reports.
    previous_transition: Mutex<Option<Value>>,
    workspace_control: Mutex<()>,
    /// Pacing changes are consumed by the planner between preparations.
    /// A request is tied to its set, so it cannot affect a newer session.
    fast_mode_request: Mutex<Option<(String, bool)>>,
    #[allow(dead_code)]
    paused_plan: Mutex<Option<(RunningPlan, f64)>>,
    params: Arc<Params>,
    analysis: Arc<Analysis>,
    /// Recent meter blocks with their sequence numbers.
    history: Mutex<VecDeque<(u64, MeterBlock)>>,
    links: Mutex<Links>,
    handle: std::sync::atomic::AtomicPtr<c_void>,
    /// The graph runs for the engine alone (a deck plays): see `idler`.
    always: std::sync::atomic::AtomicBool,
    /// An app has a stream on Patch's sinks (`check_streams`).
    apps: std::sync::atomic::AtomicBool,
    /// Last second's callback cost: (average ns, worst ns).
    cost: Mutex<(u64, u64)>,
    sync: Mutex<[SyncState; crate::MAX_DECKS]>,
    tempo_master: std::sync::atomic::AtomicUsize,
    /// Per-deck reviewed tempo maps, replaced on load and consumed by syncer.
    tempo_maps: Mutex<Vec<Vec<SyncTempoSegment>>>,
    /// The latest output latency measurement.
    measure: Arc<Mutex<Measure>>,
    /// Handing over to a new engine: don't touch the graph any more.
    retired: std::sync::atomic::AtomicBool,
    /// The Plugins Unit's chain on the master.
    plugins: Option<patch_plugin::chain::ChainControl>,
    /// The DJ session (crossfader, faders, auto-gain), owned here.
    session: Arc<Mutex<crate::session::Session>>,
    /// Deck paths the trims were computed for.
    trimmed: Mutex<[String; crate::MAX_DECKS]>,
    /// A transition plan running on the beat clock of its outgoing deck.
    plan: Mutex<Option<RunningPlan>>,
    /// Display lanes: crossfader, faders A/B, then the decks' EQ knobs
    /// (A high, mid, low, filter, then B's): what the knobs show between
    /// a plan's steps, on the samples they sound at.
    visual_moves: Mutex<[VecDeque<VisualMove>; VISUAL_LANES]>,
    /// Auto-mix: what it prepared (deck, path, suggestion) and why it waits.
    auto: Mutex<AutoState>,
    /// The AI's current evaluation (candidates, factors, the DJ's picks).
    eval: Mutex<Eval>,
    /// Every EQ's settings, by target ("master", "bus", "deck-a", …),
    /// persisted in eq.json.
    eq: Mutex<std::collections::BTreeMap<String, patch_core::eq::EqState>>,
    /// Applications with their own EQ: app name → engine channel ("app-1").
    app_eq: Mutex<std::collections::BTreeMap<String, String>>,
    /// Microphone channels in use (`mics.json`).
    mics: Mutex<std::collections::BTreeMap<String, MicSetup>>,
    /// The Pattern Unit's running pattern (`pattern.json`).
    pattern: Arc<crate::pattern_service::Service>,
    /// The Tidal Unit's session (`tidal.json`), played by the instrument.
    tidal: Arc<crate::pattern_service::Service>,
    /// Derived Material: cut, transform and cache material from tracks the
    /// engine already knows (`material` op). Audio and stretching come from
    /// this crate; the recipes are Patch data.
    material: crate::material_service::Material,
    /// What the picture should mean (the AI director, patterns): read by the
    /// Visualiser (`visual_intent` op).
    visual_intent: patch_core::visual_intent::Board,
    visual_scene: Mutex<Value>,
}

/// What the Pattern service reads from and asks of the engine.
impl crate::pattern_service::Host for Shared {
    fn now(&self) -> Option<(i64, u32)> {
        let b = self.history.lock().unwrap().back().map(|(_, b)| b.stamp)?;
        let rate = b.rate.max(1);
        let ahead = crate::now_ns().saturating_sub(b.nsec) as i64;
        Some((
            b.sample_pos as i64 + ahead * rate as i64 / 1_000_000_000,
            rate,
        ))
    }

    fn deck_clock(&self, deck: Option<usize>) -> Option<(i64, f64, f64)> {
        let p = &*self.params;
        let b = *self.history.lock().unwrap().back().map(|(_, b)| b)?;
        let clock = |d: usize| -> Option<(i64, f64, f64)> {
            let beat_len = f64::from_bits(p.grid_beat.get(d)?.load(Ordering::Relaxed));
            let off = f64::from_bits(p.grid_offset[d].load(Ordering::Relaxed));
            let (pos, step) = (b.deck_pos[d], b.deck_step[d] as f64);
            if beat_len <= 0.0 || !pos.is_finite() || step <= 0.0 {
                return None;
            }
            let downbeat = p.decks[d]
                .status
                .lock()
                .unwrap()
                .grid
                .and_then(|g| g.downbeat)
                .unwrap_or(0) as f64;
            let bpm = step * b.stamp.rate as f64 * 60.0 / beat_len;
            Some((
                b.stamp.sample_pos as i64,
                (pos - off) / beat_len - downbeat,
                bpm,
            ))
        };
        match deck {
            Some(d) => clock(d),
            // The lead: the deck the crossfader favours among those playing.
            None => {
                let x = self.session.lock().unwrap().mixer.ai_controls().xfader;
                let order = if x > 0.5 { [1, 0] } else { [0, 1] };
                order.into_iter().find_map(clock)
            }
        }
    }

    fn command(
        &self,
        cmd: patch_core::command::Command,
        origin: patch_core::command::Origin,
    ) -> Result<(), String> {
        handle(
            self,
            &json!({"op": "command", "command": cmd, "origin": origin}),
        )
        .map(|_| ())
    }

    fn voices(&self) -> &patch_pattern::rt::Queue {
        &self.params.sequencer.pattern
    }

    fn sample_slot(
        &self,
        source: &patch_core::material::SampleSource,
        slot: Option<&str>,
        at: i64,
    ) -> Result<u32, String> {
        let (now, rate) =
            crate::pattern_service::Host::now(self).ok_or("the graph isn't running")?;
        self.material
            .slot_for(&self.params.sampler, source, slot, at, now, rate)
    }

    fn stop_samples(&self) {
        self.params.sampler.stop_all();
    }

    fn resolve_material(&self, address: &str) -> Result<patch_core::material::AssetId, String> {
        let deck = patch_material::address::parse(address)?.deck;
        let track = self.params.decks.get(deck).and_then(|d| {
            let st = d.status.lock().unwrap();
            st.loaded.then(|| st.path.clone())
        });
        let rate = crate::pattern_service::Host::now(self).map_or(48_000, |(_, r)| r);
        self.material.resolve(address, track.as_deref(), rate)
    }
}

/// The Tidal Unit's view of the engine: the same clock, decks and
/// commands, and its own queue (the instrument's), so the two pattern
/// services never share a producer.
pub(crate) struct TidalHost<S>(pub S);

impl<S: std::ops::Deref<Target = Shared> + Send + Sync> crate::pattern_service::Host
    for TidalHost<S>
{
    fn now(&self) -> Option<(i64, u32)> {
        crate::pattern_service::Host::now(&*self.0)
    }

    fn deck_clock(&self, deck: Option<usize>) -> Option<(i64, f64, f64)> {
        crate::pattern_service::Host::deck_clock(&*self.0, deck)
    }

    fn command(
        &self,
        cmd: patch_core::command::Command,
        origin: patch_core::command::Origin,
    ) -> Result<(), String> {
        crate::pattern_service::Host::command(&*self.0, cmd, origin)
    }

    fn voices(&self) -> &patch_pattern::rt::Queue {
        &self.0.params.tidal.queue
    }

    fn stop_samples(&self) {
        self.0.params.tidal.release.fetch_add(1, Ordering::Release);
    }

    fn sample(
        &self,
        name: &str,
        n: f64,
    ) -> Option<Result<patch_pattern::instrument::SampleInfo, String>> {
        let ix = patch_pattern::samples::index();
        let path = ix.file(name, n)?;
        Some(self.0.params.tidal.bank.lookup(path))
    }

    fn warm_samples(&self, code: &str, wait: bool) {
        let ix = patch_pattern::samples::refresh();
        let paths: Vec<std::path::PathBuf> = patch_pattern::samples::mentioned(&ix, code)
            .into_iter()
            .filter_map(|n| ix.get(n))
            .flat_map(|f| f.files.iter().cloned())
            .collect();
        let budget = if wait { 1500 } else { 0 };
        self.0
            .params
            .tidal
            .bank
            .warm(&paths, std::time::Duration::from_millis(budget));
    }

    fn samples_status(&self) -> Value {
        let ix = patch_pattern::samples::refresh();
        let folders: Vec<Value> = ix.names().map(|(n, c)| json!([n, c])).collect();
        json!({
            "own": patch_pattern::samples::own_root(),
            "roots": ix.roots(),
            "folders": folders,
            "bank": self.0.params.tidal.bank.status(),
            "missing": self.0.params.tidal.missing.load(Ordering::Relaxed),
        })
    }
}

/// A microphone channel: its capture source and its voice processing.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct MicSetup {
    source: String,
    #[serde(default = "monitor_default")]
    monitor: bool,
    #[serde(default)]
    voice: VoiceSetup,
    #[serde(default)]
    previous_profile: Option<patch_core::mic::Profile>,
    /// In the Patch Input mix (off: mix-minus, for a remote voice).
    #[serde(default = "send_default")]
    input_send: bool,
}

type VoiceSetup = patch_core::mic::Settings;



const VISUAL_LANES: usize = 3 + 8;

/// The display lane of deck `d`'s EQ knob `slot` (0 high … 3 filter).
fn eq_lane(d: usize, slot: usize) -> usize {
    3 + d * 4 + slot
}

/// Display-only ramp on the engine sample clock; never changes audio gains.
#[derive(Clone, Copy)]
struct VisualMove {
    start: u64,
    end: u64,
    from: f32,
    to: f32,
}

fn visual_value(queue: &mut VecDeque<VisualMove>, sample: u64, fallback: f32) -> f32 {
    while queue.len() > 1 && queue.front().is_some_and(|m| m.end < sample) {
        queue.pop_front();
    }
    let Some(m) = queue.front() else {
        return fallback;
    };
    if m.end <= m.start {
        return if sample < m.end { m.from } else { m.to };
    }
    let f =
        (sample.saturating_sub(m.start) as f64 / (m.end - m.start) as f64).clamp(0.0, 1.0) as f32;
    m.from + (m.to - m.from) * f
}

#[cfg(test)]
#[test]
fn display_ramps_follow_samples_and_preserve_hard_cuts() {
    let mut moves = VecDeque::from([
        VisualMove {
            start: 100,
            end: 200,
            from: 0.0,
            to: 0.5,
        },
        VisualMove {
            start: 200,
            end: 300,
            from: 0.5,
            to: 1.0,
        },
    ]);
    assert_eq!(visual_value(&mut moves, 50, 0.9), 0.0);
    assert_eq!(visual_value(&mut moves, 150, 0.9), 0.25);
    assert_eq!(visual_value(&mut moves, 200, 0.9), 0.5);
    assert_eq!(visual_value(&mut moves, 250, 0.9), 0.75);
    assert_eq!(visual_value(&mut moves, 400, 0.9), 1.0);
    moves.push_back(VisualMove {
        start: 500,
        end: 500,
        from: 1.0,
        to: 0.0,
    });
    assert_eq!(visual_value(&mut moves, 499, 0.9), 1.0);
    assert_eq!(visual_value(&mut moves, 500, 0.9), 0.0);
    moves.clear();
    assert_eq!(visual_value(&mut moves, 600, 0.7), 0.7);
}



/// A latency measurement's progress and outcome.
#[derive(Clone, Debug, Default, serde::Serialize)]
struct Measure {
    /// "idle" | "measuring" | "done" | "failed"
    phase: &'static str,
    mic: Option<String>,
    output: Option<String>,
    result: Option<crate::latency::Measured>,
    error: Option<String>,
}

const HISTORY: usize = 1024;
const MAX_REPLY_BLOCKS: usize = 256;

fn collector(s: Arc<Shared>) {
    let mut seq = 0u64;
    let mut tick = 0u32;
    // The meters for the UI, in shared memory (the socket's "meters" stays
    // for other readers and older UIs).
    let mut shm = crate::shm::Writer::create();
    let mut now = crate::shm::Now::empty();
    let mut idle_ticks = 0u32;
    loop {
        // A graph block arrives about every 21 ms at the usual quantum.
        // Drain it promptly so meters and sync do not wait another 10 ms.
        // With no blocks for a second (nothing plays, the graph sleeps):
        // 100 ms, a twentieth of the wake-ups; the first block brings it back.
        std::thread::sleep(Duration::from_millis(if idle_ticks > 200 {
            100
        } else {
            5
        }));
        tick += 1;
        if tick.is_multiple_of(200) {
            *s.cost.lock().unwrap() = s.params.take_cost();
        }
        let (channels, decks) = (s.params.channels(), s.params.decks.len());
        let mut h = s.history.lock().unwrap();
        let before = seq;
        // SAFETY: this collector thread is the meter ring's only consumer.
        unsafe {
            s.params.meters.drain(|b| {
                seq += 1;
                if let Some(w) = shm.as_mut() {
                    w.block(&b, channels, decks);
                }
                h.push_back((seq, b));
            })
        };
        while h.len() > HISTORY {
            h.pop_front();
        }
        drop(h);
        idle_ticks = if seq == before {
            idle_ticks.saturating_add(1)
        } else {
            0
        };
        // Publish the reactive display spectrum at collector cadence. Holding
        // it for four ticks adds up to 20 ms after the analysis worker.
        if shm.is_some() {
            let a = s.analysis.snapshot.lock().unwrap().clone();
            now.vu = a.vu;
            now.loudness = [
                a.momentary,
                a.short_term,
                a.integrated,
                a.range,
                a.true_peak_max,
            ];
            let n = a.spectrum.len().min(crate::shm::BANDS);
            now.bands = n as u32;
            now.spectrum[..n].copy_from_slice(&a.spectrum[..n]);
            now.set_names(&s.params.names);
        }
        // Mixer telemetry follows the 5 ms collector, independent of analysis.
        if let Some(w) = shm.as_mut() {
            {
                let ses = s.session.lock().unwrap();
                now.xfader = ses.mixer.xfader;
                now.faders = ses.mixer.faders4();
                now.eq = [ses.eq[0], ses.eq[1], ses.extra_eq[0], ses.extra_eq[1]];
            }
            for d in 0..crate::MAX_DECKS {
                (now.stems[d], now.stem_levels[d]) = s.params.decks[d]
                    .stems
                    .display()
                    .unwrap_or(([f32::NAN; 4], [0.0; 4]));
            }
            if let Some(sample) = s
                .history
                .lock()
                .unwrap()
                .back()
                .map(|(_, b)| b.stamp.sample_pos)
            {
                let mut moves = s.visual_moves.lock().unwrap();
                now.xfader = visual_value(&mut moves[0], sample, now.xfader);
                now.faders[0] = visual_value(&mut moves[1], sample, now.faders[0]);
                now.faders[1] = visual_value(&mut moves[2], sample, now.faders[1]);
                for d in 0..2 {
                    for slot in 0..4 {
                        now.eq[d][slot] =
                            visual_value(&mut moves[eq_lane(d, slot)], sample, now.eq[d][slot]);
                    }
                }
            }
            w.now(&now);
            w.beat();
        }
    }
}

#[path = "transition_adjust.rs"]
mod transition_adjust;

// Controller transport and mapping belong to the independent Unit. The
// service adapter alone knows validation, DJ state and command execution.
fn controllers(s: Arc<Shared>) {
    let mut unit = patch_controller::Controller::default();
    let mut published = std::time::Instant::now();
    let mut last_feedback = std::time::Instant::now();
    let started = std::time::Instant::now();
    loop {
        if s.handle.load(Ordering::Acquire).is_null() {
            return;
        }
        if s.retired.load(Ordering::Acquire) {
            unit.suspend();
            *s.controllers.lock().unwrap() = unit.snapshot();
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if s.controller_reload.swap(false, Ordering::AcqRel) {
            unit.reload();
        }
        // No controller open: nothing to read or light, so the loop rests
        // (it still scans for one every 2 s). With one, 5 ms keeps a jog
        // wheel and the lights immediate.
        let devices = unit.has_devices();
        unit.tick(if devices { 5 } else { 50 }, &mut |command| {
            if s.retired.load(Ordering::Acquire) {
                return Err("engine handover".into());
            }
            handle(
                &s,
                &json!({"op":"command", "command":command, "origin":"user"}),
            )
            .map(|_| ())
        });
        // Feedback runs here, on the control-plane thread: the state is the
        // engine's own, so a light follows a deck whichever source moved it.
        if devices && last_feedback.elapsed() >= Duration::from_millis(30) {
            let state = controller_state(&s);
            unit.feedback(&state, started.elapsed().as_millis() as u64);
            last_feedback = std::time::Instant::now();
        }
        if published.elapsed() >= Duration::from_millis(100) {
            *s.controllers.lock().unwrap() = unit.snapshot();
            published = std::time::Instant::now();
        }
    }
}

/// Cue flags by track path, with the library's modification time beside them.
#[derive(Default)]
struct CueCache {
    /// The library file's stamp when this cache was filled. The moment it differs,
    /// everything here is suspect and goes.
    stamp: Option<std::time::SystemTime>,
    by_path: std::collections::HashMap<String, [bool; HOT_CUES]>,
}

/// The whole of the cue cache's rule, with the library read passed in.
///
/// A stamp that differs from the cache's means the library was written by someone
/// — this daemon, the DJ Booth, the CLI — so everything read before it is suspect
/// and goes. A path already read under the same stamp is served from the cache,
/// which is what keeps this off the 30 ms poll. Nothing loaded never reads.
fn cached_cue_flags(
    cache: &mut CueCache,
    stamp: Option<std::time::SystemTime>,
    path: &str,
    read: impl FnOnce(&str) -> [bool; HOT_CUES],
) -> [bool; HOT_CUES] {
    if path.is_empty() {
        return [false; HOT_CUES];
    }
    if cache.stamp != stamp {
        cache.by_path.clear();
        cache.stamp = stamp;
    }
    if let Some(flags) = cache.by_path.get(path) {
        return *flags;
    }
    let flags = read(path);
    // Bounded, like every other store here: the cache holds what is being played,
    // not the branch's whole history.
    if cache.by_path.len() >= 32 {
        cache.by_path.clear();
    }
    cache.by_path.insert(path.to_string(), flags);
    flags
}

/// Which of `path`'s cues are set, as a controller's pads follow them.
///
/// The library is the one store the booth and this daemon both write, so this
/// reads it — but not on every poll: the flags are kept until the library's stamp
/// changes.
fn cue_flags(s: &Shared, path: &str) -> [bool; HOT_CUES] {
    let stamp = patch_library::stamp();
    let mut cache = s.cue_cache.lock().unwrap();
    cached_cue_flags(&mut cache, stamp, path, read_cue_flags)
}

/// A track's cue flags, read from the library.
///
/// A track the library does not know, or a library that cannot be read, yields
/// "nothing set": a dark pad is honest and a lit wrong one is not.
fn read_cue_flags(path: &str) -> [bool; HOT_CUES] {
    let mut flags = [false; HOT_CUES];
    if let Ok(db) = patch_library::open_quick()
        && let Some(track) = db.track_by_path(path)
    {
        for (i, cue) in db
            .performance(track)
            .hot_cues
            .into_iter()
            .take(HOT_CUES)
            .enumerate()
        {
            flags[i] = cue.is_some();
        }
    }
    flags
}

#[cfg(test)]
#[test]
fn the_cue_cache_serves_what_it_read_and_drops_everything_when_the_library_moves() {
    use std::cell::Cell;
    let mut cache = CueCache::default();
    let t1 = std::time::SystemTime::UNIX_EPOCH;
    let t2 = t1 + std::time::Duration::from_secs(1);
    let reads = Cell::new(0);
    let read = |_p: &str| {
        reads.set(reads.get() + 1);
        [true; HOT_CUES]
    };
    // The first look reads the library.
    assert!(cached_cue_flags(&mut cache, Some(t1), "/a.wav", read)[0]);
    assert_eq!(reads.get(), 1);
    // The same library serves the cached flags: the 30 ms poll does not read it.
    let _ = cached_cue_flags(&mut cache, Some(t1), "/a.wav", read);
    assert_eq!(reads.get(), 1, "an unchanged stamp reads nothing again");
    // A writer moved the library, so everything already read is suspect — this is
    // the case that would otherwise light a pad from a cue that has been cleared.
    let fresh = cached_cue_flags(&mut cache, Some(t2), "/a.wav", |_p| {
        reads.set(reads.get() + 1);
        [false; HOT_CUES]
    });
    assert_eq!(reads.get(), 2, "a moved stamp reads again");
    assert!(!fresh[0], "and the new reading is what the pad follows");
    // Nothing loaded is never worth a read.
    let none = cached_cue_flags(&mut cache, Some(t2), "", read);
    assert_eq!(reads.get(), 2, "an empty path reads nothing");
    assert_eq!(none, [false; HOT_CUES]);
    // A library that cannot be read is not cached as if it were: the stamp is the
    // only thing that says a reading is current.
    for i in 0..40 {
        let _ = cached_cue_flags(&mut cache, Some(t2), &format!("/t{i}.wav"), read);
    }
    assert!(
        cache.by_path.len() <= 32,
        "the cache stays bounded: {}",
        cache.by_path.len()
    );
}

/// The decks the feedback snapshot can publish, in deck-letter order. The engine
/// labels deck `i` as letter `'a' + i`, so this is positional, and a graph with
/// fewer decks publishes fewer entries rather than claiming decks it has not got.
const FEEDBACK_DECKS: [&str; 4] = ["a", "b", "c", "d"];

/// The deck letters to publish: one per deck the engine actually runs, so a
/// controller that asks about deck C is never an index into a deck that is absent.
fn feedback_decks(params: &Params) -> &'static [&'static str] {
    &FEEDBACK_DECKS[..params.decks.len().min(FEEDBACK_DECKS.len())]
}

/// One deck's contribution to the snapshot. Separate from the loop so a test can
/// ask what deck C reports without building a whole engine.
fn deck_state(
    params: &Params,
    letter: &str,
    index: usize,
    fader: f32,
    vu: f32,
    cues: [bool; HOT_CUES],
) -> patch_controller::DeckState {
    let st = deck_status(params, index);
    let channel = params.channel(&format!("deck-{letter}"));
    let duration = st["duration"].as_f64().unwrap_or(0.0);
    let position = st["position"].as_f64().unwrap_or(0.0);
    // The main cue and the loop live on the deck's own transport, so they are read
    // from there rather than kept a second time. A loop's *length in beats* is not
    // stored at all — Patch keeps its sample range — so it is derived against the
    // grid, which is the same tempo the loop was set from.
    let (cue_set, loop_beats) = {
        let deck = &params.decks[index];
        let status = deck.status.lock().unwrap();
        // A deck starts with its cue at the track's beginning, so a cue that is
        // *set* is one placed away from it.
        let cue_set = status.cue_point.is_some();
        // The status carries the loop's points in seconds, so its length in beats is
        // its span against the grid's tempo and nothing else.
        let loop_beats = if !status.loop_on {
            None
        } else {
            match (status.loop_in, status.loop_out) {
                (Some(a), Some(b)) => {
                    let bpm = st["grid"]["bpm"].as_f64().unwrap_or(0.0);
                    (bpm > 0.0).then(|| ((b - a).abs() * bpm / 60.0) as f32)
                }
                _ => None,
            }
        };
        (cue_set, loop_beats)
    };
    // A band is killed when its own gain is null — the same state `dj.eq` sets when
    // it is given no dB, read back through the one accessor that already does this.
    let killed = match channel {
        Some(c) => {
            let eq = eq_json(params, c);
            [
                eq["low_db"].is_null(),
                eq["mid_db"].is_null(),
                eq["high_db"].is_null(),
            ]
        }
        None => [false; 3],
    };
    patch_controller::DeckState {
        fader,
        loaded: st["loaded"].as_bool().unwrap_or(false),
        playing: st["playing"].as_bool().unwrap_or(false),
        pfl: channel.is_some_and(|c| params.pfl_of(c)),
        loop_on: st["loop_on"].as_bool().unwrap_or(false),
        keylock: st["keylock"].as_bool().unwrap_or(false),
        slip: st["slip"].as_bool().unwrap_or(false),
        quantize: st["quantize"].as_bool().unwrap_or(false),
        // Percent, as the tempo fader commands it: an output maps it with its own
        // `range` (e.g. [-16, 16]).
        tempo: st["tempo"].as_f64().unwrap_or(0.0) as f32,
        progress: if duration > 0.0 {
            (position / duration).clamp(0.0, 1.0) as f32
        } else {
            0.0
        },
        vu,
        cues,
        cue_set,
        loop_beats,
        killed,
    }
}

/// One deck's snapshot comes from its own channel, the letter list covers exactly
/// the decks the graph runs, and a deck with nothing on it reports nothing.
#[cfg(test)]
#[test]
fn the_snapshot_covers_every_deck_the_engine_runs() {
    let four = Params::new(&["bus", "deck-a", "deck-b", "deck-c", "deck-d"]);
    assert_eq!(feedback_decks(&four), ["a", "b", "c", "d"]);
    // Deck C reads its own channel, not deck A's.
    let c = deck_state(&four, "c", 2, 0.2, 0.25, [false; HOT_CUES]);
    assert_eq!(c.fader, 0.2);
    assert_eq!(c.vu, 0.25);
    assert!(!c.loaded, "a deck with nothing on it says so");
    assert!(!c.playing && !c.pfl);
    // The cue flags are the deck's own, and a cue that is not set reads empty.
    let mut cues = [false; HOT_CUES];
    cues[1] = true;
    let c = deck_state(&four, "c", 2, 0.0, 0.0, cues);
    assert!(c.cues[1] && !c.cues[0]);
    // A deck with nothing loaded has no cue, no loop and no kills — which is what
    // every indicator bound to them should read.
    assert!(!c.cue_set);
    assert_eq!(c.loop_beats, None);
    assert_eq!(c.killed, [false; 3]);
    // A two-deck graph publishes two entries: asking about deck C then resolves
    // to nothing rather than indexing a deck that is not there.
    let two = Params::new(&["bus", "deck-a", "deck-b"]);
    assert_eq!(feedback_decks(&two), ["a", "b"]);
}

/// The authoritative state a controller's indicators follow: the DJ session's
/// crossfader and faders, each deck's own status, and the meter ring that the
/// Mixer, DJ Booth and remote pages all read.
///
/// Every field is read from where it already lives — with one deliberate exception,
/// the hot-cue flags, which come from a cache the library's own stamp guards (see
/// `cue_flags`). That is because the cue table lives in the library rather than on
/// the deck, and the stamp is what keeps a pad from following a cue that has been
/// cleared.
fn controller_state(s: &Shared) -> patch_controller::ControllerState {
    use patch_controller::ControllerState;
    let (crossfader, faders) = {
        let session = s.session.lock().unwrap();
        (session.mixer.xfader, session.mixer.faders4())
    };
    // The deck channel levels and the DJ pre-master sum, as the last engine
    // cycle measured them.
    let (deck_abs, master_abs) = {
        let history = s.history.lock().unwrap();
        match history.back() {
            Some((_, block)) => {
                let deck_abs = FEEDBACK_DECKS.map(|letter| {
                    s.params
                        .channel(&format!("deck-{letter}"))
                        .map(|c| (block.ch_abs[c][0] + block.ch_abs[c][1]) / 2.0)
                        .unwrap_or(0.0) as f32
                });
                (deck_abs, (block.dj_abs[0] + block.dj_abs[1]) / 2.0)
            }
            None => ([0.0; FEEDBACK_DECKS.len()], 0.0),
        }
    };
    let letters = feedback_decks(&s.params);
    let mut decks = Vec::with_capacity(letters.len());
    for (i, letter) in letters.iter().enumerate() {
        // The path comes from the same status the rest of the snapshot reads, so
        // the cues belong to the track this deck is actually on.
        let path = deck_status(&s.params, i)["path"]
            .as_str()
            .unwrap_or("")
            .to_string();
        decks.push(deck_state(
            &s.params,
            letter,
            i,
            faders.get(i).copied().unwrap_or(0.0),
            deck_abs[i].clamp(0.0, 1.0),
            cue_flags(s, &path),
        ));
    }
    ControllerState {
        crossfader,
        master_vu: master_abs.clamp(0.0, 1.0),
        decks,
    }
}

fn op_plugins(s: &Shared, req: &Value) -> Result<Value, String> {
    let plugins = s.plugins.as_ref().ok_or("the plugin chain did not start")?;
    match req["action"].as_str().unwrap_or("status") {
        "sync" => {
            let rack = patch_plugin::load_or_default().map_err(|e| format!("{e:#}"))?;
            plugins.sync(&rack);
            Ok(json!("ok"))
        }
        "set" => {
            let field = |k: &str| {
                req[k]
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or(format!("{k}?"))
            };
            let value = req["value"].as_f64().ok_or("value?")?;
            if plugins.set_parameter(field("slot")?, field("id")?, value) {
                Ok(json!("ok"))
            } else {
                Err("value must be within 0..1".into())
            }
        }
        "status" => serde_json::to_value(plugins.status()).map_err(|e| e.to_string()),
        other => Err(format!("plugins: no action {other}")),
    }
}

/// A module that needs a key (modules.json `licences`): without the key the
/// engine starts no AI DJ work (stems are free). Status reads stay open (windows keep
/// drawing), and what already runs is never stopped mid-set: a lapsed
/// key only refuses the next start.
fn paid_gate(req: &Value) -> Result<(), String> {
    let module = match req["op"].as_str().unwrap_or("") {
        "set_start" | "ai_approve" | "transition_now" => "ai_dj",
        "automix" if req["on"] == true => "ai_dj",
        "ai_mode" if req["mode"].as_str().is_some_and(|m| m != "observe") => "ai_dj",
        _ => return Ok(()),
    };
    // The open source build has no planner (a stand-in patch-ai).
    if !patch_ai::AVAILABLE {
        return Err(patch_ai::NOT_IN_THIS_BUILD.into());
    }
    if patch_core::licence::module_unlocked(module) {
        return Ok(());
    }
    let c = patch_core::modules::builtin();
    let Some(l) = patch_core::licence::for_module(c, module) else {
        return Err("Auto Mix isn't available here".into());
    };
    Err(l.refusal())
}

fn handle(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
    // Check and hold this lane's ownership through the atomic control update.
    // This does not take the graph keeper's I/O lock or any RT-thread lock.
    let _stream_owner = if let Some(key) = req.get("stream_key") {
        if !matches!(
            req["op"].as_str(),
            Some("gain" | "mute" | "solo" | "pan" | "trim" | "eq" | "pfl" | "send" | "fx")
        ) {
            return Err("stream ownership is valid only for channel controls".into());
        }
        let c = channel(p, req)?;
        let owner = p.stream_owner(c, key.as_str().ok_or("stream_key must be a string")?)?;
        // The user adjusted this caller: its strip stays in the Mixer.
        p.touch_stream(c);
        Some(owner)
    } else {
        None
    };
    paid_gate(req)?;
    match req["op"].as_str().unwrap_or("") {
        "controllers" => Ok(json!(*s.controllers.lock().unwrap())),
        "controllers_reload" => {
            s.controller_reload.store(true, Ordering::Release);
            Ok(json!("reload requested"))
        }

        // The Pattern Unit: {"op":"pattern","action":"load"|"play"|…}.
        "pattern" => op_pattern(s, req),
        // The Tidal Unit: {"op":"tidal","action":"eval"|"status"|"stop"|…}.
        "tidal" => op_tidal(s, req),
        // Derived Material: {"op":"material","action":"request"|"query"|…}.
        "material" => s.material.request(req),
        // {"op":"visual_intent","action":"get"|"set"|"clear","intent":{…}}.
        "visual_intent" => op_visual_intent(s, req),
        "visual_scene_cue" => op_visual_scene_cue(s, req),
        "sequencer" => op_sequencer(s, req),
        "sequencer-sync" => op_sequencer_sync(s, req),
        "ping" => Ok(json!("pong")),
        "status" => op_status(s, req),
        // {"op":"report","why"?:…}: the DJ heard the last transition go
        // wrong. Everything about it goes to a file to work with (replay it:
        // omarchy-patch dj rehearse-report <file>).
        "report" => op_report(s, req),
        "meters" => op_meters(s, req),
        "gain" => {
            let c = channel(p, req)?;
            p.set_gain(c, db_to_linear(number(req, "db")?.clamp(-120.0, 12.0)));
            Ok(json!("ok"))
        }
        "mute" => op_mute(s, req),
        // {"op":"solo","channel":"mic-1"|"dj","solo":true}
        "solo" => mixer_ops::op_solo(s, req),
        "pan" => {
            let c = channel(p, req)?;
            p.set_pan(c, number(req, "pan")?);
            Ok(json!("ok"))
        }
        // A send to a bus: {"op":"send","channel":"mic-1"|"dj","bus":"aux1"|"aux2"|"fx1"|"fx2",
        // "db":-6 (null: off),"pre":true}. Either of db / pre may be left out.
        "send" => mixer_ops::op_send(s, req),
        // A bus's master (AUX) or return (FX): {"op":"bus","bus":"fx1","db":-3,"mute":false}
        "bus" => mixer_ops::op_bus(s, req),
        // The returns' effects: {"op":"aux_fx","reverb_size":0.5,"delay_ms":375,"delay_feedback":0.35}
        "aux_fx" => mixer_ops::op_aux_fx(s, req),
        // Input trim (the Mixer's GAIN knob): {"op":"trim","channel":"mic-1","db":6}
        "trim" => {
            let c = channel(p, req)?;
            let db = number(req, "db")?.clamp(-24.0, 24.0);
            p.set_trim(c, db_to_linear(db));
            if let Some(i) = ["deck-a", "deck-b", "deck-c", "deck-d"].iter().position(|name| *name == p.names[c]) {
                let mut session = s.session.lock().unwrap();
                session.input_trim_db[i] = db;
                session.save();
            }
            Ok(json!("ok"))
        }
        // Partial updates: {"op":"eq","channel":"deck-a","low_db":-12,"filter":-0.3}
        // (a band's dB may be null: kill).
        "eq" => op_eq(s, req),
        // A nudge by hand holds sync's phase correction on that deck.
        // The master clock: the latest engine cycle's stamp, and now.
        "clock" => op_clock(s, req),
        "deck" if req["do"] == "bend" => {
            if let Ok(i) = deck_index(p, req) {
                let mut all = s.sync.lock().unwrap();
                if all[i].on && req["value"].as_f64().is_some_and(|v| v != 0.0) {
                    all[i].phase_hold = true;
                }
            }
            deck_op(p, req)
        }
        "deck" => op_deck(s, req),
        // {"op":"sync","deck":"b","on":true,"master":"a"?}
        "sync" => op_sync(s, req),
        "sync_master" => dj_ops::op_sync_master(s, req),
        "deck_status" => op_deck_status(s, req),
        // A deck's sample maker: {"op":"deck_sample","deck":"a","action":
        // "status"|"save","what"?:"loop"|"beats","beats"?,"from"?,"slices"?,"name"?}
        "deck_sample" => op_deck_sample(s, req),
        // The DJ mixer (session-owned): {"op":"dj","xfader"?,"fader_a"?,
        // "fader_b"?,"auto_gain"?} sets any of them and returns the state.
        "dj" => op_dj(s, req),
        // A validated command, executed here: {"op":"command","command":
        // {…command JSON…},"origin":"user"|"cli"|"ai"}.
        "command" => op_command(s, req),
        // {"op":"plan","plan":{TransitionPlan},"level":"execute","origin":"user"}
        "plan" => op_plan(s, req),
        // The queue: {"op":"queue","add":path|[paths],"remove":i,"move":[i,j],"clear":true}
        "queue" => op_queue(s, req),
        // {"op":"automix","on"?:bool,"direction"?:"build"|"hold"|"release"}
        "automix" => op_automix(s, req),
        // {"op":"set_start","minutes"?:60,"arc"?:"journey","bpm"?:[lo,hi]}
        "link_take" => op_link_take(s, req),
        "stems"=>crate::stem_service::handle(&s.params,req),
        "performance" => performance_stage(s.params.clone(), s.session.clone(), req),
        "workspace" => op_workspace(s, req),
        "set_start" => {
            let _control = s.workspace_control.lock().unwrap();
            set_start(s, req)
        },
        // {"op":"set_option","drum_swaps":true}
        "set_option" => op_set_option(s, req),
        "set_stop" => op_set_stop(s, req),
        // {"op":"say","text":"keep the energy up"}: words → intents.
        "say" => say(s, req["text"].as_str().unwrap_or("")),
        // The AI DJ's visual state (all AI DJ views render this).
        "ai_state" => Ok(serde_json::to_value(ai_state(s)).unwrap_or(Value::Null)),
        // The last transition that played, for the AI DJ window's report.
        // The transition planned next (waiting for its bar, running, or
        // waiting for the DJ's approval): from, to and how; Null when none.
        "coming" => op_coming(s, req),
        "previous" => op_previous(s, req),
        // An EQ: {"op":"master_eq","target"?:"master|bus|deck-a|deck-b"}
        // reads it; with "state":{…} it replaces it (normalised, clamped).
        // {"op":"master_eq","all":true} reads every target.
        // Applications with their own EQ, and the ones playing now (to add).
        "eq_apps" => op_eq_apps(s, req),
        // The presets: built-in first, then saved ones.
        "eq_presets" => Ok(Value::Array(
            all_presets()
                .into_iter()
                .map(|(p, builtin)| json!({"id": p.id, "name": p.name, "description": p.description, "builtin": builtin, "bands": p.bands.len()}))
                .collect(),
        )),
        "master_eq" => op_master_eq(s, req),
        // The Plugins Unit: {"op":"plugins","action":"sync"} runs the saved
        // rack now; "set" turns one control live ({"slot","id","value"});
        // "status" lists the running plugins with their levels.
        "plugins" => op_plugins(s, req),
        // {"op":"ai_mode","mode":"observe|suggest|prepare|act"}
        // Settings a DJ switches from the controller: {"name": …, "on": …}
        // sets one; without "on" it only reads.
        // The setup changed (omarchy-patch setup, the controller): the
        // keeper looks again now (system audio on or off).
        "setup" => {
            crate::events::kick();
            Ok(json!({}))
        }
        "setting" => op_setting(s, req),
        "levels" => op_levels(s, req),
        "mix_style" => op_mix_style(s, req),
        "tempo_plan" => op_tempo_plan(s, req),
        "stem_coverage" => op_stem_coverage(s, req),
        "set_prep_stems" => op_set_prep_stems(s, req),
        "ai_mode" => op_ai_mode(s, req),
        // The DJ's choices among candidates: lock one (it is prepared next),
        // reject one (it leaves the set of possibilities).
        "ai_select" | "ai_reject" => op_ai_select_or_ai_reject(s, req),
        // Prepare mode: run the prepared transition (on its bar).
        "ai_approve" => op_ai_approve(s, req),
        // Cancel what's prepared or running; the AI goes back to suggesting.
        "ai_cancel" => op_ai_cancel(s, req),
        // Mix into the next track at the next bar (prepares it first).
        "transition_now" => transition_now(s),
        // Adjust an AI DJ transition that has not started yet.
        "plan_adjust" => transition_adjust::adjust(
            s,
            req["direction"].as_str().ok_or("which adjustment?")?,
        ),
        "plan_adjust_status" => Ok(transition_adjust::status()),
        // The running transition alone (what "dj" returns as "plan"): the AI
        // DJ window reads it up to 20 times a second, and the whole mixer
        // state with every deck's status cost the engine 2 ms a read.
        "plan_state" => Ok(plan_json(s)),
        "plan_cancel" => Ok(json!({"cancelled": drop_plan(s, "cancelled")})),
        // {"op":"fx","channel":"deck-a","kind"?|"chain"?,"on"?,"beats"?,"depth"?}
        // `kind` sets one effect; `chain` (1–3 names) sets a chain.
        "fx" => op_fx(s, req),
        // Route one isolated stem cleanly to the other deck. Beat FX are an
        // explicit optional layer on that send, never a requirement.
        "stem_bridge" => handle_stem_bridge(p, req),
        "mic_history" => op_mic_history(s, req),
        "mic_profiles" => Ok(json!(crate::mic_profiles::list()?)),
        "mic_profile" => op_mic_profile(s, req),
        // Capture sources to choose a microphone from.
        "sources" => Ok(json!(capture_sources())),
        // The microphones with their sources' names, and the sources.
        "mics" => Ok(json!({"mics": mics_json(s), "sources": capture_sources(), "remotes": remote_sources(s)})),
        "mic_send" => op_mic_send(s, req),
        "mic_remote" => op_mic_remote(s, req),
        // {"op":"mic","channel":"mic-1","source":"alsa_input…"} puts a mic on
        // a channel (muted at first: speakers would feed it back; the DJ
        // turns it up with headphones on); "source": null takes it off.
        "mic" => op_mic(s, req),
        // {"op":"voice","channel":"mic-1","on"?,"hpf"?,"gate"?(dB or null),
        // "deess"?,"comp"?}: a mic's voice processing.
        "voice" => op_voice(s, req),
        // The DJ Booth on the main mix: {"op":"dj_group","gain_db"?,"mute"?}.
        "dj_group" => op_dj_group(s, req),
        "pfl" => {
            let c = channel(p, req)?;
            p.set_pfl(c, req["on"].as_bool().ok_or("'on' must be true or false")?);
            Ok(json!("ok"))
        }
        // Headphones: {"op":"cue","mix":0.3,"level_db":-6}
        "cue" => op_cue(s, req),
        // A deck's stream player needs the decks' soundcards: made now
        // (the minimal service starts without them).
        // {"on": false} releases them (nothing plays into them any more).
        "deck_sinks" => {
            let mut l = s.links.lock().unwrap();
            l.decks_wanted = Some(req["on"].as_bool() != Some(false));
            l.tick();
            Ok(json!("ok"))
        }
        // The DJ mixer's input selector: {"op":"deck_line","deck":"a",
        // "app":"Chromium"} puts that application on the channel (LINE);
        // "app": null puts the channel back on its deck.
        "deck_line" => {
            let deck = req["deck"].as_str().ok_or("which deck?")?;
            let deck = if deck.starts_with("deck-") { deck.to_string() } else { format!("deck-{}", deck.to_lowercase()) };
            let mut l = s.links.lock().unwrap();
            l.set_deck_line(&deck, req["app"].as_str().map(String::from))?;
            l.tick();
            Ok(json!({"lines": l.deck_lines, "line_sources": l.line_sources}))
        }
        "cue_output" => {
            let mut l = s.links.lock().unwrap();
            l.set_cue(req["sink"].as_str().map(String::from));
            l.tick();
            Ok(json!("ok"))
        }
        // {"op":"master","db"?,"mute"?}: the Patch mix's fader and mute.
        "master" => op_master(s, req),
        "output" => {
            let mut l = s.links.lock().unwrap();
            l.set_output(req["sink"].as_str().map(String::from))?;
            l.tick();
            Ok(json!("ok"))
        }
        "mic_monitor" => op_mic_monitor(s, req),
        "input_select" => select_input(s, req["source"].as_str().ok_or("no input source")?),
        "input_mix" => op_input_mix(s, req),
        "module_presence" => op_module_presence(s, req),
        "output_route" => op_output_route(s, req),
        // AUX 1 or 2 to its virtual source only: {"op":"output_route_clear","bus":"aux1"}
        "output_route_clear" => {
            let bus = req["bus"].as_str().ok_or("no output bus")?;
            s.links.lock().unwrap().clear_aux(bus)?;
            Ok(json!("ok"))
        }
        "output_select" => op_output_select(s, req),
        // {"op":"latency"} progress; {"op":"latency","do":"measure","mic"?}
        // starts one; {"op":"latency","do":"forget"} drops the output's.
        "latency" => op_latency(s, req),
        "analyzer" => op_analyzer(s, req),
        "reset_loudness" => {
            s.analysis.reset_loudness();
            Ok(json!("ok"))
        }
        // Handover (see client::upgrade): step aside for a new engine,
        // still playing, or come back if the new one failed.
        "retire" => op_retire(s, req),
        "unretire" => op_unretire(s, req),
        // {"op":"fade","db":…,"ms":…}: master to `db`, linearly in gain.
        "fade" => op_fade(s, req),
        "handover_complete" => {
            s.links.lock().unwrap().streams_enabled = true;
            crate::events::kick();
            Ok(json!("ok"))
        }
        "quit" => op_quit(s, req),
        op => Err(format!("unknown op '{op}'")),
    }
}

/// Run the engine until `quit`. Blocks.
pub fn run(names: &[&str]) -> Result<(), String> {
    let path = socket_path();
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    if UnixStream::connect(&path).is_ok() {
        return Err("the engine is already running".into());
    }
    // Mid-handover only the chosen successor starts (a supervisor that
    // saw the socket move must not start a rival).
    if std::env::var_os("PATCH_ENGINE_SILENT").is_none()
        && UnixStream::connect(retiring_socket_path()).is_ok()
    {
        return Err("an engine handover is in progress".into());
    }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).map_err(|e| e.to_string())?;
    patch_core::ipc::private_socket(&path);

    // The output keeps the graph clock (a WirePlumber rule; see
    // output_clock.rs): before the engine links anything.
    crate::output_clock::ensure();
    // The kernel table: built here, before the real-time thread exists.
    patch_dsp::registry::init();
    let params = Arc::new(Params::new(names));
    // A successor in a handover starts silent and fades in.
    if std::env::var_os("PATCH_ENGINE_SILENT").is_some() {
        params.set_master(0.0);
    }
    let mut rt = Box::new(Rt::new(params.clone()));
    // The rack's active plugins run in a plugin host process and join the
    // master with a one-block crossfade: audio never waits for them, and a
    // plugin's crash never reaches the engine.
    let plugins = rt.take_plugin_control();
    if let Some(plugins) = &plugins {
        match patch_plugin::load_config() {
            Ok(Some(rack)) => plugins.sync(&rack),
            Ok(None) => {}
            Err(error) => eprintln!("Plugins Unit: could not read rack: {error:#}"),
        }
    }
    let rt = Box::into_raw(rt);
    let cnames = CString::new(params.names.join(",")).map_err(|e| e.to_string())?;
    // SAFETY: `rt` lives until after patch_engine_run returns.
    let h = unsafe { patch_engine_new(cnames.as_ptr(), process, rt as *mut c_void) };
    if h.is_null() {
        drop(unsafe { Box::from_raw(rt) });
        return Err("could not create the PipeWire filter".into());
    }
    // The system volume straight from PipeWire (the volume thread falls
    // back to pactl until the first report, or without it).
    let sink = CString::new(crate::links::BUS).map_err(|e| e.to_string())?;
    // SAFETY: before patch_engine_run, on its thread; the Params outlive it.
    if unsafe {
        patch_engine_watch_volume(
            h,
            sink.as_ptr(),
            bus_volume_changed,
            Arc::into_raw(params.clone()) as *mut c_void,
        )
    } != 0
    {
        eprintln!("patch engine: no volume listener (system volume via pactl)");
    }
    for (i, d) in params.decks.iter().enumerate() {
        d.spawn(&((b'a' + i as u8) as char).to_string());
    }
    let shared = Arc::new(Shared {
        controllers: Mutex::new(Default::default()),
        controller_reload: std::sync::atomic::AtomicBool::new(false),
        cue_cache: Mutex::new(CueCache::default()),
        module_leases: Mutex::new(Default::default()),
        last_transition: Mutex::new(None),
        previous_transition: Mutex::new(None),
        workspace_control: Mutex::new(()),
        fast_mode_request: Mutex::new(None),
        paused_plan: Mutex::new(None),
        analysis: analysis::spawn(params.clone()),
        links: Mutex::new({
            let mut l = Links::new(&params.names);
            l.psd = params.surround_psd.clone();
            l.params = Some(params.clone());
            for (key, channel) in &l.stream_lanes.assigned {
                if let Some(c) = params.channel(channel) {
                    *params.stream_owners[c].lock().unwrap() = Some(key.clone());
                    if l.stream_lanes.heard.contains(key) {
                        params.restore_stream_signal(c);
                    }
                    if l.stream_lanes.touched.contains(key) {
                        params.touch_stream(c);
                    }
                }
            }
            l
        }),
        params,
        history: Mutex::new(VecDeque::with_capacity(HISTORY)),
        handle: std::sync::atomic::AtomicPtr::new(h),
        always: std::sync::atomic::AtomicBool::new(true),
        apps: std::sync::atomic::AtomicBool::new(false),
        cost: Mutex::new((0, 0)),
        sync: Mutex::new([SyncState::default(); crate::MAX_DECKS]),
        tempo_master: std::sync::atomic::AtomicUsize::new(0),
        tempo_maps: Mutex::new(vec![Vec::new(); crate::MAX_DECKS]),
        measure: Arc::new(Mutex::new(Measure {
            phase: "idle",
            ..Default::default()
        })),
        retired: std::sync::atomic::AtomicBool::new(false),
        plugins,
        session: Arc::new(Mutex::new({
            let n = crate::session::learn_from_reports();
            ai_log(&format!("learned from {n} transition reports: moves reported bad are picked less"));
            crate::session::Session::load()
        })),
        trimmed: Mutex::new(Default::default()),
        plan: Mutex::new(None),
        visual_moves: Mutex::new(Default::default()),
        auto: Mutex::new(AutoState::default()),
        eval: Mutex::new(Eval::default()),
        eq: Mutex::new(load_eq()),
        app_eq: Mutex::new(load_app_eq()),
        mics: Mutex::new(load_mics()),
        pattern: Arc::new(crate::pattern_service::Service::new(
            crate::session::state_dir().join("pattern.json"),
        )),
        tidal: Arc::new(crate::pattern_service::Service::tidal(
            crate::session::state_dir().join("tidal.json"),
        )),
        // The graph rate is the recipe's own; this is only the fallback when a
        // caller passes no rate.
        material: crate::material_service::Material::new(48_000),
        visual_intent: Default::default(),
        visual_scene: Mutex::new(serde_json::json!({"sequence":0,"clear":true})),
    });
    {
        shared.params.input_main.store(
            std::fs::read_to_string(crate::session::state_dir().join("input-main"))
                .ok()
                .as_deref()
                == Some("true"),
            Ordering::Relaxed,
        );
        let mics = shared.mics.lock().unwrap().clone();
        let mut l = shared.links.lock().unwrap();
        for (ch, m) in mics {
            if let Some(c) = shared.params.channel(&ch) {
                shared.params.input_monitor[c].store(m.monitor, Ordering::Relaxed);
                shared.params.input_send[c].store(m.input_send, Ordering::Relaxed);
                shared.params.set_voice(c, m.voice);
                l.mics.insert(ch, m.source);
            }
        }
    }
    {
        let apps = shared.app_eq.lock().unwrap().clone();
        let mut l = shared.links.lock().unwrap();
        for (app, ch) in apps {
            l.apps.insert(ch, app);
        }
    }
    for (t, st) in shared.eq.lock().unwrap().iter() {
        publish_eq(&shared, t, st);
    }
    apply_mixer(&shared);
    for d in 0..crate::MAX_DECKS {
        apply_eq(&shared, d);
    }
    for (name, f) in [
        ("patch-meters", collector as fn(Arc<Shared>)),
        ("patch-controller", controllers),
        ("patch-links", keeper),
        ("patch-sync", syncer),
        ("patch-session", sessioner),
        ("patch-history", play_history::record_plays),
        ("patch-plans", planner),
        ("patch-volume", system_volume),
        ("patch-idle", idler),
        ("patch-health", health::health),
    ] {
        let s = shared.clone();
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || f(s))
            .map_err(|e| e.to_string())?;
    }
    shared.material.spawn_worker().map_err(|e| e.to_string())?;
    if let Err(error) = crate::stem_service::spawn_preparation(shared.params.clone()) {
        eprintln!("patch stems: could not start preparation recovery: {error}");
    }
    {
        let (svc, host) = (shared.pattern.clone(), shared.clone());
        std::thread::Builder::new()
            .name("patch-pattern".into())
            .spawn(move || crate::pattern_service::run(svc, host))
            .map_err(|e| e.to_string())?;
        let (svc, host) = (shared.tidal.clone(), Arc::new(TidalHost(shared.clone())));
        std::thread::Builder::new()
            .name("patch-tidal".into())
            .spawn(move || crate::pattern_service::run(svc, host))
            .map_err(|e| e.to_string())?;
    }
    let s = shared.clone();
    std::thread::Builder::new()
        .name("patch-engine-ctl".into())
        .spawn(move || control::control(listener, s))
        .map_err(|e| e.to_string())?;

    // SAFETY: blocks until quit; frees the node and loop before returning.
    unsafe { patch_engine_run(h) };
    shared.handle.store(std::ptr::null_mut(), Ordering::Release);
    // Only the socket this engine still owns: after a handover the
    // successor owns `engine.sock`.
    let _ = std::fs::remove_file(if shared.retired.load(Ordering::Acquire) {
        retiring_socket_path()
    } else {
        path
    });
    // SAFETY: the RT thread is gone with the filter; nothing references `rt`.
    drop(unsafe { Box::from_raw(rt) });
    Ok(())
}
