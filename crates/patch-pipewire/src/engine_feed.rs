//! Meters from the Patch engine, shown when they are heard.
//!
//! The engine stamps every graph cycle with the graph clock (CLOCK_MONOTONIC
//! ns). The UI keeps the recent blocks and, each frame, shows those whose
//! time has come: `block nsec + path latency ≤ now` (meter reference
//! OUTPUT). Analysis readings (VU, loudness, spectrum) are stamped when
//! fetched and delayed the same way. If the engine goes away the feed
//! reconnects on its own; audio never depends on it.

use patch_engine::client::Client;
use patch_model::audio::{LoudnessReading, SILENCE_DB};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// A block's DJ Booth fields ("dj": peak L, R, then mean |x| L, R, dB):
/// (peak dB, mean |x| linear); silence from an engine without them.
fn dj_of(b: &Value) -> ([f32; 2], [f32; 2]) {
    let v = |i: usize| b["dj"][i].as_f64().map(|x| x as f32);
    match (v(0), v(1), v(2), v(3)) {
        (Some(a), Some(b2), Some(c), Some(d)) => ([a, b2], [db_to_lin(c), db_to_lin(d)]),
        _ => ([-999.0; 2], [0.0; 2]),
    }
}

pub struct Block {
    pub nsec: u64,
    pub ch: Vec<[f32; 2]>,
    pub out: [f32; 2],
    /// Mean |x| per channel (linear); empty from engines that predate it.
    pub abs: Vec<[f32; 2]>,
    /// Fader gain per channel (linear); empty from engines that predate it.
    pub gain: Vec<[f32; 2]>,
    /// Block length in seconds.
    pub secs: f64,
    /// Native decks: (seconds, step) at this block; None = no track.
    pub decks: Vec<Option<(f64, f64)>>,
    /// Per deck, which track the playhead is in (its load count).
    pub loads: Vec<u32>,
    /// The DJ Booth's sum: peak (dB) and mean |x| (linear), L and R.
    pub dj: [f32; 2],
    pub dj_abs: [f32; 2],
    /// The console's buses as they leave (AUX 1, 2, FX 1, 2): peak dB.
    pub bus: [[f32; 2]; 4],
}

/// A block's bus levels ("bus": [[L, R] × 4], dB); silence from an engine
/// without them.
fn bus_of(b: &Value) -> [[f32; 2]; 4] {
    std::array::from_fn(|i| {
        let v = |s: usize| b["bus"][i][s].as_f64().map(|x| x as f32).unwrap_or(SILENCE_DB);
        [v(0), v(1)]
    })
}

/// The console's buses as the engine has them (status).
#[derive(Clone, Debug, Default)]
pub struct Buses {
    /// Per bus (AUX 1, 2, FX 1, 2): fader dB and mute.
    pub faders: [(f32, bool); 4],
    /// The DJ Booth's sends.
    pub dj_sends: [patch_model::audio::Send; 4],
    /// FX 1's size, FX 2's time (ms) and feedback.
    pub reverb_size: f32,
    pub delay_ms: f32,
    pub delay_feedback: f32,
    /// AUX 1, 2's physical ports: (device, ports), None = source only.
    pub aux_routes: [Option<(String, Vec<String>)>; 2],
}

fn sends_of(v: &Value) -> Option<[patch_model::audio::Send; 4]> {
    let a = v.as_array()?;
    (a.len() == 4).then(|| {
        std::array::from_fn(|i| patch_model::audio::Send {
            db: a[i]["db"].as_f64().map(|x| x as f32),
            pre: a[i]["pre"].as_bool().unwrap_or(i < 2),
        })
    })
}

#[derive(Clone, Default)]
pub struct Snap {
    pub vu: [f32; 2],
    pub loudness: Option<LoudnessReading>,
    pub spectrum: Vec<f32>,
}

#[derive(Default)]
pub struct EngineFeed {
    client: Option<Client>,
    /// Meters and status, fetched on their own thread.
    fetch: Option<Fetcher>,
    /// Control moves to send (knobs, faders): one request per knob, its
    /// latest value. `poll` sends them on the fetcher's thread — the UI
    /// must never wait on the engine.
    writes: std::collections::BTreeMap<String, Value>,
    bindings: std::collections::HashMap<String, String>,
    /// The fetcher's last round trip reached the engine.
    connected: bool,
    /// Since when round trips fail (None: the engine answers, or no round
    /// trip has finished yet).
    silent_since: Option<Instant>,
    /// Meters straight from the engine's shared memory (None: an engine
    /// without it, or not reached yet: the fetcher asks over the socket).
    shm: Option<patch_engine::shm::Reader>,
    shm_seq: u64,
    shm_tried: Option<Instant>,
    last_status: Option<Instant>,
    seq: u64,
    blocks: VecDeque<Block>,
    snaps: VecDeque<(u64, Snap)>,
    shown_until: u64,
    /// Deck playheads of the newest block shown (presentation-aligned).
    pub deck_heads: Vec<Option<(f64, f64)>>,
    /// Per deck, which track `deck_heads` is in (its load count).
    pub deck_loads: Vec<u32>,
    /// The stamp of the block `deck_heads` came from, and the delay to the
    /// ear it was shown with: the playhead moves on from there per frame.
    heads_nsec: u64,
    last_delay: u64,
    /// Per deck: its position moved with the last block (playing).
    heads_moving: Vec<bool>,
    /// Last raw peaks keep a meter frame stable when the UI asks between
    /// graph blocks. A missing block is not a new silent audio measurement.
    last_ch: Vec<[f32; 2]>,
    pub input_ch: Vec<[f32; 2]>,
    last_out: [f32; 2],
    /// Channel VUs, run on the blocks as they come due (so they are
    /// delayed to the ear like the peaks). None: the engine doesn't send
    /// channel levels (an older engine).
    ch_vu: Vec<[patch_meter::BlockVu; 2]>,
    /// The DJ Booth's level as shown (dB) and its VU.
    pub dj_level: [f32; 2],
    dj_vu: [patch_meter::BlockVu; 2],
    has_dj: bool,
    /// The DJ Booth's fader (dB) and mute, from the status.
    pub dj_group: (f32, bool),
    /// The Booth is soloed as one strip. Not saved with the session.
    pub dj_solo: bool,
    /// The Patch mix's fader (dB) and mute, from the status.
    pub master: (f32, bool),
    /// The engine applies the system volume after its recording tap (on
    /// what leaves for the device), not in each channel.
    pub system_post: bool,
    has_abs: bool,
    /// The engine sends fader gains: channel levels are post-fader.
    pub post_fader: bool,
    /// The DJ mixer at the last poll: crossfader, channel faders.
    pub dj_mixer: Option<(f32, [f32; 4])>,
    /// The decks' EQ knobs as the engine moves them (from shared memory
    /// only; None without it).
    pub dj_eq: Option<[[f32; 4]; 4]>,
    /// The decks' stem gains and levels as they move (shared memory only;
    /// NaN gains: no stems attached).
    pub dj_stems: Option<([[f32; 4]; 4], [[f32; 4]; 4])>,
    pub channels: Vec<String>,
    pub visible_modules: std::collections::BTreeSet<String>,
    /// Output device the program plays on, and whether the engine is linked.
    pub output: Option<String>,
    /// Separate device receiving the DJ headphone cue, when configured.
    pub cue_output: Option<String>,
    pub output_ports: Vec<String>,
    pub cue_ports: Vec<String>,
    pub linked: bool,
    pub rate: u32,
    pub quantum: u32,
    /// The output device's delay to the DAC as its driver tracks it,
    /// seconds (None: the driver reports none).
    pub output_delay: Option<f64>,
    /// Real-time callback cost as a share of the cycle: last second's
    /// average and worst.
    pub load: (f32, f32),
    /// Cycles after an xrun PipeWire reported to the engine's node, since
    /// the engine started (None: not read yet).
    pub xruns: Option<u32>,
    /// The engine's audio thread has real-time scheduling.
    pub realtime: bool,
    /// Mute per engine channel (from status).
    pub mutes: std::collections::HashMap<String, bool>,
    /// Solo-in-place per engine channel (from status). Clearing every solo
    /// restores the previous gains; mute is a separate flag.
    pub solos: std::collections::HashMap<String, bool>,
    /// Headphone cue/PFL state per DJ deck.
    pub pfls: std::collections::HashMap<String, bool>,
    /// Each channel's gain (dB), and the microphones on mic channels:
    /// (channel, capture source).
    pub gains: std::collections::HashMap<String, f32>,
    /// Each channel's console knobs (trim, 3-band EQ, pan, sends).
    pub controls: std::collections::HashMap<String, patch_model::audio::StripControls>,
    /// The console's buses; None from an engine without them.
    pub buses: Option<Buses>,
    /// The buses' levels as last shown (peak dB, AUX 1, 2, FX 1, 2).
    pub bus_level: [[f32; 2]; 4],
    pub mics: Vec<(String, String)>,
    /// Mic lanes kept out of Patch Input and every send (Remote voices).
    pub mix_minus: std::collections::BTreeSet<String>,
    /// Lanes a Remote voice took: lane ("stream-3") → mic lane ("mic-6").
    pub remote_lanes: std::collections::BTreeMap<String, String>,
    pub active_inputs: std::collections::BTreeSet<String>,
}

/// The engine round trips for meters and status, on a thread of their
/// own: a request in, its answer out (None: the engine isn't reachable;
/// it tries again at most once a second).
struct Fetcher {
    tx: std::sync::mpsc::Sender<(&'static str, Value)>,
    rx: std::sync::mpsc::Receiver<(&'static str, Option<Value>)>,
    inflight: usize,
}

fn fetched_reply(op: &str, reply: Result<Result<Value, String>, String>) -> Option<Value> {
    match reply {
        Ok(Ok(value)) => Some(value),
        // A caller can leave before a queued control reaches the engine.
        // Its rejection is not a disconnected engine or a reason to start
        // fallback routing/recovery on the UI thread.
        Ok(Err(error)) if op == "write" => Some(json!({"error": error})),
        _ => None,
    }
}

impl Fetcher {
    fn spawn() -> Fetcher {
        let (tx, req) = std::sync::mpsc::channel::<(&'static str, Value)>();
        let (out, rx) = std::sync::mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("patch-meter-fetch".into())
            .spawn(move || {
                let mut client: Option<Client> = None;
                let mut last_try: Option<Instant> = None;
                for (op, r) in req {
                    if client.is_none()
                        && last_try.is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
                    {
                        last_try = Some(Instant::now());
                        client = Client::connect();
                    }
                    let v = client.as_mut().and_then(|c| fetched_reply(op, c.exchange(&r)));
                    if v.is_none() {
                        client = None;
                    }
                    if out.send((op, v)).is_err() {
                        return;
                    }
                }
            });
        Fetcher {
            tx,
            rx,
            inflight: 0,
        }
    }

    fn send_writes(&mut self, writes: &mut std::collections::BTreeMap<String, Value>) -> bool {
        if self.inflight != 0 { return false; }
        // Writes precede status, so a status cannot echo the pre-drag value.
        for (_, req) in std::mem::take(writes) { self.send("write", req); }
        true
    }

    fn send(&mut self, op: &'static str, req: Value) {
        if self.tx.send((op, req)).is_ok() {
            self.inflight += 1;
        }
    }
}

/// Keep ~3 s of history (Bluetooth paths reach ~250 ms; trims ±500 ms).
const KEEP_NS: u64 = 3_000_000_000;

impl EngineFeed {
    /// The engine is up and owns the program path.
    pub fn live(&self) -> bool {
        self.connected && self.linked
    }

    /// The engine is gone, not merely not reached yet: its round trips have
    /// failed for 3 s. A window that has just opened has had no answer yet,
    /// and one that briefly sees `linked` false (the engine relinking) still
    /// has an engine: neither may take over the engine's stream routing.
    pub fn absent(&self) -> bool {
        self.silent_since
            .is_some_and(|t| t.elapsed() > Duration::from_secs(3))
    }

    /// A command (mute, cue, …) on the feed's own connection, opened when
    /// first needed (meters and status go through the fetcher).
    fn call(&mut self, req: Value) -> Option<Value> {
        self.call_report(req).ok()
    }

    /// Queue a control move: one write per knob (`key`), the latest value
    /// winning — a drag posts faster than the engine wants writes. Sent on
    /// the fetcher's thread from [`Self::poll`]; the reply is not needed,
    /// the next status is the truth.
    fn attach_stream_owner(&self, req: &mut Value) {
        if let Some(owner) = req["channel"].as_str().and_then(|channel| self.bindings.get(channel)) {
            req["stream_key"] = json!(owner);
        }
    }

    fn post(&mut self, key: String, mut req: Value) {
        self.attach_stream_owner(&mut req);
        self.writes.insert(key, req);
    }

    pub fn bind_stream_channels(&mut self, assigned: &std::collections::HashMap<String, String>) {
        self.bindings = assigned.iter().filter(|(key, _)| patch_core::stream::is_key(key))
            .map(|(key, channel)| (channel.clone(), key.clone())).collect();
        self.writes.retain(|_, req| req["stream_key"].as_str().is_none_or(|key|
            req["channel"].as_str().and_then(|channel| self.bindings.get(channel)).is_some_and(|current| current == key)));
    }

    /// Like [`Self::call`], but the engine's error is the `Err` instead of
    /// a dropped reply. A transport failure still drops the socket.
    fn call_report(&mut self, mut req: Value) -> Result<Value, String> {
        self.attach_stream_owner(&mut req);
        if self.client.is_none() {
            self.client = Client::connect();
        }
        let Some(c) = self.client.as_mut() else {
            return Err("engine is not running".into());
        };
        match c.call(&req) {
            Ok(v) => Ok(v),
            Err(error) => {
                self.client = None;
                Err(error)
            }
        }
    }

    /// Forget the reconnect backoff and the last status: the next poll
    /// connects and asks for status at once.
    pub fn reconnect_now(&mut self) {
        self.client = None;
        self.last_status = None;
    }

    /// Take what the fetcher brought (new blocks; once a second the engine
    /// status) and ask for the next, without waiting: the socket round
    /// trip runs on the fetcher's thread, not in the UI's frame.
    pub fn poll(&mut self) {
        let fetch = self.fetch.get_or_insert_with(Fetcher::spawn);
        let got: Vec<(&'static str, Option<Value>)> =
            std::iter::from_fn(|| fetch.rx.try_recv().ok()).collect();
        for (op, v) in got {
            if let Some(f) = self.fetch.as_mut() {
                f.inflight = f.inflight.saturating_sub(1);
            }
            match v {
                None => {
                    self.connected = false;
                    self.linked = false;
                    self.silent_since.get_or_insert_with(Instant::now);
                }
                Some(v) => {
                    self.connected = true;
                    self.silent_since = None;
                    match op {
                        "status" => self.apply_status(v),
                        // A control move: the next status is the truth.
                        "write" => {}
                        _ => self.apply_meters(v),
                    }
                }
            }
        }
        let shared = self.read_shm();
        let status_due = self
            .last_status
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
        let Some(f) = self.fetch.as_mut() else { return };
        // Only one batch may be in flight. While the engine is slow, keep
        // coalescing each knob's latest value on the UI side instead of
        // piling a drag's obsolete values onto the worker's socket queue.
        if !f.send_writes(&mut self.writes) { return; }
        if status_due {
            self.last_status = Some(Instant::now());
            f.send("status", json!({"op": "status"}));
        }
        if !shared {
            f.send(
                "meters",
                json!({"op": "meters", "since": self.seq, "packed": true}),
            );
        }
    }

    /// New blocks and the engine's state from shared memory; false when
    /// there is none to read (the socket's meters then).
    fn read_shm(&mut self) -> bool {
        if self.shm.as_mut().is_some_and(|r| r.stale()) {
            self.shm = None;
        }
        if self.shm.is_none() {
            if self
                .shm_tried
                .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
            {
                return false;
            }
            self.shm_tried = Some(Instant::now());
            self.shm = patch_engine::shm::Reader::open();
            // A new ring: its block numbers start again.
            self.shm_seq = 0;
        }
        let Some(r) = self.shm.as_ref() else {
            return false;
        };
        let Some(state) = r.now() else { return false };
        let (newest, blocks) = r.blocks_since(self.shm_seq);
        self.shm_seq = newest;
        let now = patch_engine::now_ns();
        self.dj_mixer = Some((state.xfader, state.faders));
        self.dj_eq = Some(state.eq);
        self.dj_stems = Some((state.stems, state.stem_levels));
        let channels = state.names();
        if channels != self.channels {
            self.last_ch.clear();
            self.input_ch.clear();
            self.last_out = [SILENCE_DB; 2];
            self.ch_vu.clear();
            self.channels = channels;
        }
        let db = patch_engine::linear_to_db;
        let n = self.channels.len();
        for b in blocks {
            let m = (b.channels as usize).min(n);
            self.blocks.push_back(Block {
                nsec: b.nsec,
                ch: (0..m)
                    .map(|c| [db(b.ch_peak[c][0]), db(b.ch_peak[c][1])])
                    .collect(),
                abs: (0..m).map(|c| b.ch_abs[c]).collect(),
                out: [db(b.out_peak[0]), db(b.out_peak[1])],
                gain: (0..m).map(|c| b.ch_gain[c]).collect(),
                secs: b.frames as f64 / (b.rate.max(1)) as f64,
                decks: (0..b.decks as usize)
                    .map(|d| {
                        (!b.deck_secs[d].is_nan()).then(|| (b.deck_secs[d], b.deck_step[d] as f64))
                    })
                    .collect(),
                loads: b.deck_load[..(b.decks as usize).min(b.deck_load.len())].to_vec(),
                dj: [db(b.dj_peak[0]), db(b.dj_peak[1])],
                dj_abs: b.dj_abs,
                bus: b.bus_peak.map(|p| [db(p[0]), db(p[1])]),
            });
        }
        let l = state.loudness;
        self.snaps.push_back((
            now,
            Snap {
                vu: state.vu,
                loudness: Some(LoudnessReading {
                    momentary: l[0],
                    short_term: l[1],
                    integrated: l[2],
                    range: l[3],
                    true_peak_max: l[4],
                }),
                spectrum: state.spectrum[..(state.bands as usize).min(patch_engine::shm::BANDS)]
                    .to_vec(),
            },
        ));
        let horizon = now.saturating_sub(KEEP_NS);
        while self.blocks.front().is_some_and(|b| b.nsec < horizon) {
            self.blocks.pop_front();
        }
        while self.snaps.front().is_some_and(|(t, _)| *t < horizon) {
            self.snaps.pop_front();
        }
        true
    }

    /// A status already in flight predates controls still waiting locally.
    /// Overlay only those fields; other channels and external edits remain
    /// authoritative as soon as there is no pending write for that field.
    fn overlay_pending_status(&self, status: &mut Value) {
        for request in self.writes.values() {
            let op = request["op"].as_str().unwrap_or("");
            let channel = request["channel"].as_str().unwrap_or("");
            if let Some(channels) = status["channels"].as_array_mut()
                && let Some(row) = channels.iter_mut().find(|c| c["name"] == channel)
            {
                for (operation, from, to) in [("gain", "db", "gain_db"), ("mute", "muted", "mute"), ("trim", "db", "trim_db"), ("pan", "pan", "pan"), ("solo", "solo", "solo"), ("pfl", "on", "pfl")] {
                    if op == operation && let Some(value) = request.get(from) { row[to] = value.clone(); }
                }
                if op == "eq" {
                    for key in ["low_db", "mid_db", "high_db", "filter"] {
                        if let Some(value) = request.get(key) { row["eq"][key] = value.clone(); }
                    }
                }
                if op == "send" && let Some(bus) = request["bus"].as_u64()
                    && let Some(send) = row["sends"].as_array_mut().and_then(|sends| sends.get_mut(bus as usize))
                {
                    for key in ["db", "pre"] { if let Some(value) = request.get(key) { send[key] = value.clone(); } }
                }
            }
            for (operation, from, to) in [("master", "db", "master_db"), ("master", "mute", "master_mute")] {
                if op == operation && let Some(value) = request.get(from) { status[to] = value.clone(); }
            }
            if op == "solo" && channel == "dj" { status["dj_group"]["solo"] = request["solo"].clone(); }
            if op == "dj_group" {
                for key in ["gain_db", "mute"] { if let Some(value) = request.get(key) { status["dj_group"][key] = value.clone(); } }
            }
        }
    }

    fn apply_status(&mut self, mut s: Value) {
        self.overlay_pending_status(&mut s);
        self.active_inputs = s["mics"].as_array().into_iter().flatten().filter(|m|
            m["monitor"] == true || (s["capture_active"] == true && s["input_main"] != true))
            .filter_map(|m| m["channel"].as_str().map(String::from)).collect();
        self.visible_modules = s["visible_modules"].as_array().into_iter().flatten()
            .filter_map(|v| v.as_str().map(String::from)).collect();
        self.output = s["output"].as_str().map(String::from);
        self.cue_output = s["cue_output"].as_str().map(String::from);
        let ports = |name: &str| s[name].as_array().into_iter().flatten().filter_map(|p| p.as_str().map(String::from)).collect();
        self.output_ports = ports("output_ports");
        self.cue_ports = ports("cue_ports");
        self.linked = s["linked"].as_bool().unwrap_or(false);
        self.rate = s["rate"].as_u64().unwrap_or(0) as u32;
        self.quantum = s["quantum"].as_u64().unwrap_or(0) as u32;
        self.output_delay = s["output_delay_ms"]
            .as_f64()
            .filter(|ms| *ms > 0.0)
            .map(|ms| ms / 1000.0);
        self.realtime = s["realtime"].as_bool().unwrap_or(false);
        self.mutes = s["channels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| Some((c["name"].as_str()?.to_string(), c["mute"].as_bool()?)))
            .collect();
        self.solos = s["channels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| Some((c["name"].as_str()?.to_string(), c["solo"].as_bool()?)))
            .collect();
        self.pfls = s["channels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| Some((c["name"].as_str()?.to_string(), c["pfl"].as_bool()?)))
            .collect();
        self.gains = s["channels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| {
                Some((
                    c["name"].as_str()?.to_string(),
                    c["gain_db"].as_f64()? as f32,
                ))
            })
            .collect();
        self.controls = s["channels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| {
                let db = |v: &Value| v.as_f64().unwrap_or(0.0).max(-120.0) as f32;
                Some((
                    c["name"].as_str()?.to_string(),
                    patch_model::audio::StripControls {
                        gain_db: c["trim_db"].as_f64().map(|v| v as f32),
                        high_db: db(&c["eq"]["high_db"]),
                        mid_db: db(&c["eq"]["mid_db"]),
                        low_db: db(&c["eq"]["low_db"]),
                        pan: c["pan"].as_f64().unwrap_or(0.0) as f32,
                        tone: true,
                        sends: sends_of(&c["sends"]),
                        sends_off: None,
                    },
                ))
            })
            .collect();
        self.buses = s["mixer_buses"]["buses"].as_array().filter(|a| a.len() == 4).map(|a| {
            let m = &s["mixer_buses"];
            let f = |v: &Value, d: f32| v.as_f64().map_or(d, |x| x as f32);
            let route = |k: &str| {
                let r = &s["aux_routes"][k];
                Some((
                    r["device"].as_str()?.to_string(),
                    r["ports"].as_array()?.iter().filter_map(|p| p.as_str().map(String::from)).collect(),
                ))
            };
            Buses {
                faders: std::array::from_fn(|i| (f(&a[i]["gain_db"], 0.0).max(-120.0), a[i]["mute"].as_bool().unwrap_or(false))),
                dj_sends: sends_of(&m["dj_sends"]).unwrap_or_default(),
                reverb_size: f(&m["aux_fx"]["reverb_size"], 0.5),
                delay_ms: f(&m["aux_fx"]["delay_ms"], 375.0),
                delay_feedback: f(&m["aux_fx"]["delay_feedback"], 0.35),
                aux_routes: [route("aux1"), route("aux2")],
            }
        });
        self.system_post = s["system_post"].as_bool().unwrap_or(false);
        self.master = (
            s["master_db"].as_f64().unwrap_or(0.0).max(-120.0) as f32,
            s["master_mute"].as_bool().unwrap_or(false),
        );
        self.dj_group = (
            s["dj_group"]["gain_db"].as_f64().unwrap_or(0.0) as f32,
            s["dj_group"]["mute"].as_bool().unwrap_or(false),
        );
        self.dj_solo = s["dj_group"]["solo"].as_bool().unwrap_or(false);
        self.mix_minus = s["mics"].as_array().into_iter().flatten()
            .filter(|m| m["input_send"] == false)
            .filter_map(|m| m["channel"].as_str().map(String::from)).collect();
        self.remote_lanes = s["remote_lanes"].as_object().into_iter().flatten()
            .filter_map(|(lane, mic)| Some((lane.clone(), mic.as_str()?.to_string()))).collect();
        self.mics = s["mics"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| {
                Some((
                    m["channel"].as_str()?.to_string(),
                    m["source"].as_str()?.to_string(),
                ))
            })
            .collect();
        self.mics.sort_by_key(|(channel, _)| channel.trim_start_matches("mic-").parse::<usize>().unwrap_or(usize::MAX));
        self.xruns = s["timing"]["xruns"].as_u64().map(|x| x.min(u64::from(u32::MAX)) as u32);
        self.load = (
            s["load_avg"].as_f64().unwrap_or(0.0) as f32,
            s["load_max"].as_f64().unwrap_or(0.0) as f32,
        );
    }

    fn apply_meters(&mut self, m: Value) {
        let now = patch_engine::now_ns();
        let fl = |v: &Value| v.as_f64().map(|x| x as f32);
        self.dj_mixer = match (
            fl(&m["mixer"]["xfader"]),
            fl(&m["mixer"]["faders"][0]),
            fl(&m["mixer"]["faders"][1]),
        ) {
            (Some(x), Some(a), Some(b)) => Some((x, [a, b, fl(&m["mixer"]["faders4"][2]).unwrap_or(0.0), fl(&m["mixer"]["faders4"][3]).unwrap_or(0.0)])),
            _ => None,
        };
        self.seq = m["seq"].as_u64().unwrap_or(self.seq);
        let channels = m["channels"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if channels != self.channels {
            self.last_ch.clear();
            self.input_ch.clear();
            self.last_out = [SILENCE_DB; 2];
            self.ch_vu.clear();
            self.channels = channels;
        }
        let pair = |v: &Value| {
            [
                v[0].as_f64().unwrap_or(-120.0) as f32,
                v[1].as_f64().unwrap_or(-120.0) as f32,
            ]
        };
        let n = self.channels.len();
        for b in m["blocks"].as_array().into_iter().flatten() {
            // Packed levels (centi-dB) and gains (×8000), when the engine has them.
            if let (Some(lv), Some(g)) = (b["lv"].as_str(), b["g"].as_str()) {
                let lv = patch_core::pack::unpack_i16(lv);
                let g = patch_core::pack::unpack_i16(g);
                if lv.len() == 4 * n + 2 && g.len() == 2 * n {
                    let db = |i: usize| lv[i] as f32 / 100.0;
                    self.blocks.push_back(Block {
                        nsec: b["nsec"].as_u64().unwrap_or(0),
                        ch: (0..n).map(|c| [db(2 * c), db(2 * c + 1)]).collect(),
                        abs: (0..n)
                            .map(|c| {
                                [
                                    db_to_lin(db(2 * n + 2 * c)),
                                    db_to_lin(db(2 * n + 2 * c + 1)),
                                ]
                            })
                            .collect(),
                        out: [db(4 * n), db(4 * n + 1)],
                        gain: (0..n)
                            .map(|c| [g[2 * c] as f32 / 8000.0, g[2 * c + 1] as f32 / 8000.0])
                            .collect(),
                        secs: b["frames"].as_f64().unwrap_or(0.0)
                            / b["rate"].as_f64().unwrap_or(48_000.0).max(1.0),
                        decks: b["decks"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .map(|d| Some((d[0].as_f64()?, d[1].as_f64()?)))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        dj: dj_of(b).0,
                        dj_abs: dj_of(b).1,
                        bus: bus_of(b),
                        loads: vec![],
                    });
                }
                continue;
            }
            self.blocks.push_back(Block {
                nsec: b["nsec"].as_u64().unwrap_or(0),
                ch: b["ch"]
                    .as_array()
                    .map(|a| a.iter().map(pair).collect())
                    .unwrap_or_default(),
                out: pair(&b["out"]),
                abs: b["abs"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(pair)
                            .map(|[l, r]| [db_to_lin(l), db_to_lin(r)])
                            .collect()
                    })
                    .unwrap_or_default(),
                gain: b["gain"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|g| {
                                [
                                    g[0].as_f64().unwrap_or(1.0) as f32,
                                    g[1].as_f64().unwrap_or(1.0) as f32,
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                secs: b["frames"].as_f64().unwrap_or(0.0)
                    / b["rate"].as_f64().unwrap_or(48_000.0).max(1.0),
                decks: b["decks"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|d| Some((d[0].as_f64()?, d[1].as_f64()?)))
                            .collect()
                    })
                    .unwrap_or_default(),
                dj: dj_of(b).0,
                dj_abs: dj_of(b).1,
                bus: bus_of(b),
                loads: vec![],
            });
        }
        let l = &m["loudness"];
        let f = |k: &str| l[k].as_f64().unwrap_or(-120.0) as f32;
        let snap = Snap {
            vu: pair(&m["vu"]),
            loudness: Some(LoudnessReading {
                momentary: f("m"),
                short_term: f("s"),
                integrated: f("i"),
                range: f("lra"),
                true_peak_max: f("tp_max"),
            }),
            spectrum: match m["spectrum16"].as_str() {
                Some(p) => patch_core::pack::unpack_i16(p)
                    .into_iter()
                    .map(|x| x as f32 / 100.0)
                    .collect(),
                None => m["spectrum"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|v| v.as_f64().unwrap_or(-120.0) as f32)
                            .collect()
                    })
                    .unwrap_or_default(),
            },
        };
        self.snaps.push_back((now, snap));
        let horizon = now.saturating_sub(KEEP_NS);
        while self.blocks.front().is_some_and(|b| b.nsec < horizon) {
            self.blocks.pop_front();
        }
        while self.snaps.front().is_some_and(|(t, _)| *t < horizon) {
            self.snaps.pop_front();
        }
    }

    /// The decks' playheads as heard now: the newest shown block's, moved
    /// on at each deck's speed for the time since (at most 50 ms), so the
    /// waveforms glide instead of stepping a block (21 ms) at a time.
    pub fn heads_now(&self) -> Vec<Option<(f64, f64)>> {
        let due = patch_engine::now_ns().saturating_sub(self.last_delay);
        let dt = (due.saturating_sub(self.heads_nsec) as f64 / 1e9).min(0.05);
        self.deck_heads
            .iter()
            .enumerate()
            .map(|(i, h)| {
                h.map(|(pos, step)| {
                    if self.heads_moving.get(i) == Some(&true) {
                        (pos + dt * step, step)
                    } else {
                        (pos, step)
                    }
                })
            })
            .collect()
    }

    /// Peaks of every block that came due since the previous call: per
    /// channel and of the program. `delay_ns` is the path to the ear.
    pub fn take_due(&mut self, delay_ns: u64) -> (Vec<[f32; 2]>, [f32; 2]) {
        let due = patch_engine::now_ns().saturating_sub(delay_ns);
        self.last_delay = delay_ns;
        let n = self.channels.len();
        self.last_ch.resize(n, [SILENCE_DB; 2]);
        let mut ch = self.last_ch.clone();
        self.input_ch.resize(n, [SILENCE_DB; 2]);
        let mut input = self.input_ch.clone();
        let mut out = self.last_out;
        let mut dj = self.dj_level;
        let mut bus = self.bus_level;
        let mut newest = self.shown_until;
        let mut has_new_block = false;
        for b in self
            .blocks
            .iter()
            .filter(|b| b.nsec > self.shown_until && b.nsec <= due)
        {
            // Aggregate only this newly due window. Starting from the prior
            // peak made every later silent block inherit the old maximum
            // forever, so paused decks appeared pinned at full scale.
            if !has_new_block {
                ch.fill([SILENCE_DB; 2]);
                input.fill([SILENCE_DB; 2]);
                out = [SILENCE_DB; 2];
                dj = [SILENCE_DB; 2];
                bus = [[SILENCE_DB; 2]; 4];
                has_new_block = true;
            }
            for (acc, v) in input.iter_mut().zip(&b.ch) {
                *acc = [acc[0].max(v[0]), acc[1].max(v[1])];
            }
            // Post-fader when the engine says what each fader did in this
            // cycle: the level a channel actually adds to the mix.
            self.post_fader = !b.gain.is_empty();
            let lift = |g: f32| if g > 1e-6 { 20.0 * g.log10() } else { -200.0 };
            for (i, (acc, v)) in ch.iter_mut().zip(&b.ch).enumerate() {
                let g = b
                    .gain
                    .get(i)
                    .map(|g| [lift(g[0]), lift(g[1])])
                    .unwrap_or([0.0; 2]);
                let v = [(v[0] + g[0]).max(SILENCE_DB), (v[1] + g[1]).max(SILENCE_DB)];
                *acc = [acc[0].max(v[0]), acc[1].max(v[1])];
            }
            out = [out[0].max(b.out[0]), out[1].max(b.out[1])];
            dj = [dj[0].max(b.dj[0]), dj[1].max(b.dj[1])];
            for (acc, v) in bus.iter_mut().zip(&b.bus) {
                *acc = [acc[0].max(v[0]), acc[1].max(v[1])];
            }
            if b.dj[0] > -900.0 {
                self.has_dj = true;
                self.dj_vu[0].step(b.dj_abs[0] as f64, b.secs);
                self.dj_vu[1].step(b.dj_abs[1] as f64, b.secs);
            }
            if !b.abs.is_empty() {
                self.has_abs = true;
                self.ch_vu.resize_with(n, Default::default);
                for (i, (vu, a)) in self.ch_vu.iter_mut().zip(&b.abs).enumerate() {
                    let g = b.gain.get(i).copied().unwrap_or([1.0; 2]);
                    vu[0].step((a[0] * g[0]) as f64, b.secs);
                    vu[1].step((a[1] * g[1]) as f64, b.secs);
                }
            }
            newest = b.nsec;
            self.heads_moving = b
                .decks
                .iter()
                .enumerate()
                .map(
                    |(i, h)| match (h, self.deck_heads.get(i).copied().flatten()) {
                        (Some((p, _)), Some((q, _))) => (p - q).abs() > 1e-6,
                        _ => false,
                    },
                )
                .collect();
            self.deck_heads.clone_from(&b.decks);
            self.deck_loads.clone_from(&b.loads);
            self.heads_nsec = b.nsec;
        }
        self.shown_until = newest;
        if has_new_block {
            self.dj_level = dj;
            self.bus_level = bus;
        }
        self.input_ch = input;
        self.last_ch.clone_from(&ch);
        self.last_out = out;
        (ch, out)
    }

    /// The DJ Booth's VU (L, R), after `take_due`; None from an engine
    /// that doesn't send it.
    pub fn dj_vu(&self) -> Option<[f32; 2]> {
        self.has_dj.then(|| {
            [
                self.dj_vu[0].reading() as f32,
                self.dj_vu[1].reading() as f32,
            ]
        })
    }

    /// VU per channel (L, R) as heard, after `take_due`; None when the
    /// engine doesn't send channel levels.
    pub fn channel_vu(&self, i: usize) -> Option<[f32; 2]> {
        if !self.has_abs {
            return None;
        }
        let v = self.ch_vu.get(i)?;
        Some([v[0].reading() as f32, v[1].reading() as f32])
    }

    /// Analysis as it was `delay_ns` ago.
    pub fn snap_due(&self, delay_ns: u64) -> Option<&Snap> {
        let due = patch_engine::now_ns().saturating_sub(delay_ns);
        self.snaps
            .iter()
            .rev()
            .find(|(t, _)| *t <= due)
            .map(|(_, s)| s)
    }

    pub fn channel(&self, name: &str) -> Option<usize> {
        self.channels.iter().position(|c| c == name)
    }

    /// Point the program at another output.
    pub fn set_output(&mut self, sink: &str) -> Result<(), String> {
        let v = self
            .call(json!({"op": "output", "sink": sink}))
            .ok_or("the engine is not running")?;
        if v == json!("ok") {
            self.output = Some(sink.to_string());
            Ok(())
        } else {
            Err("the engine refused the output".into())
        }
    }

    /// Route the DJ cue to a separate hardware output, or disable it.
    pub fn route_output_ports(&mut self, bus: &str, sink: &str, ports: &[String]) -> Result<(), String> {
        // Keep the engine's refusal. `call` turns every failure, including a
        // routing error, into a missing reply.
        let reply = self
            .call_report(json!({"op":"output_route","bus":bus,"sink":sink,"ports":ports}))?;
        if reply != json!("ok") { return Err(reply.to_string()); }
        match bus {
            "main" => { self.output = Some(sink.into()); self.output_ports = ports.to_vec(); }
            "cue" => { self.cue_output = Some(sink.into()); self.cue_ports = ports.to_vec(); }
            aux => {
                if let Some(b) = self.buses.as_mut() {
                    b.aux_routes[if aux == "aux1" { 0 } else { 1 }] = Some((sink.into(), ports.to_vec()));
                }
            }
        }
        Ok(())
    }

    pub fn select_output(&mut self, sink: &str) -> Result<(), String> {
        let reply = self.call(serde_json::json!({"op": "output_select", "sink": sink}));
        if reply.as_ref().is_some_and(|v| *v == json!("ok")) { Ok(()) }
        else { Err(reply.and_then(|v| v["error"].as_str().map(String::from)).unwrap_or_else(|| "output selection failed".into())) }
    }

    pub fn set_cue_output(&mut self, sink: Option<&str>) -> Result<(), String> {
        let v = self
            .call(json!({"op": "cue_output", "sink": sink}))
            .ok_or("the engine is not running")?;
        if v == json!("ok") {
            self.cue_output = sink.map(String::from);
            Ok(())
        } else {
            Err("the engine refused the headphone output".into())
        }
    }

    /// One Advanced Mixer knob on an engine channel: "gain" (trim, dB),
    /// "high" / "mid" / "low" (EQ, dB) or "pan" (−1 … 1).
    pub fn set_control(&mut self, channel: &str, control: &str, value: f32) -> Result<(), String> {
        let req = match control {
            "gain" => json!({"op": "trim", "channel": channel, "db": value}),
            "pan" => json!({"op": "pan", "channel": channel, "pan": value}),
            "high" => json!({"op": "eq", "channel": channel, "high_db": value}),
            "mid" => json!({"op": "eq", "channel": channel, "mid_db": value}),
            "low" => json!({"op": "eq", "channel": channel, "low_db": value}),
            _ => return Err(format!("no control '{control}'")),
        };
        // Optimistic and non-blocking: queued as one write per knob (the
        // latest value wins), sent on the fetcher's thread; the next
        // status is the truth.
        self.post(format!("control:{channel}:{control}"), req);
        // Shown at once, before the next status.
        let c = self.controls.entry(channel.to_string()).or_default();
        match control {
            "gain" => c.gain_db = Some(value),
            "pan" => c.pan = value,
            "high" => c.high_db = value,
            "mid" => c.mid_db = value,
            _ => c.low_db = value,
        }
        Ok(())
    }

    /// A send on a channel (or "dj"): `bus` 0 … 3, dB (None: off) and/or pre.
    /// One queued write per field, so a level drag never waits on the
    /// engine and never swallows a pre/post flip beside it.
    pub fn set_send(&mut self, channel: &str, bus: usize, db: Option<Option<f32>>, pre: Option<bool>) -> Result<(), String> {
        if let Some(db) = db {
            let mut req = json!({"op": "send", "channel": channel, "bus": bus});
            req["db"] = json!(db);
            self.post(format!("send:{channel}:{bus}:db"), req);
        }
        if let Some(pre) = pre {
            let mut req = json!({"op": "send", "channel": channel, "bus": bus});
            req["pre"] = json!(pre);
            self.post(format!("send:{channel}:{bus}:pre"), req);
        }
        let apply = |sends: &mut [patch_model::audio::Send; 4]| {
            if let Some(db) = db {
                sends[bus].db = db;
            }
            if let Some(pre) = pre {
                sends[bus].pre = pre;
            }
        };
        if channel == "dj" {
            if let Some(b) = self.buses.as_mut() {
                apply(&mut b.dj_sends);
            }
        } else if let Some(sends) = self.controls.get_mut(channel).and_then(|c| c.sends.as_mut()) {
            apply(sends);
        }
        Ok(())
    }

    /// A bus's fader (dB) and/or mute, or its effect's control.
    pub fn set_bus(&mut self, bus: usize, db: Option<f32>, mute: Option<bool>, param: Option<f32>) -> Result<(), String> {
        // One queued write per field: a fader drag, a mute flip and an FX
        // knob never wait on the engine and never overwrite each other.
        if let Some(db) = db {
            self.post(format!("bus:{bus}:db"), json!({"op": "bus", "bus": bus, "db": db}));
        }
        if let Some(m) = mute {
            self.post(format!("bus:{bus}:mute"), json!({"op": "bus", "bus": bus, "mute": m}));
        }
        if let Some(v) = param {
            let req = match bus {
                2 => json!({"op": "aux_fx", "reverb_size": v}),
                3 => json!({"op": "aux_fx", "delay_ms": 40.0 + v * 960.0}),
                _ => return Err("only the FX returns have a control".into()),
            };
            self.post(format!("bus:{bus}:param"), req);
        }
        if let Some(b) = self.buses.as_mut() {
            let f = &mut b.faders[bus];
            if let Some(db) = db {
                f.0 = db;
            }
            if let Some(m) = mute {
                f.1 = m;
            }
            match (bus, param) {
                (2, Some(v)) => b.reverb_size = v,
                (3, Some(v)) => b.delay_ms = 40.0 + v * 960.0,
                _ => {}
            }
        }
        Ok(())
    }

    /// AUX 1 or 2 back to its virtual source only.
    pub fn clear_aux_route(&mut self, bus: &str) -> Result<(), String> {
        let reply = self.call(json!({"op": "output_route_clear", "bus": bus})).ok_or("the engine is not running")?;
        if let Some(e) = reply.get("error").and_then(Value::as_str) {
            return Err(e.to_string());
        }
        if let Some(b) = self.buses.as_mut() {
            b.aux_routes[if bus == "aux1" { 0 } else { 1 }] = None;
        }
        Ok(())
    }

    /// Applications with an engine channel of their own: name → channel.
    pub fn app_channels(&mut self) -> std::collections::HashMap<String, String> {
        self.call(json!({"op": "eq_apps"}))
            .and_then(|v| serde_json::from_value(v["assigned"].clone()).ok())
            .unwrap_or_default()
    }

    /// Give an application an engine channel of its own (one of four):
    /// its knobs then work. Returns the channel.
    pub fn assign_app(&mut self, app: &str) -> Result<String, String> {
        if patch_core::stream::is_key(app) {
            return self.app_channels().get(app).cloned()
                .ok_or_else(|| "this stream is waiting for a native lane (32 simultaneous playback streams supported)".into());
        }
        let cmd = patch_core::command::Command::EqApp { app: app.to_string(), on: true };
        let reply = self
            .call(json!({"op": "command", "origin": "user", "command": cmd}))
            .ok_or("the engine is not running")?;
        if let Some(e) = reply.get("error").and_then(Value::as_str) {
            return Err(e.to_string());
        }
        self.app_channels()
            .get(app)
            .cloned()
            .ok_or_else(|| "four applications have their own channel already".into())
    }

    /// Set an engine channel's fader (deck strips in the Mixer). Queued:
    /// a drag never waits on the engine; the next status is the truth.
    pub fn set_gain(&mut self, channel: &str, db: f32) -> Result<(), String> {
        self.post(format!("gain:{channel}"), json!({"op": "gain", "channel": channel, "db": db}));
        self.gains.insert(channel.to_string(), db);
        Ok(())
    }

    pub fn set_mute(&mut self, channel: &str, muted: bool) -> Result<(), String> {
        self.post(format!("mute:{channel}"), json!({"op": "mute", "channel": channel, "muted": muted}));
        self.mutes.insert(channel.to_string(), muted);
        Ok(())
    }

    /// Solo one engine channel in place, using the same non-blocking path
    /// as its fader. The button lights before the next status.
    pub fn set_solo(&mut self, channel: &str, solo: bool) -> Result<(), String> {
        self.post(format!("solo:{channel}"), json!({"op": "solo", "channel": channel, "solo": solo}));
        self.solos.insert(channel.to_string(), solo);
        Ok(())
    }

    /// Solo the Booth as one strip. Deck channels follow it.
    pub fn set_dj_solo(&mut self, solo: bool) -> Result<(), String> {
        self.post("solo:dj".into(), json!({"op": "solo", "channel": "dj", "solo": solo}));
        self.dj_solo = solo;
        Ok(())
    }

    /// The DJ Booth's fader and mute on the Patch mix. The fader queues
    /// (a drag never waits); the mute click rides the same queue.
    pub fn set_dj_group(&mut self, db: Option<f32>, mute: Option<bool>) -> Result<(), String> {
        if let Some(d) = db {
            self.post("dj:gain".into(), json!({"op": "dj_group", "gain_db": d}));
            self.dj_group.0 = d;
        }
        if let Some(m) = mute {
            self.post("dj:mute".into(), json!({"op": "dj_group", "mute": m}));
            self.dj_group.1 = m;
        }
        Ok(())
    }

    /// The Patch mix's fader and mute. The fader queues (a drag never
    /// waits); the mute click rides the same queue.
    pub fn set_master(&mut self, db: Option<f32>, mute: Option<bool>) -> Result<(), String> {
        if let Some(d) = db {
            self.post("master:gain".into(), json!({"op": "master", "db": d}));
            self.master.0 = d;
        }
        if let Some(m) = mute {
            self.post("master:mute".into(), json!({"op": "master", "mute": m}));
            self.master.1 = m;
        }
        Ok(())
    }

    /// Send one deck to or from the separate headphone cue mix.
    pub fn set_pfl(&mut self, channel: &str, on: bool) -> Result<(), String> {
        self.post(format!("pfl:{channel}"), json!({"op": "pfl", "channel": channel, "on": on}));
        self.pfls.insert(channel.to_string(), on);
        Ok(())
    }

    pub fn reset_loudness(&mut self) {
        let _ = self.call(json!({"op": "reset_loudness"}));
    }
}

fn db_to_lin(db: f32) -> f32 {
    if db <= SILENCE_DB {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_peaks_survive_closed_faders_and_clear_on_new_silent_audio() {
        let mut feed = EngineFeed { channels: ["deck-c", "deck-a", "deck-b", "deck-d"].map(String::from).to_vec(), ..Default::default() };
        let block = |nsec, ch| Block {
            nsec, ch, out: [SILENCE_DB; 2], abs: vec![],
            gain: vec![[0.0; 2], [0.5; 2], [1.0; 2], [0.0; 2]],
            secs: 256.0 / 48000.0, decks: vec![], loads: vec![],
            dj: [SILENCE_DB; 2], dj_abs: [0.0; 2], bus: [[SILENCE_DB; 2]; 4],
        };
        let peaks = vec![[6.0, 3.0], [-3.0, -4.0], [-12.0, -13.0], [-18.0, -19.0]];
        feed.blocks.push_back(block(1, peaks.clone()));
        let (heard, _) = feed.take_due(0);
        assert_eq!(feed.input_ch, peaks, "actual input peaks, including overloads with a closed fader");
        assert_eq!(heard[0], [SILENCE_DB; 2]);
        assert!((heard[1][0] + 9.0206).abs() < 0.001);
        assert_eq!(heard[2], [-12.0, -13.0]);
        assert_eq!(heard[3], [SILENCE_DB; 2]);
        feed.take_due(0);
        assert_eq!(feed.input_ch, peaks, "between blocks the last measurement stays stable");
        feed.blocks.push_back(block(2, vec![[SILENCE_DB; 2]; 4]));
        feed.take_due(0);
        assert_eq!(feed.input_ch, vec![[SILENCE_DB; 2]; 4], "a new silent block must not inherit an old maximum");
    }

    /// A drag posts a value per pointer move: the queue keeps one write
    /// per knob, the latest value (one write per knob per UI frame).
    #[test]
    fn a_drag_coalesces_to_one_write_keeping_the_latest_value() {
        let mut feed = EngineFeed::default();
        feed.set_control("app-1", "high", 1.0).unwrap();
        feed.set_control("app-1", "high", 2.5).unwrap();
        assert_eq!(feed.writes.len(), 1, "one write for the knob");
        let (_, req) = feed.writes.iter().next().unwrap();
        assert_eq!(req["high_db"], json!(2.5), "the latest value");
        // The knob is where it was dragged, before any status.
        assert_eq!(feed.controls["app-1"].high_db, 2.5);
    }

    #[test]
    fn different_knobs_keep_their_own_writes() {
        let mut feed = EngineFeed::default();
        feed.set_control("app-1", "high", 1.0).unwrap();
        feed.set_gain("app-1", -3.0).unwrap();
        feed.set_control("app-1", "low", -1.0).unwrap();
        assert_eq!(feed.writes.len(), 3);
    }

    /// A send level drag and a pre/post flip beside it are different
    /// fields: neither may swallow the other when both are queued.
    #[test]
    fn a_send_level_and_its_pre_flip_are_separate_writes() {
        let mut feed = EngineFeed::default();
        feed.set_send("bus", 0, Some(Some(-3.0)), Some(true)).unwrap();
        assert_eq!(feed.writes.len(), 2);
        let reqs: Vec<&Value> = feed.writes.values().collect();
        assert!(reqs.iter().any(|r| r.get("db").is_some()));
        assert!(reqs.iter().any(|r| r.get("pre").is_some()));
        // And a later level keeps its own slot.
        feed.set_send("bus", 0, Some(Some(-6.0)), None).unwrap();
        assert_eq!(feed.writes.len(), 2, "still two writes, level updated");
    }

    #[test]
    fn a_slow_engine_retains_only_the_latest_drag_until_the_batch_finishes() {
        let (tx, requests) = std::sync::mpsc::channel();
        let (_, rx) = std::sync::mpsc::channel();
        let mut fetch = Fetcher { tx, rx, inflight: 1 };
        let mut feed = EngineFeed::default();
        for i in 0..1000 {
            feed.set_gain("app-1", -(i as f32)).unwrap();
            assert!(!fetch.send_writes(&mut feed.writes));
        }
        feed.set_mute("app-1", true).unwrap();
        assert_eq!(feed.writes.len(), 2);
        assert!(requests.try_recv().is_err());
        fetch.inflight = 0;
        assert!(fetch.send_writes(&mut feed.writes));
        let sent: Vec<_> = requests.try_iter().map(|(_, value)| value).collect();
        assert_eq!(sent.len(), 2);
        assert!(sent.iter().any(|v| v["db"] == -999.0));
        assert!(sent.iter().any(|v| v["muted"] == true));
        assert_eq!(fetch.inflight, 2);
    }

    #[test]
    fn an_inflight_status_cannot_pull_back_an_unsent_native_control() {
        let mut feed = EngineFeed::default();
        let snapshot = json!({"channels": [
            {"name": "stream-1", "gain_db": -30.0, "mute": false, "eq": {"low_db": 0.0}, "sends": [{"db": null, "pre": true}, {"db": null, "pre": true}, {"db": null, "pre": false}, {"db": null, "pre": false}]},
            {"name": "stream-2", "gain_db": -9.0, "mute": false}
        ]});
        feed.set_gain("stream-1", -6.0).unwrap();
        feed.set_mute("stream-1", true).unwrap();
        feed.set_solo("stream-1", true).unwrap();
        feed.set_pfl("stream-1", true).unwrap();
        feed.set_control("stream-1", "low", -3.0).unwrap();
        feed.set_send("stream-1", 0, Some(Some(-4.0)), None).unwrap();
        feed.apply_status(snapshot.clone());
        assert_eq!(feed.gains["stream-1"], -6.0);
        assert!(feed.mutes["stream-1"]);
        assert!(feed.solos["stream-1"] && feed.pfls["stream-1"]);
        assert_eq!(feed.controls["stream-1"].low_db, -3.0);
        assert_eq!(feed.controls["stream-1"].sends.unwrap()[0].db, Some(-4.0));
        assert_eq!(feed.gains["stream-2"], -9.0);
        feed.writes.clear();
        feed.apply_status(snapshot);
        assert_eq!(feed.gains["stream-1"], -30.0);
        assert!(!feed.mutes["stream-1"]);
    }

    #[test]
    fn queued_native_controls_keep_their_original_owner_and_are_dropped_on_reuse() {
        let old = patch_core::stream::key(1, Some(2), "3");
        let new = patch_core::stream::key(4, Some(2), "3");
        let mut feed = EngineFeed::default();
        feed.bind_stream_channels(&[(old.clone(), "stream-1".into())].into());
        feed.set_gain("stream-1", -30.0).unwrap();
        let inflight = feed.writes.values().next().unwrap().clone();
        assert_eq!(inflight["stream_key"], old);
        feed.bind_stream_channels(&[(new.clone(), "stream-1".into())].into());
        assert!(feed.writes.is_empty());
        feed.set_mute("stream-1", true).unwrap();
        assert_eq!(feed.writes.values().next().unwrap()["stream_key"], new);
        assert_eq!(inflight["stream_key"], old);
    }

    #[test]
    fn rejecting_a_departed_callers_write_does_not_disconnect_the_engine() {
        assert!(fetched_reply("write", Ok(Err("playback stream no longer owns this lane".into()))).is_some());
        assert!(fetched_reply("write", Err("broken pipe".into())).is_none());
    }

    #[test]
    fn an_unknown_control_is_rejected_and_queues_nothing() {
        let mut feed = EngineFeed::default();
        assert!(feed.set_control("app-1", "warp", 0.5).is_err());
        assert!(feed.writes.is_empty());
    }

    #[test]
    fn a_window_that_just_opened_leaves_routing_to_the_engine() {
        let mut feed = EngineFeed::default();
        // No answer yet: not gone.
        assert!(!feed.absent());
        feed.silent_since = Some(Instant::now());
        assert!(!feed.absent());
        // Failing for longer than 3 s: gone, the window takes over.
        feed.silent_since = Instant::now().checked_sub(Duration::from_secs(4));
        assert!(feed.absent());
    }
}
