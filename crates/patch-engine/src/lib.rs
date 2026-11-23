//! Patch engine: the one real-time audio engine of Patch.
//!
//! A native PipeWire filter node ("Patch Engine", `patch.engine`) with a
//! stereo input pair per named channel (`bus`, `deck-a` … `deck-d`, …) and a
//! stereo program output that the engine links to the output device itself.
//! It is its own detached process: Patch's UI can quit, crash or restart and
//! the program keeps playing.
//!
//! On the real-time thread, once per graph cycle, it
//! * mixes the channels with per-channel gain ramps (x86-64 assembly,
//!   through the kernel ABI (`patch_dsp::registry`, AVX2 when this CPU has
//!   it and `omarchy-patch optimize` kept it), bit-identical to the
//!   reference — no zipper noise;
//! * measures every channel and the program (sample peak, assembly kernel);
//! * publishes one [`MeterBlock`] stamped with the graph clock
//!   ([`patch_model::timing::BlockStamp`]) and the program audio, each
//!   through a preallocated lock-free [`ring::Ring`].
//!
//! It never allocates, locks or blocks there. VU, EBU R128 loudness, true
//! peak and the spectrum run on the [`analysis`] thread from the program ring.

mod overlap;
pub mod analysis;
pub mod aux_fx;
pub mod automix;
pub mod track_analysis;
pub mod client;
pub mod daemon;
pub mod deck;
pub mod deck_sample;
pub mod eq;
pub mod events;
mod folder_watch;
mod ipc_line;
pub mod fx;
pub mod latency;
pub mod librarian;
pub mod limiter;
pub mod links;
mod device_properties;
pub mod master_eq;
pub mod instrument;
pub mod pattern_service;
pub mod sample_bank;
pub mod sampler;
pub mod material_service;
pub mod performance;
pub mod rehearse;
pub mod ring;
pub mod sequencer;
pub mod session;
pub mod shm;
pub mod surround;
mod timeline;
pub mod voice;

use patch_model::timing::BlockStamp;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

pub const MAX_CHANNELS: usize = 64;
// Solo state is one bit per channel in a u64.
const _: () = assert!(MAX_CHANNELS <= 64);
pub mod stream_lanes;
/// The Mixer's send buses: AUX 1, AUX 2 (mixes of their own, to an output
/// and a recordable source), FX 1 (reverb) and FX 2 (delay), whose returns
/// play in the program.
pub const BUSES: usize = 4;
pub const BUS_NAMES: [&str; BUSES] = ["aux1", "aux2", "fx1", "fx2"];
/// The send slot of the DJ Booth (the decks' group), after the channels.
pub const DJ_SEND: usize = MAX_CHANNELS;
const TUNE_FILTERS: usize = patch_tune::MAX_FILTERS;
/// See `Params::bus_watch`. One lock: a change can't land after system
/// audio went off.
#[derive(Default)]
pub struct BusWatch {
    /// Patch is the system's audio: the sink's volume is the system volume.
    pub live: bool,
    /// Last reported (linear per side, mute); None until the first report.
    pub volume: Option<([f32; 2], bool)>,
}

/// Largest graph quantum the engine buffers for (PipeWire's max is 8192).
pub const MAX_FRAMES: usize = 8192;

pub fn has_avx2() -> bool {
    patch_dsp::allows(patch_dsp::Tier::Avx2)
}

/// What mixes, e.g. "mix_ramp avx2 · peak avx2 · interleave2 reference".
pub fn backend() -> String {
    let t = patch_dsp::registry::table();
    patch_dsp::abi::Op::ALL
        .iter()
        .map(|op| format!("{} {}", op.name(), t.get(*op).name))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// out[n] += input[n] · (g0 + n·dg), through the kernel table.
#[inline]
pub fn mix(out: &mut [f32], input: &[f32], g0: f32, dg: f32) {
    patch_dsp::registry::mix_ramp(out, input, g0, dg);
}

/// NaN and ±infinity become silence (finite samples are untouched).
#[inline]
fn finite_or_silent(x: &mut [f32]) {
    for v in x {
        *v = if v.is_finite() { *v } else { 0.0 };
    }
}

/// Sample peak |x| of one mono buffer (kernel table).
#[inline]
fn peak(x: &[f32]) -> f32 {
    patch_dsp::registry::peak(x)
}

fn load(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

fn store(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Ordering::Relaxed);
}

/// The graph clock of one cycle, as the C shim passes it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Clock {
    pub position: u64,
    pub nsec: u64,
    pub rate: u32,
    pub frames: u32,
    /// The driver's delay between position and hardware, samples: what
    /// the output device holds before the DAC (0: the driver says nothing).
    pub delay: i64,
    /// The cycle's SPA_IO_CLOCK_FLAG_* (bit 1: this node missed the last
    /// cycle's deadline).
    pub flags: u32,
    /// The driver's accumulated xrun time, samples (0: it keeps none).
    pub xrun: u64,
}

impl Clock {
    pub fn stamp(&self) -> BlockStamp {
        BlockStamp {
            sample_pos: self.position,
            nsec: self.nsec,
            rate: self.rate,
            frames: self.frames,
        }
    }
}

/// Meter data of one graph cycle: sample peak (linear) per channel side and
/// of the program, stamped with the cycle's clock.
#[derive(Clone, Copy, Debug)]
pub struct MeterBlock {
    pub stamp: BlockStamp,
    pub ch_peak: [[f32; 2]; MAX_CHANNELS],
    /// Mean rectified level (mean |x|) per channel and side, pre-fader:
    /// what a VU meter integrates (the program's VU runs on the analysis
    /// thread; channels get theirs from this).
    pub ch_abs: [[f32; 2]; MAX_CHANNELS],
    /// The channel's gain in this cycle (fader × crossfader × mute, linear,
    /// the larger end of its ramp): pre-fader × gain = what it adds to the
    /// mix, so every channel meter can show its real contribution.
    pub ch_gain: [[f32; 2]; MAX_CHANNELS],
    pub out_peak: [f32; 2],
    /// Native decks: source position (frames at the graph rate) of the
    /// first frame this cycle played, and source frames per output frame
    /// (0 = stopped); NaN when the deck has no track.
    pub deck_pos: [f64; MAX_DECKS],
    pub deck_step: [f32; MAX_DECKS],
    /// Per deck, its load count (`deck::worker::Shared::loads`) this
    /// cycle: which track `deck_pos` is in.
    pub deck_load: [u32; MAX_DECKS],
    /// The DJ Booth's pre-master-limiter sum (decks after their faders,
    /// crossfader, and group's fader): peak and mean |x| per side. It may
    /// exceed 0 dBFS when two decks overlap; `out_peak` is the protected
    /// program output and is capped by the master limiter.
    pub dj_peak: [f32; 2],
    pub dj_abs: [f32; 2],
    /// Each bus as it leaves: AUX after its master, FX after its return
    /// fader (peak per side).
    pub bus_peak: [[f32; 2]; BUSES],
}

impl Default for MeterBlock {
    fn default() -> Self {
        MeterBlock {
            stamp: BlockStamp::default(),
            ch_peak: [[0.0; 2]; MAX_CHANNELS],
            ch_abs: [[0.0; 2]; MAX_CHANNELS],
            ch_gain: [[0.0; 2]; MAX_CHANNELS],
            out_peak: [0.0; 2],
            deck_pos: [f64::NAN; MAX_DECKS],
            deck_load: [0; MAX_DECKS],
            deck_step: [0.0; MAX_DECKS],
            dj_peak: [0.0; 2],
            dj_abs: [0.0; 2],
            bus_peak: [[0.0; 2]; BUSES],
        }
    }
}

/// Master limiter ceiling (sample peak): 1 dB of room for inter-sample
/// peaks in the DAC's reconstruction. (At −0.5 the output read +0.1 dBTP
/// on hot house masters, 2026-10-13.)
pub const MASTER_CEILING_DBFS: f32 = -1.0;

/// Native decks the engine can run (channels named `deck-a`, `deck-b`, …).
pub const MAX_DECKS: usize = 4;

/// A channel the engine synthesises itself: it has no input port, its audio
/// comes from the step Sequencer's drum bank or from the Pattern Unit (its
/// drum voices and the derived material it plays). Which channel is which
/// comes from the engine's channel list, so the mixer sees both as strips.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Internal {
    Sequencer,
    Pattern,
    /// The Tidal Unit's instrument (drums, synths, its delay and room).
    Tidal,
}

/// Parameters shared between the control thread and the RT thread (atomics
/// only), plus the rings the RT thread publishes into.
pub struct Params {
    pub sequencer: sequencer::Shared,
    /// The Tidal Unit's events, for the instrument (their own queue: the
    /// Tidal service is its only producer).
    pub tidal: instrument::Shared,
    /// Derived material the Pattern Unit plays: the control thread's side
    /// of the sampler (installs, retires, what plays).
    pub sampler: sampler::Shared,
    pub names: Vec<String>,
    /// Fixed channel classification for real-time solo checks.
    mic_channels: [bool; MAX_CHANNELS],
    /// Fader gain per channel, linear.
    pub gain: [AtomicU32; MAX_CHANNELS],
    channel_epoch: [AtomicU32; MAX_CHANNELS],
    /// Native playback lanes (`stream-N`), precomputed for the callback.
    stream_channels: [bool; MAX_CHANNELS],
    /// The lane's last pre-fader signal: its owner's epoch (high 32 bits)
    /// and the callback cycle (low 32 bits). Another epoch: never heard.
    stream_signal: [AtomicU64; MAX_CHANNELS],
    /// The user moved one of this lane's controls (not a seeded value).
    stream_touched: [AtomicBool; MAX_CHANNELS],
    /// Control-plane ownership only; never locked by the audio callback.
    pub stream_owners: [std::sync::Mutex<Option<String>>; MAX_CHANNELS],
    /// Changes due at an exact graph sample (the AI DJ's moves on the beat):
    /// the RT thread applies each at its sample, splitting the block there.
    pub timed: [TimedWrite; TIMED_SLOTS],
    pub mute: [AtomicBool; MAX_CHANNELS],
    /// Solo-in-place. A mic solo keeps the DJ Booth in the programme under
    /// the mic; other unsoloed sources drop out. Mute still wins.
    /// Soloed channels, one bit each: "is anything soloed" is one load,
    /// not 64 (it was asked per channel and send bus every cycle).
    solo: AtomicU64,
    /// The mic channels' bits in `solo`.
    mic_mask: u64,
    /// Input channels always feed capture; speaker monitoring is independent.
    pub input_monitor: [AtomicBool; MAX_CHANNELS],
    /// Input channels in the Patch Input mix (what calls and streams record).
    /// Off for a remote voice (a call's far end on a mic lane): mix-minus, so
    /// the people in the call never hear themselves back.
    pub input_send: [AtomicBool; MAX_CHANNELS],
    pub input_main: AtomicBool,
    /// Pan −1 … +1 per channel.
    pub pan: [AtomicU32; MAX_CHANNELS],
    /// Input trim (the console's GAIN) per channel, linear: on the input,
    /// before voice processing, EQ, PFL and the fader. 1 = unity, no pass.
    pub trim: [AtomicU32; MAX_CHANNELS],
    /// Sends per channel (and the DJ Booth at `DJ_SEND`) to each bus,
    /// linear (0 = off, no pass), and whether each is pre-fader.
    pub sends: [[AtomicU32; BUSES]; MAX_CHANNELS + 1],
    pub send_pre: [[AtomicBool; BUSES]; MAX_CHANNELS + 1],
    /// Each bus's master (AUX) or return (FX) fader, linear, and mute.
    pub bus_gain: [AtomicU32; BUSES],
    pub bus_mute: [AtomicBool; BUSES],
    /// FX 1's size 0…1; FX 2's time (ms) and feedback 0…0.9.
    pub reverb_size: AtomicU32,
    pub delay_ms: AtomicU32,
    pub delay_feedback: AtomicU32,
    pub master: AtomicU32,
    /// The DJ Booth as one channel on the main mix: its fader (linear) and
    /// mute, on every deck channel at once (a VCA: no extra pass).
    pub dj_gain: AtomicU32,
    pub dj_mute: AtomicBool,
    /// Solo the Booth as one strip. Deck channels follow this, not `solo`.
    pub dj_solo: AtomicBool,
    /// The Patch mix's mute (its fader is `master`).
    pub master_mute: AtomicBool,
    /// The system volume (the `patch` sink's, set from the bar and media
    /// keys when Patch is the system's audio), linear per side, and its
    /// mute. Applied to every channel but the bus: the bus is read from
    /// that sink's monitor, which already carries it.
    pub system: [AtomicU32; 2],
    pub system_mute: AtomicBool,
    /// The system volume after the recording tap: the bus's monitor is free
    /// of it (the daemon set that), so it's applied once, on what leaves
    /// for the device. Off: in every channel's gain, as before (recordings
    /// and streams then follow the menu bar's volume).
    pub system_post: AtomicBool,
    /// The `patch` sink's volume as the engine's own PipeWire listener
    /// reports it, and whether it is the system volume now.
    pub bus_watch: std::sync::Mutex<BusWatch>,
    /// Which surround pairs carry the decoded surround (see surround.rs),
    /// for the device the program plays on.
    pub surround_psd: Arc<std::sync::atomic::AtomicU8>,
    /// The bus channel's index (usize::MAX when there's none).
    pub bus_ch: usize,
    /// DJ EQ per channel: band gains (linear, 0 = kill) and filter knob.
    pub eq: [[AtomicU32; 4]; MAX_CHANNELS],
    /// Headphone cue (PFL) per channel: post-EQ, pre-fader, pre-crossfader.
    pub pfl: [AtomicBool; MAX_CHANNELS],
    /// Headphone blend: 0 = cue only … 1 = program only.
    pub cue_mix: AtomicU32,
    /// Headphone level, linear.
    pub cue_level: AtomicU32,
    pub cycles: AtomicU64,
    pub last_rate: AtomicU32,
    pub last_frames: AtomicU32,
    /// The output device's delay to the DAC, frames at `last_rate` (f64
    /// bits), smoothed over about a second: the driver's buffer fill swings
    /// by a quantum each cycle. 0 while the driver reports none.
    pub out_delay: AtomicU64,
    /// Callback cost, ns: sum and count since the last read, and the worst.
    pub cost_sum: AtomicU64,
    pub cost_n: AtomicU64,
    pub cost_max: AtomicU64,
    /// Callbacks that took longer than their cycle (a deadline missed),
    /// since the engine started.
    pub overruns: AtomicU64,
    /// Cycles PipeWire marks as after an xrun (this node missed a deadline,
    /// or the driver's xrun time grew), since the engine started.
    pub xruns: AtomicU64,
    /// Cycles that started over two cycles after the one before: the graph
    /// woke too late and the device played out what it held.
    pub gaps: AtomicU64,
    /// Skips (xruns, gaps, late callbacks) while Patch was sounding: on its
    /// output or headphones within half a second. Silence can't be heard
    /// skipping; the bar's health dot counts only these.
    pub heard_dry: AtomicU64,
    pub heard_late: AtomicU64,
    /// The worst callback since the health watch last looked (it takes it).
    pub cost_peak: AtomicU64,
    /// Every callback's cost since the engine started, by power of two of
    /// nanoseconds (bucket k: below 2^k ns): percentiles without a lock.
    pub cost_hist: [AtomicU64; 32],
    pub meters: Ring<MeterBlock>,
    /// Native decks, and which channel each plays on.
    pub decks: Vec<Arc<deck::worker::Shared>>,
    pub deck_of: [Option<usize>; MAX_CHANNELS],
    /// Program output, interleaved stereo, for the analysis thread.
    pub program: Ring<f32>,
    pub capture: capture::Capture,
    /// Output latency measurement (probe out, microphone in).
    pub latency: latency::Shared,
    /// Beat FX per channel: kind, on, beats, depth.
    pub fx_kind: [AtomicU32; MAX_CHANNELS],
    pub fx_on: [AtomicBool; MAX_CHANNELS],
    pub fx_beats: [AtomicU32; MAX_CHANNELS],
    pub fx_depth: [AtomicU32; MAX_CHANNELS],
    /// Off-thread FX buffer preparation and reclamation.
    pub fx_pool: Arc<fx::Pool>,
    /// One-way isolated stem bridge. Audio is tapped in the deck worker,
    /// processed by a dedicated RT FX instance, then returned to the other
    /// deck's mixer lane (after its own FX to prevent feedback).
    pub stem_bridge_on: AtomicBool,
    pub stem_bridge_source: AtomicU32,
    pub stem_bridge_stem: AtomicU32,
    /// Optional source-deck Beat FX on the isolated stem send. The send is
    /// clean by default; this switch never changes the source deck's FX.
    pub stem_bridge_fx: AtomicBool,
    /// Independent wet amount for the optional bridge FX, 0…1.
    pub stem_bridge_depth: AtomicU32,
    /// Native decks' beat grid in source frames (beat length, first beat);
    /// 0 = no grid. Beat FX lock to it.
    pub grid_beat: [AtomicU64; MAX_DECKS],
    pub grid_offset: [AtomicU64; MAX_DECKS],
    /// Which beat of the grid (0-3) starts a bar; 255 when unknown.
    pub grid_downbeat: [AtomicU32; MAX_DECKS],
    /// Compiled, validated output correction coefficients. A zero filter count disables EQ.
    pub tune_filter_count: AtomicU32,
    pub tune_coefficients: [[AtomicU64; 5]; TUNE_FILTERS],
    tune_generation: AtomicU64,
    pub tune_headroom: AtomicU32,
    pub tune_protection: AtomicBool,
    /// The master limiter (on; off only to measure the raw mix).
    pub master_limiter: AtomicBool,
    /// The master EQ's settings (the EQ Unit).
    pub master_eq: master_eq::Published,
    /// Each channel's EQ (bus, decks …): in the strip, before the fader.
    pub channel_eq: Vec<master_eq::Published>,
    /// Voice processing per channel (microphones): on, high-pass Hz, gate
    /// threshold dBFS (NaN: off), de-esser and compressor amounts.
    pub voice: Vec<voice::Published>,
    pub mic_meter: Vec<mic_meter::Published>,
    pub tune_ceiling: AtomicU32,
}

use ring::Ring;

/// One pending change: `kind` 0 is the channel's gain, 1–4 its EQ (low,
/// mid, high, filter), to `value` at graph sample `at`, gliding there over
/// the `glide` samples before it (0: a step, still click-free).
#[derive(Default)]
pub struct TimedWrite {
    armed: AtomicBool,
    at: AtomicU64,
    glide: AtomicU32,
    ch: AtomicU32,
    kind: AtomicU32,
    value: AtomicU32,
}

/// A timed change under way on the RT thread: from `from` at sample
/// `start` in a straight line to `to` at `end`.
#[derive(Clone, Copy, Default)]
struct Glide {
    active: bool,
    from: f32,
    to: f32,
    start: u64,
    end: u64,
}

impl Glide {
    /// The value at sample `t`, or None when not gliding (the stored value
    /// applies). A value stored since (by hand, a cancelled plan) ends it.
    fn at(&mut self, t: u64, stored: f32) -> Option<f32> {
        if !self.active {
            return None;
        }
        if stored.to_bits() != self.to.to_bits() {
            self.active = false;
            return None;
        }
        if t >= self.end {
            self.active = false;
            return Some(self.to);
        }
        let f = t.saturating_sub(self.start) as f64 / (self.end - self.start).max(1) as f64;
        Some(self.from + (self.to - self.from) * f as f32)
    }
}

/// Pieces a gliding channel's block is processed in: settings are exact at
/// each piece's end, ramped in between (64 samples, as the EQ redesigns).
const GLIDE_PIECE: usize = 64;
const MAX_PIECES: usize = MAX_FRAMES / GLIDE_PIECE + 40;

pub const TIMED_SLOTS: usize = 64;
pub const TIMED_GAIN: u32 = 0;

/// A change the RT thread starts this block, at `frame`, to land at
/// graph sample `end`.
#[derive(Clone, Copy, Default)]
struct Due {
    frame: usize,
    end: u64,
    ch: usize,
    kind: u32,
    value: f32,
}

/// Samples a timed change takes to reach its value (1.3 ms at 48 kHz):
/// on the beat, without a click.
const TIMED_RAMP: usize = 64;

impl Params {
    /// Queue a change for graph sample `at`. False when the queue is full
    /// (the caller applies it at once instead).
    pub fn post_at(&self, at: u64, ch: usize, kind: u32, value: f32) -> bool {
        self.post_glide(at, 0, ch, kind, value)
    }

    /// Queue a change that glides over the `glide` samples before `at` and
    /// lands there (a ramp's step). Started late, it glides over what is
    /// left and still lands on `at`.
    pub fn post_glide(&self, at: u64, glide: u32, ch: usize, kind: u32, value: f32) -> bool {
        for s in &self.timed {
            if !s.armed.load(Ordering::Acquire) {
                s.at.store(at, Ordering::Relaxed);
                s.glide.store(glide, Ordering::Relaxed);
                s.ch.store(ch as u32, Ordering::Relaxed);
                s.kind.store(kind, Ordering::Relaxed);
                s.value.store(value.to_bits(), Ordering::Relaxed);
                s.armed.store(true, Ordering::Release);
                return true;
            }
        }
        false
    }

    /// A gain for channel `ch` now, from outside a timed step, while one is
    /// still due (posted up to ~120 ms ahead): it goes into the latest
    /// pending one instead, so the scheduled move keeps its sample. Earlier
    /// pending ones (a ramp's steps) keep theirs. False when none is pending.
    pub fn retarget_timed_gain(&self, ch: usize, value: f32) -> bool {
        let latest = self
            .timed
            .iter()
            .filter(|s| {
                s.armed.load(Ordering::Acquire)
                    && s.ch.load(Ordering::Relaxed) as usize == ch
                    && s.kind.load(Ordering::Relaxed) == TIMED_GAIN
            })
            .max_by_key(|s| s.at.load(Ordering::Relaxed));
        match latest {
            Some(s) => {
                s.value.store(value.to_bits(), Ordering::Relaxed);
                // Applied meanwhile: then it is a plain change after all.
                s.armed.load(Ordering::Acquire)
            }
            None => false,
        }
    }

    /// Drop every pending change (a plan was cancelled: the DJ took over).
    pub fn clear_timed(&self) {
        for s in &self.timed {
            s.armed.store(false, Ordering::Release);
        }
    }

    /// Gain on the combined DJ output, after deck processing and effects.
    fn solo_engaged(&self) -> bool {
        self.dj_solo.load(Ordering::Relaxed) || self.solo.load(Ordering::Relaxed) != 0
    }

    /// A live mic solo must leave the DJ programme in place: the mic is being
    /// checked in the mix, not used to cut the music underneath it.
    fn mic_solo_engaged(&self) -> bool {
        self.solo.load(Ordering::Relaxed) & self.mic_mask != 0
    }

    /// The Booth is out while muted, or while soloing another strip, except
    /// that it stays under a soloed live mic.
    fn booth_muted(&self) -> bool {
        self.dj_mute.load(Ordering::Relaxed)
            || (self.solo_engaged()
                && !self.dj_solo.load(Ordering::Relaxed)
                && !self.mic_solo_engaged())
    }

    fn booth_gain(&self) -> f32 {
        if self.booth_muted() {
            0.0
        } else {
            load(&self.dj_gain)
        }
    }

    pub fn set_dj_group(&self, linear: f32, mute: bool) {
        store(&self.dj_gain, fader_floor(linear).clamp(0.0, 4.0));
        self.dj_mute.store(mute, Ordering::Relaxed);
    }

    fn side_gains_for(&self, ch: usize, linear: f32) -> [f32; 2] {
        // This channel's own gain, and no other: the Patch Mix's fader is one
        // scalar on the summed mix (the output stage applies it), so a
        // channel's gain — and every meter read off it — is the channel's
        // own. Folded in here, the master moved the DJ Booth's level and the
        // decks' meters in the crossfader whenever the main fader moved.
        let g = if self.heard(ch) { linear } else { 0.0 };
        let p = load(&self.pan[ch]).clamp(-1.0, 1.0);
        let fade = |x: f32| (x * std::f32::consts::FRAC_PI_2).cos();
        let sys = self.system_gains(ch);
        [
            g * sys[0] * if p > 0.0 { fade(p) } else { 1.0 },
            g * sys[1] * if p < 0.0 { fade(-p) } else { 1.0 },
        ]
    }

    /// The system volume on channel `ch` (unity on the bus: its sink's
    /// monitor carries it already).
    fn system_gains(&self, ch: usize) -> [f32; 2] {
        if ch == self.bus_ch || self.system_post.load(Ordering::Relaxed) {
            return [1.0, 1.0];
        }
        if self.system_mute.load(Ordering::Relaxed) {
            return [0.0, 0.0];
        }
        [load(&self.system[0]), load(&self.system[1])]
    }

    /// The system volume: linear per side (`patch` sink's channel volumes),
    /// and mute. Unity and unmuted when Patch isn't the system's audio.
    pub fn set_system(&self, left: f32, right: f32, mute: bool) {
        store(&self.system[0], left.clamp(0.0, 4.0));
        store(&self.system[1], right.clamp(0.0, 4.0));
        self.system_mute.store(mute, Ordering::Relaxed);
    }

    /// The `patch` sink's volume changed (the engine's PipeWire listener,
    /// at once): the system volume while Patch is the system's audio. No
    /// process in between: a `pactl` read took ~100 ms on a slow CPU and
    /// most of a second when its pages had to come back from swap.
    pub fn bus_volume_changed(&self, left: f32, right: f32, mute: bool) {
        let mut w = self.bus_watch.lock().unwrap();
        w.volume = Some(([left, right], mute));
        if w.live {
            self.set_system(left, right, mute);
        }
    }

    pub fn system(&self) -> ([f32; 2], bool) {
        (
            [load(&self.system[0]), load(&self.system[1])],
            self.system_mute.load(Ordering::Relaxed),
        )
    }

    /// The system volume as the output stage applies it (0 muted).
    fn system_out(&self) -> [f32; 2] {
        if self.system_mute.load(Ordering::Relaxed) {
            [0.0; 2]
        } else {
            [load(&self.system[0]), load(&self.system[1])]
        }
    }

    pub fn new(names: &[&str]) -> Self {
        Params {
            sequencer: sequencer::Shared::default(),
            tidal: instrument::Shared::default(),
            sampler: sampler::Shared::default(),
            names: names
                .iter()
                .take(MAX_CHANNELS)
                .map(|s| s.to_string())
                .collect(),
            mic_channels: std::array::from_fn(|i| {
                names.get(i).is_some_and(|name| name.starts_with("mic-"))
            }),
            channel_epoch: std::array::from_fn(|_| AtomicU32::new(0)),
            stream_channels: std::array::from_fn(|i| {
                names.get(i).is_some_and(|name| name.starts_with("stream-"))
            }),
            stream_signal: std::array::from_fn(|_| AtomicU64::new(u64::MAX)),
            stream_touched: std::array::from_fn(|_| AtomicBool::new(false)),
            stream_owners: std::array::from_fn(|_| std::sync::Mutex::new(None)),
            gain: std::array::from_fn(|i| {
                let silent_performance_lane = names
                    .get(i)
                    .is_some_and(|name| matches!(*name, "deck-c" | "deck-d"));
                AtomicU32::new(if silent_performance_lane {
                    0.0f32.to_bits()
                } else {
                    1.0f32.to_bits()
                })
            }),
            timed: std::array::from_fn(|_| TimedWrite::default()),
            mute: std::array::from_fn(|_| AtomicBool::new(false)),
            solo: AtomicU64::new(0),
            mic_mask: (0..names.len().min(MAX_CHANNELS))
                .filter(|&i| names[i].starts_with("mic-"))
                .fold(0, |m, i| m | 1u64 << i),
            input_monitor: std::array::from_fn(|_| AtomicBool::new(true)),
            input_send: std::array::from_fn(|_| AtomicBool::new(true)),
            input_main: AtomicBool::new(false),
            pan: std::array::from_fn(|_| AtomicU32::new(0.0f32.to_bits())),
            trim: std::array::from_fn(|_| AtomicU32::new(1.0f32.to_bits())),
            sends: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU32::new(0))),
            // AUX sends start pre-fader (monitor mixes), FX sends post.
            send_pre: std::array::from_fn(|_| std::array::from_fn(|b| AtomicBool::new(b < 2))),
            bus_gain: std::array::from_fn(|_| AtomicU32::new(1.0f32.to_bits())),
            bus_mute: std::array::from_fn(|_| AtomicBool::new(false)),
            reverb_size: AtomicU32::new(0.5f32.to_bits()),
            delay_ms: AtomicU32::new(375.0f32.to_bits()),
            delay_feedback: AtomicU32::new(0.35f32.to_bits()),
            master: AtomicU32::new(1.0f32.to_bits()),
            dj_gain: AtomicU32::new(1.0f32.to_bits()),
            dj_mute: AtomicBool::new(false),
            dj_solo: AtomicBool::new(false),
            master_mute: AtomicBool::new(false),
            system: [
                AtomicU32::new(1.0f32.to_bits()),
                AtomicU32::new(1.0f32.to_bits()),
            ],
            system_mute: AtomicBool::new(false),
            system_post: AtomicBool::new(false),
            bus_watch: Default::default(),
            surround_psd: Arc::new(std::sync::atomic::AtomicU8::new(0)),
            bus_ch: names.iter().position(|n| *n == "bus").unwrap_or(usize::MAX),
            eq: std::array::from_fn(|_| {
                [1.0f32, 1.0, 1.0, 0.0].map(|v| AtomicU32::new(v.to_bits()))
            }),
            pfl: std::array::from_fn(|_| AtomicBool::new(false)),
            cue_mix: AtomicU32::new(0.0f32.to_bits()),
            cue_level: AtomicU32::new(1.0f32.to_bits()),
            cycles: AtomicU64::new(0),
            last_rate: AtomicU32::new(0),
            last_frames: AtomicU32::new(0),
            out_delay: AtomicU64::new(0),
            cost_sum: AtomicU64::new(0),
            cost_n: AtomicU64::new(0),
            cost_max: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            xruns: AtomicU64::new(0),
            gaps: AtomicU64::new(0),
            heard_dry: AtomicU64::new(0),
            heard_late: AtomicU64::new(0),
            cost_peak: AtomicU64::new(0),
            cost_hist: std::array::from_fn(|_| AtomicU64::new(0)),
            // ~10 s of blocks at 48 kHz / 1024, ~1.4 s of program audio.
            meters: Ring::new(512, MeterBlock::default()),
            decks: names
                .iter()
                .filter(|n| n.starts_with("deck-"))
                .take(MAX_DECKS)
                .map(|_| deck::worker::Shared::new(48_000))
                .collect(),
            deck_of: {
                let mut m = [None; MAX_CHANNELS];
                let mut d = 0;
                for (c, n) in names.iter().take(MAX_CHANNELS).enumerate() {
                    if n.starts_with("deck-") && d < MAX_DECKS {
                        m[c] = Some(d);
                        d += 1;
                    }
                }
                m
            },
            program: Ring::new(1 << 17, 0.0),
            capture: capture::Capture::default(),
            latency: latency::Shared::default(),
            fx_kind: std::array::from_fn(|_| AtomicU32::new(0)),
            fx_on: std::array::from_fn(|_| AtomicBool::new(false)),
            fx_beats: std::array::from_fn(|_| AtomicU32::new(1.0f32.to_bits())),
            fx_depth: std::array::from_fn(|_| AtomicU32::new(0.5f32.to_bits())),
            fx_pool: fx::Pool::new(),
            stem_bridge_on: AtomicBool::new(false),
            stem_bridge_source: AtomicU32::new(0),
            stem_bridge_stem: AtomicU32::new(0),
            stem_bridge_fx: AtomicBool::new(false),
            stem_bridge_depth: AtomicU32::new(0.5f32.to_bits()),
            grid_beat: std::array::from_fn(|_| AtomicU64::new(0)),
            grid_offset: std::array::from_fn(|_| AtomicU64::new(0)),
            grid_downbeat: std::array::from_fn(|_| AtomicU32::new(255)),
            tune_filter_count: AtomicU32::new(0),
            tune_coefficients: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
            tune_generation: AtomicU64::new(0),
            tune_headroom: AtomicU32::new(1.0f32.to_bits()),
            tune_protection: AtomicBool::new(false),
            master_limiter: AtomicBool::new(true),
            master_eq: master_eq::Published::default(),
            channel_eq: (0..MAX_CHANNELS)
                .map(|_| master_eq::Published::default())
                .collect(),
            voice: (0..MAX_CHANNELS).map(|_| voice::Published::default()).collect(),
            mic_meter: (0..MAX_CHANNELS).map(|_| mic_meter::Published::default()).collect(),
            tune_ceiling: AtomicU32::new((-1.0f32).to_bits()),
        }
    }

    pub fn fx_of(&self, ch: usize) -> fx::Settings {
        let mut s = fx::Settings {
            on: self.fx_on[ch].load(Ordering::Relaxed),
            beats: load(&self.fx_beats[ch]),
            depth: load(&self.fx_depth[ch]),
            ..fx::Settings::default()
        };
        s.unpack_kinds(self.fx_kind[ch].load(Ordering::Relaxed));
        s
    }

    /// Publish a previously compiled profile from the control thread. No parsing or
    /// coefficient design is performed by the real-time callback.
    pub fn set_tune_profile(
        &self,
        filters: &[patch_tune::BiquadCoefficients],
        headroom_db: f32,
        ceiling_dbfs: f32,
        protection: bool,
    ) -> Result<(), String> {
        if filters.len() > TUNE_FILTERS
            || !headroom_db.is_finite()
            || !(0.0..=24.0).contains(&headroom_db)
        {
            return Err("Tune profile exceeds supported filter or headroom limits".into());
        }
        for c in filters {
            if [c.b0, c.b1, c.b2, c.a1, c.a2]
                .iter()
                .any(|v| !v.is_finite())
                || c.a2.abs() >= 1.0
            {
                return Err("Tune filter coefficients are invalid".into());
            }
        }
        self.tune_generation.fetch_add(1, Ordering::AcqRel); // odd: control thread writing
        for (i, c) in filters.iter().enumerate() {
            for (j, v) in [c.b0, c.b1, c.b2, c.a1, c.a2].iter().enumerate() {
                self.tune_coefficients[i][j].store(v.to_bits(), Ordering::Relaxed);
            }
        }
        store(&self.tune_headroom, db_to_linear(-headroom_db));
        store(&self.tune_ceiling, ceiling_dbfs.clamp(-12.0, 0.0));
        self.tune_protection.store(protection, Ordering::Relaxed);
        self.tune_filter_count
            .store(filters.len() as u32, Ordering::Release);
        self.tune_generation.fetch_add(1, Ordering::Release); // even: complete snapshot
        Ok(())
    }

    /// Validate and compile profile data before publishing it to the RT thread.
    pub fn apply_tune_profile(
        &self,
        profile: &patch_tune::OutputProfile,
        identity: &patch_tune::DeviceIdentity,
        sample_rate: u32,
    ) -> Result<(), String> {
        profile.validate()?;
        if !profile.matches(identity) {
            return Err("profile does not match the selected hardware identity".into());
        }
        if !profile.sample_rates.contains(&sample_rate) {
            return Err(format!("profile does not support {sample_rate} Hz"));
        }
        let filters = profile
            .correction
            .filters
            .iter()
            .map(|f| f.compile(sample_rate))
            .collect::<Result<Vec<_>, _>>()?;
        let (ceiling, enabled) = profile
            .protection
            .as_ref()
            .map(|p| (p.peak_ceiling_dbfs, true))
            .unwrap_or((-1.0, false));
        self.set_tune_profile(&filters, profile.headroom_db(), ceiling, enabled)
    }

    pub fn set_fx(&self, ch: usize, s: fx::Settings) {
        self.fx_pool.request(ch, s.on);
        self.fx_kind[ch].store(s.pack_kinds(), Ordering::Relaxed);
        store(&self.fx_beats[ch], s.beats.clamp(1.0 / 32.0, 32.0));
        store(&self.fx_depth[ch], s.depth.clamp(0.0, 1.0));
        self.fx_on[ch].store(s.on, Ordering::Relaxed);
    }

    pub fn set_stem_bridge(&self, source: usize, stem: usize, on: bool) {
        self.stem_bridge_source
            .store(source.min(MAX_DECKS - 1) as u32, Ordering::Relaxed);
        self.stem_bridge_stem
            .store(stem.min(3) as u32, Ordering::Relaxed);
        if on {
            self.stem_bridge_fx.store(false, Ordering::Relaxed);
            self.fx_pool.request(fx::BRIDGE, false);
        }
        self.stem_bridge_on.store(on, Ordering::Release);
    }

    pub fn set_stem_bridge_fx(&self, on: bool) {
        self.fx_pool.request(
            fx::BRIDGE,
            on && self.stem_bridge_on.load(Ordering::Acquire),
        );
        self.stem_bridge_fx.store(on, Ordering::Release);
    }

    pub fn set_stem_bridge_depth(&self, depth: f32) -> Result<(), String> {
        if !depth.is_finite() || !(0.0..=1.0).contains(&depth) {
            return Err("stem bridge FX depth must be between 0 and 1".into());
        }
        self.stem_bridge_depth
            .store(depth.to_bits(), Ordering::Release);
        Ok(())
    }

    /// A deck's beat grid for Beat FX: BPM and first-beat seconds at the
    /// graph rate (None: no grid).
    pub fn set_grid(&self, deck: usize, grid: Option<(f64, f64)>, rate: u32) {
        let (beat, off) = match grid {
            Some((bpm, offset)) if bpm > 0.0 => (60.0 / bpm * rate as f64, offset * rate as f64),
            _ => (0.0, 0.0),
        };
        self.grid_offset[deck].store(off.to_bits(), Ordering::Relaxed);
        self.grid_beat[deck].store(beat.to_bits(), Ordering::Relaxed);
    }

    /// The bar's first beat on a deck's grid (None: unknown).
    pub fn set_grid_downbeat(&self, deck: usize, downbeat: Option<u8>) {
        self.grid_downbeat[deck].store(downbeat.map_or(255, u32::from), Ordering::Relaxed);
    }

    /// Where the step Sequencer's sync deck is, for the next block: None
    /// while sync is off or that deck is not playing on a grid.
    fn sequencer_follow(&self) -> Option<sequencer::Follow> {
        if !self.sequencer.sync_on() {
            return None;
        }
        let d = self.sequencer.sync_source() as usize;
        let deck = self.decks.get(d)?;
        if !deck.active.load(Ordering::Acquire) {
            return None;
        }
        let beat = f64::from_bits(self.grid_beat[d].load(Ordering::Relaxed));
        // The source position of the frame this block plays first, and
        // source frames per output frame (0: stopped).
        let (pos, step) = deck.playhead();
        if !(beat > 0.0 && step > 0.0 && pos.is_finite()) {
            return None;
        }
        let off = f64::from_bits(self.grid_offset[d].load(Ordering::Relaxed));
        let down = self.grid_downbeat[d].load(Ordering::Relaxed);
        let beats = (pos - off) / beat - if down < 4 { down as f64 } else { 0.0 };
        Some(sequencer::Follow {
            sixteenths: beats * 4.0,
            frames_per_sixteenth: beat / step / 4.0,
        })
    }

    /// Callback cost since the previous call: (average ns, worst ns).
    pub fn take_cost(&self) -> (u64, u64) {
        let sum = self.cost_sum.swap(0, Ordering::Relaxed);
        let n = self.cost_n.swap(0, Ordering::Relaxed);
        let max = self.cost_max.swap(0, Ordering::Relaxed);
        (sum.checked_div(n).unwrap_or(0), max)
    }

    /// Upper bounds (ns) of the callback cost's 50th, 95th, 99th and 99.9th
    /// percentiles since the engine started: (cycles, [p50, p95, p99, p99.9]).
    pub fn cost_percentiles(&self) -> (u64, [u64; 4]) {
        let counts: Vec<u64> = self.cost_hist.iter().map(|c| c.load(Ordering::Relaxed)).collect();
        let total: u64 = counts.iter().sum();
        let at = |q: f64| {
            let want = (total as f64 * q).ceil() as u64;
            let mut seen = 0;
            for (k, n) in counts.iter().enumerate() {
                seen += n;
                if seen >= want.max(1) {
                    return 1u64 << k;
                }
            }
            0
        };
        (total, [at(0.5), at(0.95), at(0.99), at(0.999)])
    }

    pub fn channels(&self) -> usize {
        self.names.len()
    }

    pub fn channel(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }

    /// The internal channel `ch` carries, when it is one (its audio is
    /// synthesised here, not read from a port).
    pub fn internal_channel(&self, ch: usize) -> Option<Internal> {
        match self.names.get(ch).map(String::as_str) {
            Some("sequencer") => Some(Internal::Sequencer),
            Some("pattern") => Some(Internal::Pattern),
            Some("tidal") => Some(Internal::Tidal),
            _ => None,
        }
    }

    pub fn set_gain(&self, ch: usize, linear: f32) {
        store(&self.gain[ch], linear.clamp(0.0, 4.0));
    }

    /// Callback cycles since this lane's owner last carried a signal
    /// (None: never). A handover successor counts it as heard at the handover.
    pub fn stream_signal_age(&self, channel: usize) -> Option<u32> {
        let v = self.stream_signal[channel].load(Ordering::Relaxed);
        ((v >> 32) as u32 == self.channel_epoch[channel].load(Ordering::Acquire))
            .then(|| (self.cycles.load(Ordering::Relaxed) as u32).wrapping_sub(v as u32))
    }

    /// The lane as never heard (its history belonged to another stream).
    pub fn forget_stream_signal(&self, channel: usize) {
        self.stream_signal[channel].store(u64::MAX, Ordering::Relaxed);
    }

    pub fn stream_has_signal(&self, channel: usize) -> bool {
        self.stream_signal_age(channel).is_some()
    }

    /// Whether the lane carried a signal within the last `seconds`.
    pub fn stream_heard_within(&self, channel: usize, seconds: f32) -> bool {
        let rate = self.last_rate.load(Ordering::Relaxed).max(1) as f32;
        let frames = self.last_frames.load(Ordering::Relaxed).max(1) as f32;
        self.stream_signal_age(channel).is_some_and(|age| (age as f32) <= seconds * rate / frames)
    }

    pub fn restore_stream_signal(&self, channel: usize) {
        let epoch = self.channel_epoch[channel].load(Ordering::Acquire) as u64;
        let now = self.cycles.load(Ordering::Relaxed) as u32;
        self.stream_signal[channel].store(epoch << 32 | now as u64, Ordering::Relaxed);
    }

    pub fn stream_touched(&self, channel: usize) -> bool {
        self.stream_touched[channel].load(Ordering::Relaxed)
    }

    pub fn touch_stream(&self, channel: usize) {
        self.stream_touched[channel].store(true, Ordering::Relaxed);
    }

    pub fn stream_owner(&self, channel: usize, key: &str) -> Result<std::sync::MutexGuard<'_, Option<String>>, String> {
        let owner = self.stream_owners[channel].lock().unwrap();
        if owner.as_deref() != Some(key) { return Err("playback stream no longer owns this lane".into()); }
        Ok(owner)
    }

    /// Release all controls belonging to a departed playback identity.
    /// The callback observes the epoch and clears its small filter state and
    /// FX delay cursors without allocating or freeing any buffers.
    pub fn reset_stream_channel(&self, ch: usize) {
        self.set_gain(ch, 1.0);
        self.set_mute(ch, false);
        self.set_solo(ch, false);
        self.set_pfl(ch, false);
        self.set_pan(ch, 0.0);
        self.set_trim(ch, 1.0);
        self.set_eq(ch, eq::Settings::default());
        self.channel_eq[ch].set(&patch_core::eq::EqState::default());
        self.set_fx(ch, fx::Settings::default());
        for bus in 0..BUSES { self.set_send(ch, bus, 0.0); self.set_send_pre(ch, bus, bus < 2); }
        self.stream_touched[ch].store(false, Ordering::Relaxed);
        self.channel_epoch[ch].fetch_add(1, Ordering::Release);
    }

    pub fn set_mute(&self, ch: usize, mute: bool) {
        self.mute[ch].store(mute, Ordering::Relaxed);
    }

    pub fn set_solo(&self, ch: usize, solo: bool) {
        if ch < MAX_CHANNELS {
            let bit = 1u64 << ch;
            if solo {
                self.solo.fetch_or(bit, Ordering::Relaxed);
            } else {
                self.solo.fetch_and(!bit, Ordering::Relaxed);
            }
        }
    }

    pub fn solo_of(&self, ch: usize) -> bool {
        ch < MAX_CHANNELS && self.solo.load(Ordering::Relaxed) & (1u64 << ch) != 0
    }

    pub fn set_dj_solo(&self, solo: bool) {
        self.dj_solo.store(solo, Ordering::Relaxed);
    }

    pub fn dj_solo(&self) -> bool {
        self.dj_solo.load(Ordering::Relaxed)
    }

    /// In the program and on sends. Mute wins. A mic solo adds the selected
    /// mic over the running programme; it must not cut music from the decks,
    /// apps, or other non-mic sources. Other solos remain solo-in-place.
    fn heard(&self, ch: usize) -> bool {
        if self.is_muted(ch) {
            return false;
        }
        if self.mic_solo_engaged() {
            return !self.mic_channels[ch] || self.solo_of(ch);
        }
        if !self.solo_engaged() {
            return true;
        }
        if self.deck_of.get(ch).and_then(|d| *d).is_some() {
            return self.dj_solo.load(Ordering::Relaxed);
        }
        self.solo_of(ch)
    }

    fn send_target(&self, ch: usize, bus: usize) -> f32 {
        if self.heard(ch) && !self.mix_minus(ch) { self.send_of(ch, bus) } else { 0.0 }
    }

    /// An input lane kept out of everything that leaves Patch for a call:
    /// Patch Input, and every send (AUX 1/2 are captured by browsers, an FX
    /// return can carry it there), whatever its send knobs say. A remote
    /// voice's far end never hears itself.
    pub fn mix_minus(&self, ch: usize) -> bool {
        ch < 64 && self.mic_mask >> ch & 1 == 1 && !self.input_send[ch].load(Ordering::Relaxed)
    }

    pub fn set_pan(&self, ch: usize, pan: f32) {
        store(&self.pan[ch], pan.clamp(-1.0, 1.0));
    }

    pub fn set_master(&self, linear: f32) {
        store(&self.master, fader_floor(linear).clamp(0.0, 4.0));
    }

    pub fn set_eq(&self, ch: usize, s: eq::Settings) {
        store(&self.eq[ch][0], s.low.clamp(0.0, 2.0));
        store(&self.eq[ch][1], s.mid.clamp(0.0, 2.0));
        store(&self.eq[ch][2], s.high.clamp(0.0, 2.0));
        store(&self.eq[ch][3], s.filter.clamp(-1.0, 1.0));
    }

    pub fn eq_of(&self, ch: usize) -> eq::Settings {
        let e = &self.eq[ch];
        eq::Settings {
            low: load(&e[0]),
            mid: load(&e[1]),
            high: load(&e[2]),
            filter: load(&e[3]),
        }
    }

    pub fn set_pfl(&self, ch: usize, on: bool) {
        self.pfl[ch].store(on, Ordering::Relaxed);
    }

    pub fn pfl_of(&self, ch: usize) -> bool {
        self.pfl[ch].load(Ordering::Relaxed)
    }

    /// Voice processing on channel `ch` (a microphone's).
    pub fn set_voice(&self, ch: usize, v: voice::Settings) {
        if let Some(slot) = self.voice.get(ch) { slot.set(v); }
    }

    pub fn voice_of(&self, ch: usize) -> voice::Settings {
        self.voice.get(ch).and_then(voice::Published::read).unwrap_or(voice::Settings::OFF)
    }

    pub fn set_cue_mix(&self, m: f32) {
        store(&self.cue_mix, m.clamp(0.0, 1.0));
    }

    pub fn set_cue_level(&self, g: f32) {
        store(&self.cue_level, g.clamp(0.0, 4.0));
    }

    pub fn cue(&self) -> (f32, f32) {
        (load(&self.cue_mix), load(&self.cue_level))
    }

    pub fn gain_of(&self, ch: usize) -> f32 {
        load(&self.gain[ch])
    }

    /// A send's level, linear (0 = off); `slot` is a channel or `DJ_SEND`.
    pub fn set_send(&self, slot: usize, bus: usize, linear: f32) {
        store(&self.sends[slot][bus], linear.clamp(0.0, 4.0));
    }

    pub fn send_of(&self, slot: usize, bus: usize) -> f32 {
        load(&self.sends[slot][bus])
    }

    pub fn set_send_pre(&self, slot: usize, bus: usize, pre: bool) {
        self.send_pre[slot][bus].store(pre, Ordering::Relaxed);
    }

    pub fn send_pre_of(&self, slot: usize, bus: usize) -> bool {
        self.send_pre[slot][bus].load(Ordering::Relaxed)
    }

    pub fn set_bus(&self, bus: usize, linear: Option<f32>, mute: Option<bool>) {
        if let Some(g) = linear {
            store(&self.bus_gain[bus], fader_floor(g).clamp(0.0, 4.0));
        }
        if let Some(m) = mute {
            self.bus_mute[bus].store(m, Ordering::Relaxed);
        }
    }

    pub fn bus_of(&self, bus: usize) -> (f32, bool) {
        (load(&self.bus_gain[bus]), self.bus_mute[bus].load(Ordering::Relaxed))
    }

    fn bus_target(&self, bus: usize) -> f32 {
        if self.bus_mute[bus].load(Ordering::Relaxed) { 0.0 } else { load(&self.bus_gain[bus]) }
    }

    /// Input trim, linear (−24 … +24 dB).
    pub fn set_trim(&self, ch: usize, linear: f32) {
        store(&self.trim[ch], linear.clamp(0.063, 15.85));
    }

    pub fn trim_of(&self, ch: usize) -> f32 {
        load(&self.trim[ch])
    }

    pub fn is_muted(&self, ch: usize) -> bool {
        self.mute[ch].load(Ordering::Relaxed)
    }

    pub fn master(&self) -> f32 {
        load(&self.master)
    }

    /// The master as it acts: 0 while the Patch mix is muted.
    fn master_gain(&self) -> f32 {
        if self.master_mute.load(Ordering::Relaxed) {
            0.0
        } else {
            load(&self.master)
        }
    }

    /// Balance law: centre is unity on both sides (a stereo source stays
    /// exactly as it was); turning right attenuates the left side with a
    /// constant-power curve, and vice versa.
    pub fn side_gains(&self, ch: usize) -> [f32; 2] {
        // The channel alone: the master is applied once, at the output.
        let g = if self.heard(ch) {
            load(&self.gain[ch])
        } else {
            0.0
        };
        let p = load(&self.pan[ch]).clamp(-1.0, 1.0);
        let fade = |x: f32| (x * std::f32::consts::FRAC_PI_2).cos();
        let sys = self.system_gains(ch);
        [
            g * sys[0] * if p > 0.0 { fade(p) } else { 1.0 },
            g * sys[1] * if p < 0.0 { fade(-p) } else { 1.0 },
        ]
    }
}

/// Real-time state, owned by the RT thread only.
pub struct Rt {
    sequencer: sequencer::Sequencer,
    sampler: sampler::Sampler,
    instrument: Box<instrument::Instrument>,
    /// The Plugins Unit's chain on the master (empty: straight through).
    plugins: Option<patch_plugin::chain::Bridge>,
    plugin_control: Option<patch_plugin::chain::ChainControl>,
    pub params: Arc<Params>,
    current: [[f32; 2]; MAX_CHANNELS],
    channel_epoch: [u32; MAX_CHANNELS],
    /// Frames of signal each playback lane carried since its owner arrived.
    signal_frames: [u32; MAX_CHANNELS],
    booth_now: f32,
    overlap: overlap::Overlap,
    scratch: Box<[f32]>,
    /// EQ + filter state per channel side, and the buffer it works in.
    lanes: Box<[[eq::Lane; 2]; MAX_CHANNELS]>,
    /// Timed changes under way per channel: gain, then EQ low, mid, high,
    /// filter.
    glides: Box<[[Glide; 5]; MAX_CHANNELS]>,
    /// A gliding channel's block: (end frame, gain, EQ) per piece.
    pieces: Box<[(usize, f32, eq::Settings); MAX_PIECES]>,
    eq_buf: Box<[f32]>,
    /// Headphone cue sum (L, R) and the blend/level reached last block.
    cue_sum: Box<[f32]>,
    input_sum: Box<[f32]>,
    /// The mix-minus lanes as they went into the program this block (N−1
    /// for Patch Input when it carries the whole program).
    minus_sum: Box<[f32]>,
    /// The finished program (L, then R at MAX_FRAMES) before the system
    /// volume: what `patch.program` records, whatever the speakers are at.
    program_tap: Box<[f32]>,
    input_guard: limiter::Limiter,
    input_monitor_now: [f32; MAX_CHANNELS],
    /// Each input channel's ramped share of the Patch Input mix.
    input_send_now: [f32; MAX_CHANNELS],
    cue_now: (f32, f32),
    /// The system volume the output stage applied last (its ramp's start);
    /// None while it's in the channel gains instead.
    sys_now: Option<[f32; 2]>,
    /// The last cycle's start (ns), length (ns) and the driver's xrun time.
    last_cycle: Option<(u64, u64, u64)>,
    /// The first cycle's start (ns): the graph settles after it.
    first_cycle: Option<u64>,
    /// The loudest sample this cycle on the output and headphones, and the
    /// last cycle (ns) that carried one above −80 dBFS.
    out_level: f32,
    sounding_at: Option<u64>,
    /// The graph position made continuous across driver changes.
    timeline: timeline::Timeline,
    /// The Patch Mix's fader as the output stage applied it last (the ramp's
    /// start). One scalar on the summed mix, not one per channel.
    master_now: Option<f32>,
    /// Native deck playback: consumers and their planar buffers.
    consumers: Vec<deck::worker::Consumer>,
    deck_l: Box<[f32]>,
    deck_r: Box<[f32]>,
    /// The internal channels' audio for this block: the step Sequencer's
    /// drums, then the Pattern Unit's (its voices and derived material).
    seq_l: Box<[f32]>,
    seq_r: Box<[f32]>,
    pat_l: Box<[f32]>,
    pat_r: Box<[f32]>,
    tid_l: Box<[f32]>,
    tid_r: Box<[f32]>,
    bridge_l: Box<[f32]>,
    bridge_r: Box<[f32]>,
    bridge_fx: Option<fx::Fx>,
    /// Beat FX per channel and the post-fader buffers they work in.
    fx: Vec<Option<fx::Fx>>,
    fx_l: Box<[f32]>,
    /// The DJ Booth's sum this cycle (L at 0, R at MAX_FRAMES): the decks
    /// mix here, it's metered, then added to the program.
    grp: Box<[f32]>,
    /// C/D arrangement lanes bypass the A/B crossfader correction.
    aux_grp: Box<[f32]>,
    fx_r: Box<[f32]>,
    tune_filters: [[eq::Biquad; TUNE_FILTERS]; 2],
    tune_active_count: usize,
    tune_guard: [patch_tune::PeakGuard; 1],
    /// Master limiter: the last stage of the program (never clips).
    limiter: limiter::Limiter,
    /// Master EQ (the EQ Unit): after the mix, before Tune.
    master_eq: master_eq::MasterEq,
    /// Channel EQs and their scratch buffer.
    channel_eq: Vec<master_eq::MasterEq>,
    voice: Vec<voice::Voice>,
    voice_buf: Box<[f32]>,
    /// A channel's input on its way through its plugins (L, then R at
    /// MAX_FRAMES).
    plug_buf: Box<[f32]>,
    /// A trimmed microphone's dry input (L, then R at MAX_FRAMES), for the
    /// voice processor's bypass fade: only filled while trim is in use.
    dry_buf: Box<[f32]>,
    /// Each channel's input trim as applied last (its ramp's start).
    trim_now: [f32; MAX_CHANNELS],
    /// The send buses this cycle: bus b's L at (2b) × MAX_FRAMES, R next.
    aux: Box<[f32]>,
    /// Each send and bus fader as applied last (their ramps' starts).
    send_now: [[f32; BUSES]; MAX_CHANNELS + 1],
    bus_now: [f32; BUSES],
    /// A bus that something sent to this cycle.
    bus_fed: [bool; BUSES],
    reverb: aux_fx::Reverb,
    delay: aux_fx::Delay,
    ceq_buf: Box<[f32]>,
    tune_gain: f32,
    /// The program upmixed for a surround or mono device.
    pub surround: surround::Surround,
}

fn stem_bridge_fx_settings(mut source: fx::Settings, enabled: bool, depth: f32) -> fx::Settings {
    source.on = enabled;
    source.depth = depth.clamp(0.0, 1.0);
    source
}

impl Rt {
    /// The chain's control end, for the daemon (once).
    pub fn take_plugin_control(&mut self) -> Option<patch_plugin::chain::ChainControl> {
        self.plugin_control.take()
    }

    /// The engine's real-time state. Its plugin chain starts empty: the
    /// daemon hands it the rack (`take_plugin_control`), tests their own.
    pub fn new(params: Arc<Params>) -> Self {
        Self::new_with_plugin_config(params, None)
    }

    /// Construct with an explicit Plugins Unit rack for audio-path tests.
    pub fn new_with_plugin_config(params: Arc<Params>, rack: Option<patch_plugin::RackConfig>) -> Self {
        // Prepare kernel/profile selection before any callback can run.
        patch_dsp::registry::init();
        let channels = params.channels();
        let mut current = [[0.0; 2]; MAX_CHANNELS];
        for (c, cur) in current.iter_mut().enumerate().take(params.channels()) {
            *cur = params.side_gains(c);
        }
        let input_monitor_now = std::array::from_fn(|i| if params.input_monitor[i].load(Ordering::Relaxed) { 1.0 } else { 0.0 });
        let input_send_now = std::array::from_fn(|i| if params.input_send[i].load(Ordering::Relaxed) { 1.0 } else { 0.0 });
        let trim_now = std::array::from_fn(|c| if c < channels { params.trim_of(c) } else { 1.0 });
        let bus_now = std::array::from_fn(|b| params.bus_target(b));
        let names: Vec<&str> = params.names.iter().map(String::as_str).collect();
        let (plugins, plugin_control) = match patch_plugin::chain::start(MAX_FRAMES, &names) {
            Ok((bridge, control)) => {
                if let Some(rack) = &rack {
                    control.sync(rack);
                }
                (Some(bridge), Some(control))
            }
            Err(error) => {
                eprintln!("Plugins Unit: {error:#}");
                (None, None)
            }
        };
        Rt {
            sequencer: sequencer::Sequencer::default(),
            sampler: sampler::Sampler::default(),
            instrument: Box::new(instrument::Instrument::new()),
            plugins,
            plugin_control,
            booth_now: params.booth_gain(),
            overlap: overlap::Overlap::default(),
            params,
            current,
            channel_epoch: [0; MAX_CHANNELS],
            signal_frames: [0; MAX_CHANNELS],
            scratch: vec![0.0; MAX_FRAMES * 2].into_boxed_slice(),
            lanes: Box::new([[eq::Lane::default(); 2]; MAX_CHANNELS]),
            glides: Box::new([[Glide::default(); 5]; MAX_CHANNELS]),
            pieces: Box::new([(0, 0.0, eq::Settings::default()); MAX_PIECES]),
            eq_buf: vec![0.0; MAX_FRAMES].into_boxed_slice(),
            cue_sum: vec![0.0; MAX_FRAMES * 2].into_boxed_slice(),
            input_sum: vec![0.0; MAX_FRAMES * 2].into_boxed_slice(),
            minus_sum: vec![0.0; MAX_FRAMES * 2].into_boxed_slice(),
            program_tap: vec![0.0; MAX_FRAMES * 2].into_boxed_slice(),
            input_guard: limiter::Limiter::new_zero_latency(MASTER_CEILING_DBFS, 48_000),
            input_monitor_now,
            input_send_now,
            cue_now: (0.0, 1.0),
            sys_now: None,
            last_cycle: None,
            first_cycle: None,
            out_level: 0.0,
            sounding_at: None,
            timeline: timeline::Timeline::default(),
            master_now: None,
            consumers: (0..MAX_DECKS)
                .map(|_| deck::worker::Consumer::new())
                .collect(),
            deck_l: vec![0.0; MAX_FRAMES].into_boxed_slice(),
            deck_r: vec![0.0; MAX_FRAMES].into_boxed_slice(),
            seq_l: vec![0.0; MAX_FRAMES].into_boxed_slice(),
            seq_r: vec![0.0; MAX_FRAMES].into_boxed_slice(),
            pat_l: vec![0.0; MAX_FRAMES].into_boxed_slice(),
            pat_r: vec![0.0; MAX_FRAMES].into_boxed_slice(),
            tid_l: vec![0.0; MAX_FRAMES].into_boxed_slice(),
            tid_r: vec![0.0; MAX_FRAMES].into_boxed_slice(),
            bridge_l: vec![0.0; MAX_FRAMES].into_boxed_slice(),
            bridge_r: vec![0.0; MAX_FRAMES].into_boxed_slice(),
            bridge_fx: None,
            fx: (0..channels).map(|_| None).collect(),
            fx_l: vec![0.0; MAX_FRAMES].into_boxed_slice(),
            grp: vec![0.0; MAX_FRAMES * 2].into_boxed_slice(),
            aux_grp: vec![0.0; MAX_FRAMES * 2].into_boxed_slice(),
            fx_r: vec![0.0; MAX_FRAMES].into_boxed_slice(),
            tune_filters: std::array::from_fn(|_| std::array::from_fn(|_| eq::Biquad::default())),
            tune_active_count: 0,
            tune_guard: [patch_tune::PeakGuard::new(-1.0, 48_000)],
            limiter: limiter::Limiter::new(MASTER_CEILING_DBFS, 48_000),
            master_eq: master_eq::MasterEq::default(),
            channel_eq: (0..MAX_CHANNELS)
                .map(|_| master_eq::MasterEq::default())
                .collect(),
            voice: (0..MAX_CHANNELS).map(|_| voice::Voice::default()).collect(),
            voice_buf: vec![0.0; MAX_FRAMES * 2].into_boxed_slice(),
            plug_buf: vec![0.0; MAX_FRAMES * 2].into_boxed_slice(),
            dry_buf: vec![0.0; MAX_FRAMES * 2].into_boxed_slice(),
            trim_now,
            aux: vec![0.0; BUSES * 2 * MAX_FRAMES].into_boxed_slice(),
            send_now: [[0.0; BUSES]; MAX_CHANNELS + 1],
            bus_now,
            bus_fed: [false; BUSES],
            reverb: aux_fx::Reverb::default(),
            delay: aux_fx::Delay::default(),
            ceq_buf: vec![0.0; MAX_FRAMES].into_boxed_slice(),
            tune_gain: 1.0,
            surround: surround::Surround::default(),
        }
    }

    /// One graph cycle, timed: its cost is published for the DSP load.
    pub fn process(
        &mut self,
        clock: &Clock,
        ins: &[Option<&[f32]>],
        outs: [&mut [f32]; 2],
        cue: Option<[&mut [f32]; 2]>,
    ) {
        let t0 = now_ns();
        // One timeline whichever device drives the graph (see timeline.rs).
        let clock = &self.timeline.map(clock);
        self.run(clock, ins, outs, cue);
        let cost = now_ns().saturating_sub(t0);
        let p = &self.params;
        p.cost_sum.fetch_add(cost, Ordering::Relaxed);
        p.cost_n.fetch_add(1, Ordering::Relaxed);
        p.cost_max.fetch_max(cost, Ordering::Relaxed);
        let bucket = (64 - cost.leading_zeros() as usize).min(31);
        p.cost_hist[bucket].fetch_add(1, Ordering::Relaxed);
        let period = u64::from(clock.frames) * 1_000_000_000 / u64::from(clock.rate.max(1));
        if self.out_level > 1e-4 {
            self.sounding_at = Some(clock.nsec);
        }
        let heard = self.sounding_at.is_some_and(|at| clock.nsec.saturating_sub(at) < 500_000_000);
        if period > 0 && cost > period {
            p.overruns.fetch_add(1, Ordering::Relaxed);
            if heard {
                p.heard_late.fetch_add(1, Ordering::Relaxed);
            }
        }
        p.cost_peak.fetch_max(cost, Ordering::Relaxed);
        let dry = p.xruns.load(Ordering::Relaxed) + p.gaps.load(Ordering::Relaxed);
        self.watch_cycle(clock, period);
        let p = &self.params;
        let skipped = p.xruns.load(Ordering::Relaxed) + p.gaps.load(Ordering::Relaxed) - dry;
        if heard && skipped > 0 {
            p.heard_dry.fetch_add(skipped, Ordering::Relaxed);
        }
    }

    /// Counts the cycles the device could have skipped on: after an xrun
    /// PipeWire reports, or a wake-up over two cycles late. A pause of a
    /// second or more is the graph stopping (suspend, a new device), not
    /// a skip; nor is one in the engine's first 30 s (the graph settling
    /// as it joins, or as an upgrade hands over) or as the buffer size
    /// changes.
    fn watch_cycle(&mut self, clock: &Clock, period: u64) {
        const XRUN_RECOVER: u32 = 1 << 1;
        const SETTLE: u64 = 30_000_000_000;
        let p = &self.params;
        let grew = self.last_cycle.is_some_and(|(_, _, x)| clock.xrun > x);
        if clock.flags & XRUN_RECOVER != 0 || grew {
            p.xruns.fetch_add(1, Ordering::Relaxed);
        }
        let first = *self.first_cycle.get_or_insert(clock.nsec);
        if let Some((at, last, _)) = self.last_cycle {
            let dt = clock.nsec.saturating_sub(at);
            let settled = clock.nsec.saturating_sub(first) > SETTLE;
            if settled && last == period && dt > last * 5 / 2 && dt < 1_000_000_000 {
                p.gaps.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.last_cycle = Some((clock.nsec, period, clock.xrun));
    }

    pub fn input_audio(&self, frames: usize) -> [&[f32]; 2] {
        let n = frames.min(MAX_FRAMES);
        [&self.input_sum[..n], &self.input_sum[MAX_FRAMES..MAX_FRAMES + n]]
    }

    /// AUX 1 or 2 (`bus` 0, 1) this cycle, after its master; silent when
    /// nothing was sent.
    pub fn aux_audio(&self, bus: usize, frames: usize) -> Option<[&[f32]; 2]> {
        let n = frames.min(MAX_FRAMES);
        (bus < 2 && self.bus_fed[bus]).then(|| {
            let at = bus * 2 * MAX_FRAMES;
            [&self.aux[at..at + n], &self.aux[at + MAX_FRAMES..at + MAX_FRAMES + n]]
        })
    }

    /// The last cycle's program before the system volume (the recording tap).
    pub fn program_audio(&self, frames: usize) -> [&[f32]; 2] {
        let n = frames.min(MAX_FRAMES);
        [&self.program_tap[..n], &self.program_tap[MAX_FRAMES..MAX_FRAMES + n]]
    }

    /// One graph cycle. `ins[c*2 + side]` is None for an unconnected port.
    pub fn run(
        &mut self,
        clock: &Clock,
        ins: &[Option<&[f32]>],
        outs: [&mut [f32]; 2],
        cue: Option<[&mut [f32]; 2]>,
    ) {
        let [out_l, out_r] = outs;
        let frames = out_l.len().min(out_r.len()).min(MAX_FRAMES);
        let (out_l, out_r) = (&mut out_l[..frames], &mut out_r[..frames]);
        out_l.fill(0.0);
        self.input_sum[..frames].fill(0.0);
        self.input_sum[MAX_FRAMES..MAX_FRAMES + frames].fill(0.0);
        out_r.fill(0.0);
        self.grp[..frames].fill(0.0);
        self.grp[MAX_FRAMES..MAX_FRAMES + frames].fill(0.0);
        self.aux_grp[..frames].fill(0.0);
        self.aux_grp[MAX_FRAMES..MAX_FRAMES + frames].fill(0.0);
        self.bridge_l[..frames].fill(0.0);
        self.bridge_r[..frames].fill(0.0);
        for b in 0..BUSES * 2 {
            self.aux[b * MAX_FRAMES..b * MAX_FRAMES + frames].fill(0.0);
        }
        self.bus_fed = [false; BUSES];
        let cueing = cue.is_some();
        if cueing {
            // Both halves: R lives at MAX_FRAMES. (Clearing the first
            // 2 × frames left R's half standing whenever a block is shorter
            // than MAX_FRAMES: it summed forever, then played the same
            // full-scale block every cycle — a ~47 Hz buzz, right only.)
            self.cue_sum[..frames].fill(0.0);
            self.cue_sum[MAX_FRAMES..MAX_FRAMES + frames].fill(0.0);
        }
        let p = &*self.params;
        // Decks auditioned in the headphones (never in the program).
        for (d, sh) in p.decks.iter().enumerate().take(self.consumers.len()) {
            let (l, r) = self.cue_sum.split_at_mut(MAX_FRAMES);
            self.consumers[d].audition(sh, &mut l[..frames], &mut r[..frames], cueing);
        }
        while let Some((slot, instance)) = p.fx_pool.take_ready() {
            if !p.fx_pool.wanted(slot) {
                if let Err(instance) = p.fx_pool.retire(slot, instance) {
                    // Retain ownership on queue pressure; never drop buffers here.
                    if slot == fx::BRIDGE {
                        self.bridge_fx = Some(instance);
                    } else if let Some(target) = self.fx.get_mut(slot) {
                        *target = Some(instance);
                    }
                }
            } else if slot == fx::BRIDGE {
                self.bridge_fx = Some(instance);
            } else if let Some(target) = self.fx.get_mut(slot) {
                *target = Some(instance);
            } else if let Err(instance) = p.fx_pool.retire(slot, instance) {
                // Unreachable in practice (a slot beyond the channels has
                // no requester): forgetting leaks the instance's buffers
                // rather than dropping them — a deallocation — on the
                // real-time thread.
                std::mem::forget(instance);
            }
        }
        let mut block = MeterBlock {
            stamp: clock.stamp(),
            ..Default::default()
        };
        let fs = clock.rate.max(1) as f64;
        // Timed changes due in this block, in time order.
        let mut due = [Due::default(); 32];
        let mut n_due = 0;
        let start = clock.position;
        for s in &p.timed {
            if n_due < due.len() && s.armed.load(Ordering::Acquire) {
                let at = s.at.load(Ordering::Relaxed);
                let from = at.saturating_sub(s.glide.load(Ordering::Relaxed) as u64);
                if from < start + frames as u64 {
                    due[n_due] = Due {
                        frame: (from.saturating_sub(start) as usize).min(frames.saturating_sub(1)),
                        end: at,
                        ch: s.ch.load(Ordering::Relaxed) as usize,
                        kind: s.kind.load(Ordering::Relaxed),
                        value: f32::from_bits(s.value.load(Ordering::Relaxed)),
                    };
                    n_due += 1;
                    s.armed.store(false, Ordering::Release);
                }
            }
        }
        due[..n_due].sort_by_key(|d| d.frame);
        // The internal channels' audio, rendered before the channel pass so
        // their strips carry it: the step Sequencer's drums into `sequencer`,
        // the Pattern Unit's voices and derived material into `pattern`.
        self.seq_l[..frames].fill(0.0);
        self.seq_r[..frames].fill(0.0);
        self.pat_l[..frames].fill(0.0);
        self.pat_r[..frames].fill(0.0);
        let block_start = clock.position as i64;
        self.sequencer.follow(p.sequencer_follow());
        self.sequencer.render_at(
            &p.sequencer,
            clock.rate,
            Some(block_start),
            [&mut self.seq_l[..frames], &mut self.seq_r[..frames]],
            [&mut self.pat_l[..frames], &mut self.pat_r[..frames]],
        );
        // Derived material from the same pattern queue, on its own frame, at
        // unity: the pattern channel's fader does the rest.
        if self.sampler.rate() != clock.rate {
            self.sampler.set_rate(clock.rate);
        }
        self.sampler.sync(&p.sampler);
        for e in self.sequencer.samples_due() {
            self.sampler
                .trigger(e.index as usize, e.sample - block_start, e.value);
        }
        self.sampler.render_gain(
            frames,
            &mut self.pat_l[..frames],
            &mut self.pat_r[..frames],
            [1.0; 2],
        );
        self.sampler.tick();
        self.sampler.publish(&p.sampler);
        // The Tidal Unit's instrument, from its own queue, into `tidal`.
        self.tid_l[..frames].fill(0.0);
        self.tid_r[..frames].fill(0.0);
        self.instrument.render(
            &p.tidal,
            clock.rate,
            block_start,
            &mut self.tid_l[..frames],
            &mut self.tid_r[..frames],
        );
        let mut deck_power = [0.0f64; MAX_DECKS];
        let mut input_active = false;
        // A mix-minus lane reached the program while Patch Input carries the
        // whole program: Patch Input is then N−1 (see below).
        let input_main = p.input_main.load(Ordering::Relaxed);
        let mut minus_active = false;
        // This cycle's stamp for the playback lanes' signal observations.
        let cycle = p.cycles.load(Ordering::Relaxed) as u32;
        for c in 0..p.channels() {
            let epoch = p.channel_epoch[c].load(Ordering::Acquire);
            if epoch != self.channel_epoch[c] {
                self.channel_epoch[c] = epoch;
                self.lanes[c] = [eq::Lane::default(); 2];
                self.channel_eq[c].reset();
                self.glides[c] = [Glide::default(); 5];
                self.current[c] = [0.0; 2];
                self.trim_now[c] = 1.0;
                self.send_now[c] = [0.0; BUSES];
                self.signal_frames[c] = 0;
                if let Some(fx) = self.fx[c].as_mut() { fx.reset(); }
            }
            let target = p.side_gains(c);
            let eqs = p.eq_of(c);
            // This channel's timed changes, and those still gliding from
            // earlier blocks: the block in pieces, each with the gain and
            // EQ due at its end (none: one piece, the fast path).
            let mut n_segs = 0;
            let mine = |d: &&Due| d.ch == c;
            if due[..n_due].iter().any(|d| d.ch == c) || self.glides[c].iter().any(|g| g.active) {
                let glides = &mut self.glides[c];
                let mut dues = due[..n_due].iter().filter(mine).peekable();
                let mut a = 0;
                while a < frames {
                    // Start what is due here: from where it is now.
                    while let Some(d) = dues.next_if(|d| d.frame <= a) {
                        let t = start + a as u64;
                        let k = (d.kind as usize).min(4);
                        let mut e = p.eq_of(c);
                        let stored = |e: &eq::Settings, g: f32| match k {
                            0 => g,
                            1 => e.low,
                            2 => e.mid,
                            3 => e.high,
                            _ => e.filter,
                        };
                        let g = load(&p.gain[c]);
                        let now = glides[k].at(t, stored(&e, g)).unwrap_or(stored(&e, g));
                        match k {
                            0 => p.set_gain(c, d.value),
                            1 => e.low = d.value,
                            2 => e.mid = d.value,
                            3 => e.high = d.value,
                            _ => e.filter = d.value,
                        }
                        if k > 0 {
                            p.set_eq(c, e);
                        }
                        // As stored (clamped): a later write by hand shows
                        // as a difference and ends the glide.
                        let to = stored(&p.eq_of(c), load(&p.gain[c]));
                        glides[k] = Glide {
                            active: true,
                            from: now,
                            to,
                            start: t,
                            end: d.end.max(t + TIMED_RAMP as u64),
                        };
                    }
                    let next = dues.peek().map_or(frames, |d| d.frame.max(a + 1));
                    // A glide ending inside the piece ends it there: it
                    // lands on its sample, not up to a piece later.
                    let t_a = start + a as u64;
                    let landing = glides
                        .iter()
                        .filter(|g| g.active && g.end > t_a)
                        .map(|g| (g.end - start) as usize)
                        .min()
                        .unwrap_or(frames);
                    let b = (a + GLIDE_PIECE).min(next).min(landing).min(frames);
                    let b = if n_segs + 1 >= MAX_PIECES { frames } else { b };
                    let t = start + b as u64;
                    let mut e = p.eq_of(c);
                    let g0 = load(&p.gain[c]);
                    let g = glides[0].at(t, g0).unwrap_or(g0);
                    for (k, v) in [(1, &mut e.low), (2, &mut e.mid), (3, &mut e.high), (4, &mut e.filter)] {
                        *v = glides[k].at(t, *v).unwrap_or(*v);
                    }
                    self.pieces[n_segs] = (b, g, e);
                    n_segs += 1;
                    a = b;
                }
            }
            // A native deck with a track plays from its worker; otherwise
            // the channel's ports (apps, radio on mpv).
            let native = match p.deck_of[c] {
                Some(d)
                    if p.decks
                        .get(d)
                        .is_some_and(|sh| sh.active.load(Ordering::Acquire)) =>
                {
                    p.decks[d].publish_graph_sample(clock.position);
                    self.consumers[d].graph_sample = clock.position;
                    let bridge_on = p.stem_bridge_on.load(Ordering::Acquire);
                    let bridge_source = p.stem_bridge_source.load(Ordering::Relaxed) as usize;
                    let send = (bridge_on && d == bridge_source)
                        .then_some(p.stem_bridge_stem.load(Ordering::Relaxed) as usize);
                    let (pos, step) = if send.is_some() {
                        self.consumers[d].pull_with_stem(
                            &p.decks[d],
                            &mut self.deck_l[..frames],
                            &mut self.deck_r[..frames],
                            Some(&mut self.bridge_l[..frames]),
                            Some(&mut self.bridge_r[..frames]),
                            send,
                        )
                    } else {
                        self.consumers[d].pull(
                            &p.decks[d],
                            &mut self.deck_l[..frames],
                            &mut self.deck_r[..frames],
                        )
                    };
                    block.deck_pos[d] = pos;
                    block.deck_step[d] = step as f32;
                    block.deck_load[d] = p.decks[d].loads.load(Ordering::Acquire);
                    true
                }
                Some(d) if d < p.decks.len() => {
                    self.consumers[d].idle(&p.decks[d], clock.position, frames);
                    false
                }
                _ => false,
            };
            // LINE on the channel's input selector: the deck was pulled
            // above (it runs on unheard), the channel plays its ports.
            let native = native
                && !p.deck_of[c]
                    .and_then(|d| p.decks.get(d))
                    .is_some_and(|sh| sh.line.load(Ordering::Relaxed));
            // An internal channel (the step Sequencer's drums, the Pattern
            // Unit's voices and material): its audio is synthesised above.
            let internal = p.internal_channel(c);
            // Beat FX (or its ringing tail): the channel goes through its
            // own post-fader buffers first.
            let fxs = p.fx_of(c);
            let fx_on = fxs.on
                || self
                    .fx
                    .get(c)
                    .and_then(Option::as_ref)
                    .is_some_and(|f| f.active(&fxs));
            let is_input = p.names[c].starts_with("mic-");
            if p.names[c].starts_with("stream-") && !fx_on
                && ins.get(c * 2).copied().flatten().is_none()
                && ins.get(c * 2 + 1).copied().flatten().is_none()
            {
                self.current[c] = target;
                continue;
            }
            if is_input {
                if !fx_on && ins.get(c * 2).copied().flatten().is_none()
                    && ins.get(c * 2 + 1).copied().flatten().is_none()
                {
                    self.current[c] = target;
                    self.trim_now[c] = p.trim_of(c);
                    self.send_now[c] = std::array::from_fn(|b| p.send_target(c, b));
                    self.input_monitor_now[c] = if p.input_monitor[c].load(Ordering::Relaxed) { 1.0 } else { 0.0 };
                    self.input_send_now[c] = if p.input_send[c].load(Ordering::Relaxed) { 1.0 } else { 0.0 };
                    continue;
                }
                input_active = true;
            }
            let deck = p.deck_of[c].is_some();
            if fx_on || is_input || deck {
                self.fx_l[..frames].fill(0.0);
                self.fx_r[..frames].fill(0.0);
            }
            let ceq_on = self.channel_eq[c].prepare(&p.channel_eq[c], clock.rate);
            // Input trim: a ramp from where it was to where it's set; no
            // pass at all at unity.
            let (trim_from, trim_to) = (self.trim_now[c], p.trim_of(c));
            let trimmed = trim_from != 1.0 || trim_to != 1.0;
            // A microphone's voice processing: both sides together (one
            // gain for both), before the EQ and the fader.
            let vs = if native {
                voice::Settings::OFF
            } else {
                self.voice[c].read_settings(&p.voice[c])
            };
            let voiced = (vs.on || is_input) && {
                let (pl, pr) = (
                    ins.get(c * 2).copied().flatten(),
                    ins.get(c * 2 + 1).copied().flatten(),
                );
                match pl.or(pr) {
                    Some(first) => {
                        let (vl, vr) = self.voice_buf.split_at_mut(MAX_FRAMES);
                        let n = frames.min(first.len());
                        vl[..n].copy_from_slice(&first[..n]);
                        let second = pr.filter(|_| pl.is_some()).unwrap_or(first);
                        let m = n.min(second.len());
                        vr[..m].copy_from_slice(&second[..m]);
                        vr[m..n].fill(0.0);
                        if trimmed {
                            // Trim before the gate and compressor; their
                            // bypass fade mixes with the trimmed dry input.
                            ramp_gain(&mut vl[..n], trim_from, trim_to);
                            ramp_gain(&mut vr[..n], trim_from, trim_to);
                            let (dl, dr) = self.dry_buf.split_at_mut(MAX_FRAMES);
                            dl[..n].copy_from_slice(&vl[..n]);
                            dr[..n].copy_from_slice(&vr[..n]);
                            self.voice[c].process_crossfaded(&mut vl[..n], &mut vr[..n], &dl[..n], &dr[..n], &vs, clock.rate);
                        } else {
                            self.voice[c].process_crossfaded(&mut vl[..n], &mut vr[..n], &first[..n], &second[..m], &vs, clock.rate);
                        }
                        p.mic_meter[c].publish(clock, &self.voice[c]);
                        true
                    }
                    None => false,
                }
            };
            // The channel's plugins (the Plugins Unit), on its input as a
            // pair: after a mic's voice processing, before its EQ, fader
            // and sends.
            let plugged = if clock.rate == patch_plugin::RATE {
                self.plugins.as_ref().and_then(|b| b.channel_point(c))
            } else {
                None
            };
            if let Some(point) = plugged {
                for side in 0..2 {
                    let at = side * MAX_FRAMES;
                    let source: Option<&[f32]> = if native {
                        Some(if side == 0 { &self.deck_l[..frames] } else { &self.deck_r[..frames] })
                    } else if let Some(kind) = internal {
                        let (l, r) = match kind {
                            Internal::Sequencer => (&self.seq_l, &self.seq_r),
                            Internal::Pattern => (&self.pat_l, &self.pat_r),
                            Internal::Tidal => (&self.tid_l, &self.tid_r),
                        };
                        Some(if side == 0 { &l[..frames] } else { &r[..frames] })
                    } else if voiced {
                        Some(&self.voice_buf[at..at + frames])
                    } else {
                        // One port (a mono source): both sides from it.
                        ins.get(c * 2 + side).copied().flatten().or(ins.get(c * 2 + 1 - side).copied().flatten())
                    };
                    let into = &mut self.plug_buf[at..at + frames];
                    match source {
                        Some(x) => {
                            let n = x.len().min(frames);
                            into[..n].copy_from_slice(&x[..n]);
                            into[n..].fill(0.0);
                        }
                        None => into.fill(0.0),
                    }
                }
                let (pl, pr) = self.plug_buf.split_at_mut(MAX_FRAMES);
                if let Some(bridge) = self.plugins.as_mut() {
                    bridge.process(point, &mut pl[..frames], &mut pr[..frames]);
                }
            }
            for side in 0..2 {
                let port = ins.get(c * 2 + side).copied().flatten();
                let raw: &[f32] = if plugged.is_some() {
                    &self.plug_buf[side * MAX_FRAMES..side * MAX_FRAMES + frames]
                } else if native {
                    if side == 0 {
                        &self.deck_l[..frames]
                    } else {
                        &self.deck_r[..frames]
                    }
                } else if let Some(kind) = internal {
                    let (l, r) = match kind {
                        Internal::Sequencer => (&self.seq_l, &self.seq_r),
                        Internal::Pattern => (&self.pat_l, &self.pat_r),
                        Internal::Tidal => (&self.tid_l, &self.tid_r),
                    };
                    if side == 0 { &l[..frames] } else { &r[..frames] }
                } else if voiced {
                    let n = frames.min(
                        ins.get(c * 2)
                            .copied()
                            .flatten()
                            .or(ins.get(c * 2 + 1).copied().flatten())
                            .map_or(0, |x| x.len()),
                    );
                    &self.voice_buf[side * MAX_FRAMES..side * MAX_FRAMES + n]
                } else {
                    match port {
                        Some(x) => x,
                        None => continue,
                    }
                };
                let raw = &raw[..frames.min(raw.len())];
                // EQ + filter in place on a copy; untouched when flat.
                let buf = &mut self.eq_buf[..raw.len()];
                buf.copy_from_slice(raw);
                // A voiced microphone was trimmed before its processing.
                let trim_here = trimmed && !voiced;
                if trim_here {
                    ramp_gain(buf, trim_from, trim_to);
                }
                let mut input: &[f32] = if n_segs == 0 {
                    if self.lanes[c][side].process(buf, &eqs, fs) || trim_here {
                        buf
                    } else {
                        raw
                    }
                } else {
                    // Piece by piece: each ramps to the settings due at its
                    // end, so a timed change glides as it was planned.
                    let n = buf.len();
                    let lane = &mut self.lanes[c][side];
                    let mut a = 0;
                    for &(b, _, e) in &self.pieces[..n_segs] {
                        let b = b.min(n);
                        if b > a {
                            lane.process(&mut buf[a..b], &e, fs);
                        }
                        a = b;
                    }
                    buf
                };
                // The channel's own EQ (the EQ Unit), before its fader.
                if ceq_on {
                    let cb = &mut self.ceq_buf[..input.len()];
                    cb.copy_from_slice(input);
                    self.channel_eq[c].run_side(side, cb);
                    input = cb;
                }
                if cueing && p.pfl_of(c) {
                    // Headphones hear the deck at unity, before its fader.
                    mix(
                        &mut self.cue_sum[side * MAX_FRAMES..side * MAX_FRAMES + input.len()],
                        input,
                        1.0,
                        0.0,
                    );
                }
                let g0 = self.current[c][side];
                let dg = if frames > 0 {
                    (target[side] - g0) / frames as f32
                } else {
                    0.0
                };
                block.ch_peak[c][side] = peak(input);
                // Reuse the existing pre-fader/pre-mute meter. A lane counts
                // as heard after a second of signal in all (a browser's
                // silent helper may click once as it opens). An epoch tags
                // this observation so a retiring caller cannot mark its replacement.
                if p.stream_channels[c] && block.ch_peak[c][side] > 0.00001 {
                    if side == 0 || block.ch_peak[c][0] <= 0.00001 {
                        self.signal_frames[c] = self.signal_frames[c].saturating_add(frames as u32);
                    }
                    if self.signal_frames[c] >= clock.rate {
                        p.stream_signal[c].store((epoch as u64) << 32 | cycle as u64, Ordering::Relaxed);
                    }
                }
                block.ch_abs[c][side] = mean_abs(input);
                block.ch_gain[c][side] = g0.max(target[side]);
                // Sends (not from the decks: the DJ Booth sends as one).
                // Pre: the input at the send's level; post: after the fader
                // too (and its pan). A muted channel sends nothing.
                if p.deck_of[c].is_none() {
                    let n = input.len();
                    for b in 0..BUSES {
                        let from = self.send_now[c][b];
                        let to = p.send_target(c, b);
                        if from == 0.0 && to == 0.0 {
                            continue;
                        }
                        let (a, z) = if p.send_pre_of(c, b) { (from, to) } else { (from * g0, to * target[side]) };
                        let at = (b * 2 + side) * MAX_FRAMES;
                        mix(&mut self.aux[at..at + n], input, a, (z - a) / n.max(1) as f32);
                        self.bus_fed[b] = true;
                    }
                }
                let deck = p.deck_of[c].is_some();
                let out: &mut [f32] = match (fx_on || is_input || deck, side, deck) {
                    (true, 0, _) => &mut self.fx_l[..frames],
                    (true, _, _) => &mut self.fx_r[..frames],
                    (false, s, true) => &mut self.grp[s * MAX_FRAMES..s * MAX_FRAMES + frames],
                    (false, 0, false) => &mut *out_l,
                    (false, _, false) => &mut *out_r,
                };
                if n_segs == 0 {
                    mix(&mut out[..input.len()], input, g0, dg);
                } else {
                    // Piece by piece: a straight line to the gain due at
                    // each piece's end.
                    let n = input.len();
                    let mut g = g0;
                    let mut a = 0;
                    for &(b, gain, _) in &self.pieces[..n_segs] {
                        let b = b.min(n);
                        let to = p.side_gains_for(c, gain)[side];
                        if b > a {
                            mix(&mut out[a..b], &input[a..b], g, (to - g) / (b - a) as f32);
                        }
                        g = to;
                        a = b;
                    }
                    block.ch_gain[c][side] = block.ch_gain[c][side].max(g);
                }
            }
            self.trim_now[c] = trim_to;
            self.send_now[c] = std::array::from_fn(|b| p.send_target(c, b));
            self.current[c] = if n_segs == 0 {
                target
            } else {
                p.side_gains_for(c, self.pieces[n_segs - 1].1)
            };
            if ceq_on {
                self.channel_eq[c].finish();
            }
            if fx_on {
                let beat = match p.deck_of[c] {
                    Some(d) if native => {
                        let beat_src = f64::from_bits(p.grid_beat[d].load(Ordering::Relaxed));
                        let (pos, step) = (block.deck_pos[d], block.deck_step[d] as f64);
                        if beat_src > 0.0 && pos.is_finite() {
                            let off = f64::from_bits(p.grid_offset[d].load(Ordering::Relaxed));
                            fx::Beat {
                                frames: if step > 0.0 {
                                    beat_src / step
                                } else {
                                    beat_src
                                },
                                phase: (pos - off) / beat_src,
                                per_frame: step / beat_src,
                                locked: true,
                            }
                        } else {
                            fx::Beat::free(clock.rate.max(1))
                        }
                    }
                    _ => fx::Beat::free(clock.rate.max(1)),
                };
                if let Some(fx) = self.fx[c].as_mut() {
                    fx.process(
                        &mut self.fx_l[..frames],
                        &mut self.fx_r[..frames],
                        fxs,
                        beat,
                        clock.rate.max(1),
                    );
                }
            }
            if self.fx[c].as_ref().is_some_and(|fx| !fx.active(&fxs))
                && let Some(fx) = self.fx[c].take()
                && let Err(fx) = p.fx_pool.retire(c, fx)
            {
                self.fx[c] = Some(fx);
            }
            if fx_on || is_input || deck {
                if let Some(d) = p.deck_of[c].filter(|d| *d < 2) {
                    for (&l, &r) in self.fx_l[..frames].iter().zip(&self.fx_r[..frames]) {
                        deck_power[d] += (l as f64).powi(2) + (r as f64).powi(2);
                    }
                }
                if is_input {
                    // Ramped, so switching mix-minus on a live call never clicks.
                    let to = if p.input_send[c].load(Ordering::Relaxed) { 1.0 } else { 0.0 };
                    let from = self.input_send_now[c];
                    self.input_send_now[c] = to;
                    if from != 0.0 || to != 0.0 {
                        let delta = (to - from) / frames.max(1) as f32;
                        mix(&mut self.input_sum[..frames], &self.fx_l[..frames], from, delta);
                        mix(&mut self.input_sum[MAX_FRAMES..MAX_FRAMES + frames], &self.fx_r[..frames], from, delta);
                    }
                }
                let to = if !is_input || p.input_monitor[c].load(Ordering::Relaxed) { 1.0 } else { 0.0 };
                let from = if is_input { self.input_monitor_now[c] } else { 1.0 };
                self.input_monitor_now[c] = to;
                if from == 0.0 && to == 0.0 {
                    continue;
                }
                if let Some(d) = p.deck_of[c] {
                    let group = if d < 2 { &mut self.grp } else { &mut self.aux_grp };
                    let (gl, gr) = group.split_at_mut(MAX_FRAMES);
                    mix(&mut gl[..frames], &self.fx_l[..frames], 1.0, 0.0);
                    mix(&mut gr[..frames], &self.fx_r[..frames], 1.0, 0.0);
                } else {
                    let delta = (to - from) / frames.max(1) as f32;
                    mix(out_l, &self.fx_l[..frames], from, delta);
                    mix(out_r, &self.fx_r[..frames], from, delta);
                    if input_main && is_input && p.mix_minus(c) {
                        if !minus_active {
                            self.minus_sum[..frames].fill(0.0);
                            self.minus_sum[MAX_FRAMES..MAX_FRAMES + frames].fill(0.0);
                            minus_active = true;
                        }
                        mix(&mut self.minus_sum[..frames], &self.fx_l[..frames], from, delta);
                        mix(&mut self.minus_sum[MAX_FRAMES..MAX_FRAMES + frames], &self.fx_r[..frames], from, delta);
                    }
                }
            }
        }
        // Isolated stem FX return. It is deliberately mixed after both deck
        // channels' effects, so it cannot feed either deck's FX back into
        // itself. The source tap and deck share one worker transport map.
        {
            let route_on = p.stem_bridge_on.load(Ordering::Acquire);
            let src = p.stem_bridge_source.load(Ordering::Relaxed) as usize;
            let dst = 1usize.saturating_sub(src.min(1));
            if let (Some(sc), Some(dc)) = (
                p.deck_of.iter().position(|d| *d == Some(src)),
                p.deck_of.iter().position(|d| *d == Some(dst)),
            ) {
                let use_fx = p.stem_bridge_fx.load(Ordering::Acquire);
                let settings = stem_bridge_fx_settings(
                    p.fx_of(sc),
                    route_on && use_fx,
                    f32::from_bits(p.stem_bridge_depth.load(Ordering::Acquire)),
                );
                let beat_src = f64::from_bits(p.grid_beat[src].load(Ordering::Relaxed));
                let pos = block.deck_pos[src];
                let step = block.deck_step[src] as f64;
                let beat = if beat_src > 0.0 && pos.is_finite() {
                    let off = f64::from_bits(p.grid_offset[src].load(Ordering::Relaxed));
                    fx::Beat {
                        frames: if step > 0.0 {
                            beat_src / step
                        } else {
                            beat_src
                        },
                        phase: (pos - off) / beat_src,
                        per_frame: step / beat_src,
                        locked: true,
                    }
                } else {
                    fx::Beat::free(clock.rate.max(1))
                };
                if route_on || self.bridge_fx.as_ref().is_some_and(|fx| fx.active(&settings)) {
                    if let Some(fx) = self.bridge_fx.as_mut() {
                        fx.process(
                            &mut self.bridge_l[..frames],
                            &mut self.bridge_r[..frames],
                            settings,
                            beat,
                            clock.rate.max(1),
                        );
                    }
                    let gains = p.side_gains(dc);
                    let (gl, gr) = self.grp.split_at_mut(MAX_FRAMES);
                    mix(&mut gl[..frames], &self.bridge_l[..frames], gains[0], 0.0);
                    mix(&mut gr[..frames], &self.bridge_r[..frames], gains[1], 0.0);
                }
                if self
                    .bridge_fx
                    .as_ref()
                    .is_some_and(|fx| !fx.active(&settings))
                    && let Some(fx) = self.bridge_fx.take()
                    && let Err(fx) = p.fx_pool.retire(fx::BRIDGE, fx)
                {
                    self.bridge_fx = Some(fx);
                }
            }
        }
        // The DJ Booth's sum: metered as one channel, then into the program.
        {
            let target = p.booth_gain();
            let step = if frames > 0 {
                (target - self.booth_now) / frames as f32
            } else {
                0.0
            };
            let (gl, gr) = self.grp.split_at_mut(MAX_FRAMES);
            let (gl, gr) = (&mut gl[..frames], &mut gr[..frames]);
            let (from, to) = self.overlap.apply(deck_power, gl, gr, clock.rate);
            for (c, deck) in p.deck_of.iter().enumerate().take(p.channels()) {
                if deck.is_some_and(|d| d < 2) {
                    for gain in &mut block.ch_gain[c] {
                        *gain *= from.max(to);
                    }
                }
            }
            mix(gl, &self.aux_grp[..frames], 1.0, 0.0);
            mix(gr, &self.aux_grp[MAX_FRAMES..MAX_FRAMES + frames], 1.0, 0.0);
            // The DJ Booth's sends: pre its fader here, post below.
            let dj_muted = p.booth_muted();
            let dj_send = |rt_now: f32, b: usize| -> (f32, f32) {
                (rt_now, if dj_muted { 0.0 } else { p.send_of(DJ_SEND, b) })
            };
            for b in 0..BUSES {
                let (from, to) = dj_send(self.send_now[DJ_SEND][b], b);
                if (from != 0.0 || to != 0.0) && p.send_pre_of(DJ_SEND, b) {
                    for (side, src) in [(0, &*gl), (1, &*gr)] {
                        let at = (b * 2 + side) * MAX_FRAMES;
                        mix(&mut self.aux[at..at + frames], src, from, (to - from) / frames.max(1) as f32);
                    }
                    self.bus_fed[b] = true;
                }
            }
            let mut gain = self.booth_now;
            for (l, r) in gl.iter_mut().zip(gr.iter_mut()) {
                *l *= gain;
                *r *= gain;
                gain += step;
            }
            for b in 0..BUSES {
                let (from, to) = dj_send(self.send_now[DJ_SEND][b], b);
                if (from != 0.0 || to != 0.0) && !p.send_pre_of(DJ_SEND, b) {
                    for (side, src) in [(0, &*gl), (1, &*gr)] {
                        let at = (b * 2 + side) * MAX_FRAMES;
                        mix(&mut self.aux[at..at + frames], src, from, (to - from) / frames.max(1) as f32);
                    }
                    self.bus_fed[b] = true;
                }
                self.send_now[DJ_SEND][b] = to;
            }
            self.booth_now = target;
            block.dj_peak = [peak(gl), peak(gr)];
            block.dj_abs = [mean_abs(gl), mean_abs(gr)];
            mix(out_l, gl, 1.0, 0.0);
            mix(out_r, gr, 1.0, 0.0);
        }
        // The buses. FX 1 and 2 turn their sums into the effect (wet
        // only; skipped when nothing was sent and the tail is gone) and
        // return it into the program at their return faders. AUX 1 and 2
        // are mixes of their own: their masters are applied here, and they
        // leave by their own ports (`aux_audio`).
        for b in 0..BUSES {
            let (from, to) = (self.bus_now[b], p.bus_target(b));
            self.bus_now[b] = to;
            // The bus's plugins (insert point 1 + b): it runs while they
            // are in, fed or not (their tails ring on).
            let plugged = clock.rate == patch_plugin::RATE
                && self.plugins.as_ref().is_some_and(|pl| pl.engaged_at(1 + b));
            let (lo, hi) = self.aux.split_at_mut((b * 2 + 1) * MAX_FRAMES);
            let l = &mut lo[b * 2 * MAX_FRAMES..b * 2 * MAX_FRAMES + frames];
            let r = &mut hi[..frames];
            match b {
                2 => {
                    if self.bus_fed[b] || !self.reverb.idle() || plugged {
                        self.reverb.process(l, r, load(&p.reverb_size), clock.rate);
                    } else {
                        self.reverb.rest(frames);
                        continue;
                    }
                }
                3 => {
                    if self.bus_fed[b] || !self.delay.idle() || plugged {
                        self.delay.process(l, r, load(&p.delay_ms), load(&p.delay_feedback), clock.rate);
                    } else {
                        self.delay.rest(frames);
                        continue;
                    }
                }
                _ if !self.bus_fed[b] && !plugged => continue,
                _ => {}
            }
            // FX 1 and 2: after the effect, before the return fader.
            if plugged && b >= 2 && let Some(pl) = self.plugins.as_mut() {
                pl.process(1 + b, l, r);
            }
            let step = (to - from) / frames.max(1) as f32;
            let mut g = from;
            for (x, y) in l.iter_mut().zip(r.iter_mut()) {
                g += step;
                *x *= g;
                *y *= g;
            }
            // AUX 1 and 2: after their master, as they leave.
            if plugged && b < 2 {
                if let Some(pl) = self.plugins.as_mut() {
                    pl.process(1 + b, l, r);
                }
                self.bus_fed[b] = true;
            }
            block.bus_peak[b] = [peak(l), peak(r)];
            if b >= 2 {
                mix(out_l, l, 1.0, 0.0);
                mix(out_r, r, 1.0, 0.0);
            }
        }
        // N−1: Patch Input carries the whole program but not the remote
        // voices in it, taken here before the master EQ (the room's, not
        // the call's), Tune and the limiter; the master fader and Patch
        // Input's own peak guard follow below.
        if minus_active {
            for side in 0..2 {
                let at = side * MAX_FRAMES;
                let out: &[f32] = if side == 0 { &*out_l } else { &*out_r };
                for i in 0..frames {
                    self.input_sum[at + i] = out[i] - self.minus_sum[at + i];
                }
            }
        }
        // The master EQ (intent), then Tune (the output's correction).
        self.master_eq
            .process(&p.master_eq, clock.rate, out_l, out_r);
        if let Some(plugins) = &mut self.plugins && clock.rate == patch_plugin::RATE {
            plugins.process(0, out_l, out_r);
            // The last point this block: the host may take it.
            plugins.end_block();
        }
        // Tune is the post-mix output processor. The Router still owns the
        // destination; Meter sees the resulting program signal and same graph stamp.
        // Take one bounded snapshot attempt. If the control writer is in the
        // middle of publishing, retain the last compiled filters for this block.
        let generation = p.tune_generation.load(Ordering::Acquire);
        let mut published_count = None;
        if generation & 1 == 0 {
            let filter_count = p
                .tune_filter_count
                .load(Ordering::Relaxed)
                .min(TUNE_FILTERS as u32) as usize;
            let mut coefficients = [patch_tune::BiquadCoefficients::identity(); TUNE_FILTERS];
            for i in 0..filter_count {
                let c = &p.tune_coefficients[i];
                coefficients[i] = patch_tune::BiquadCoefficients {
                    b0: f64::from_bits(c[0].load(Ordering::Relaxed)),
                    b1: f64::from_bits(c[1].load(Ordering::Relaxed)),
                    b2: f64::from_bits(c[2].load(Ordering::Relaxed)),
                    a1: f64::from_bits(c[3].load(Ordering::Relaxed)),
                    a2: f64::from_bits(c[4].load(Ordering::Relaxed)),
                };
            }
            if p.tune_generation.load(Ordering::Acquire) == generation {
                for i in 0..filter_count {
                    self.tune_filters[0][i].set_normalized(coefficients[i]);
                    self.tune_filters[1][i].set_normalized(coefficients[i]);
                }
                published_count = Some(filter_count);
            }
        }
        if let Some(count) = published_count {
            self.tune_active_count = count;
        }
        for i in 0..self.tune_active_count {
            for x in out_l.iter_mut() {
                *x = self.tune_filters[0][i].run(*x as f64) as f32;
            }
            for x in out_r.iter_mut() {
                *x = self.tune_filters[1][i].run(*x as f64) as f32;
            }
        }
        let gain_target = f32::from_bits(p.tune_headroom.load(Ordering::Relaxed));
        let g0 = self.tune_gain;
        let dg = if frames > 0 {
            (gain_target - g0) / frames as f32
        } else {
            0.0
        };
        self.eq_buf[..frames].copy_from_slice(out_l);
        out_l.fill(0.0);
        mix(out_l, &self.eq_buf[..frames], g0, dg);
        self.eq_buf[..frames].copy_from_slice(out_r);
        out_r.fill(0.0);
        mix(out_r, &self.eq_buf[..frames], g0, dg);
        self.tune_gain = gain_target;
        if p.tune_protection.load(Ordering::Relaxed) {
            let ceiling = f32::from_bits(p.tune_ceiling.load(Ordering::Relaxed));
            let rate = clock.rate.max(1);
            if !self.tune_guard[0].is_for(ceiling, rate) {
                self.tune_guard[0] = patch_tune::PeakGuard::new(ceiling, rate);
            }
            self.tune_guard[0].process(out_l, out_r);
        }
        // The Patch Mix's fader: one scalar on the summed mix, here and not in
        // every channel's gain. A channel's meter (the DJ Booth's level, the
        // decks' in the crossfader) is taken from the channel, so the main
        // fader no longer moves it; the sound is the same either way, since
        // the master is a scalar. Ramped, and skipped while it is unity.
        {
            let to = p.master_gain();
            let from = self.master_now.unwrap_or(to);
            if from != 1.0 || to != 1.0 {
                let n = frames.max(1) as f32;
                let dg = (to - from) / n;
                self.eq_buf[..frames].copy_from_slice(out_l);
                out_l.fill(0.0);
                mix(out_l, &self.eq_buf[..frames], from, dg);
                self.eq_buf[..frames].copy_from_slice(out_r);
                out_r.fill(0.0);
                mix(out_r, &self.eq_buf[..frames], from, dg);
            }
            self.master_now = Some(to);
        }
        // Nothing but numbers leaves: a NaN or infinity (an app's stream, a
        // plugin, a filter blown up) would reach the device as noise and
        // stay in the limiter's window.
        finite_or_silent(out_l);
        finite_or_silent(out_r);
        // The master limiter, last: nothing leaves the engine over the
        // ceiling (two tracks summed at the crossfader centre, EQ boosts).
        if self.limiter.rate() != clock.rate.max(1) {
            self.limiter = limiter::Limiter::new(MASTER_CEILING_DBFS, clock.rate.max(1));
        }
        if p.master_limiter.load(Ordering::Relaxed) {
            // The look-ahead (1.5 ms) only while a deck is loaded: the DJ
            // mix can sum and boost past full scale. Apps alone: zero latency.
            let want = p.decks.iter().any(|d| d.active.load(Ordering::Relaxed));
            self.limiter.process_mode(out_l, out_r, want);
        }
        block.out_peak = [peak(out_l), peak(out_r)];
        self.out_level = block.out_peak[0].max(block.out_peak[1]);
        // Headphones: cue ↔ program blend at the headphone level, ramped.
        if let Some([cl, cr]) = cue {
            let (mix_t, lvl_t) = p.cue();
            let (m0, g0) = self.cue_now;
            let n = frames.max(1) as f32;
            for (side, outc) in [cl, cr].into_iter().enumerate() {
                let prog = if side == 0 { &*out_l } else { &*out_r };
                let sum = &self.cue_sum[side * MAX_FRAMES..side * MAX_FRAMES + frames];
                for i in 0..frames.min(outc.len()) {
                    let f = i as f32 / n;
                    let (m, g) = (m0 + (mix_t - m0) * f, g0 + (lvl_t - g0) * f);
                    outc[i] = g * ((1.0 - m) * sum[i] + m * prog[i]);
                }
                finite_or_silent(outc);
                self.out_level = self.out_level.max(peak(outc));
            }
            self.cue_now = (mix_t, lvl_t);
        }
        // SAFETY (the three pushes): the callback is the only producer of
        // the meter, program and capture rings.
        unsafe { p.meters.push(block) };
        let inter = &mut self.scratch[..frames * 2];
        patch_dsp::registry::interleave2(out_l, out_r, inter);
        unsafe { p.program.push_slice(inter) };
        unsafe { p.capture.push(block.stamp.sample_pos, clock.rate, inter) };
        self.program_tap[..frames].copy_from_slice(out_l);
        self.program_tap[MAX_FRAMES..MAX_FRAMES + frames].copy_from_slice(out_r);
        if input_main && minus_active {
            let g = p.master_gain();
            if self.input_guard.rate() != clock.rate.max(1) {
                self.input_guard = limiter::Limiter::new_zero_latency(MASTER_CEILING_DBFS, clock.rate.max(1));
            }
            let (left, right) = self.input_sum.split_at_mut(MAX_FRAMES);
            let (left, right) = (&mut left[..frames], &mut right[..frames]);
            for x in left.iter_mut().chain(right.iter_mut()) {
                *x *= g;
            }
            self.input_guard.process_mode(left, right, false);
        } else if input_main {
            self.input_sum[..frames].copy_from_slice(out_l);
            self.input_sum[MAX_FRAMES..MAX_FRAMES + frames].copy_from_slice(out_r);
        } else if input_active {
            if self.input_guard.rate() != clock.rate.max(1) {
                self.input_guard = limiter::Limiter::new_zero_latency(MASTER_CEILING_DBFS, clock.rate.max(1));
            }
            let (left, right) = self.input_sum.split_at_mut(MAX_FRAMES);
            self.input_guard.process_mode(&mut left[..frames], &mut right[..frames], false);
        }
        // The system volume, last: after the recording tap, on what leaves
        // for the device only (ramped; it starts where it is when switched).
        if p.system_post.load(Ordering::Relaxed) {
            let to = p.system_out();
            let from = self.sys_now.unwrap_or(to);
            if from != [1.0; 2] || to != [1.0; 2] {
                let n = frames.max(1) as f32;
                for (side, out) in [&mut *out_l, &mut *out_r].into_iter().enumerate() {
                    let (g0, dg) = (from[side], (to[side] - from[side]) / n);
                    self.eq_buf[..frames].copy_from_slice(out);
                    out.fill(0.0);
                    mix(out, &self.eq_buf[..frames], g0, dg);
                }
            }
            self.sys_now = Some(to);
        } else {
            self.sys_now = None;
        }
        p.cycles.fetch_add(1, Ordering::Relaxed);
        p.last_rate.store(clock.rate, Ordering::Relaxed);
        p.last_frames.store(frames as u32, Ordering::Relaxed);
        p.out_delay.store(
            smooth_delay(f64::from_bits(p.out_delay.load(Ordering::Relaxed)), clock.delay)
                .to_bits(),
            Ordering::Relaxed,
        );
    }
}

/// One cycle's step of the smoothed output delay (frames): the driver's
/// fill swings by a quantum within a cycle, so its mean over about 200
/// cycles (a second at 256 frames) is what the meters wait for. A driver
/// that reports none (0) leaves none.
fn smooth_delay(prev: f64, delay: i64) -> f64 {
    let d = delay.unsigned_abs() as f64;
    if d == 0.0 || prev == 0.0 {
        d
    } else {
        prev + (d - prev) * 0.005
    }
}

unsafe extern "C" {
    fn patch_engine_now_ns() -> u64;
}

/// Now on CLOCK_MONOTONIC, ns: the clock [`MeterBlock`] stamps use.
/// Mean rectified level of a block (0 for an empty one).
#[inline]
/// Scale `x` in place from `from` to `to` across the block (no zipper).
fn ramp_gain(x: &mut [f32], from: f32, to: f32) {
    let n = x.len();
    if n == 0 {
        return;
    }
    if from == to {
        for v in x.iter_mut() {
            *v *= to;
        }
        return;
    }
    let step = (to - from) / n as f32;
    let mut g = from;
    for v in x.iter_mut() {
        g += step;
        *v *= g;
    }
}

pub fn mean_abs(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    x.iter().map(|v| v.abs()).sum::<f32>() / x.len() as f32
}

pub fn now_ns() -> u64 {
    // SAFETY: plain clock_gettime wrapper.
    unsafe { patch_engine_now_ns() }
}

/// A fader at the bottom of its travel (−60 dB, Patch's fader law) is off:
/// at −60 dB the Booth Out at zero still played, faintly.
fn fader_floor(linear: f32) -> f32 {
    if linear <= 0.001_001 { 0.0 } else { linear }
}

pub fn db_to_linear(db: f32) -> f32 {
    if db <= -120.0 {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

pub fn linear_to_db(x: f32) -> f32 {
    if x > 1e-6 {
        20.0 * x.log10()
    } else {
        patch_model::audio::SILENCE_DB
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

pub mod capture;
pub mod workspace;

pub mod stem_service;

mod mic_profiles;

mod mic_meter;

pub mod output_routes;
mod output_clock;
