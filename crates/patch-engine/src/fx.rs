//! Beat FX: DJ effects on a deck channel, locked to its beat grid.
//!
//! A channel runs a chain of up to three effects in series, all driven by
//! the same two controls: `beats` (the musical length) and `depth` (one
//! "super knob" each effect maps to its own parameters). The chain runs
//! after the channel's EQ and fader, so echo and reverb tails ring on when
//! the effect is switched off or the fader closes ("spill-over"), like a
//! club mixer's Beat FX.
//!
//! Everything here runs on the real-time thread and is built for it:
//!
//! * Buffers are allocated once for the highest graph rate. Nothing
//!   allocates, locks or clears memory in `process`: a reset is O(1) (delay
//!   lines remember how much of them was written since, and read silence
//!   beyond that), so switching effects costs no more than running them.
//! * Work happens in 32-frame chunks with time-based ramps: switching on or
//!   off fades over 6 ms and depth glides over ~20 ms, whatever the graph
//!   quantum, so a 32-frame quantum doesn't click and an 8192-frame one
//!   doesn't lag. The output doesn't depend on the quantum.
//! * Changing the effect while it sounds fades the old one out (6 ms)
//!   before the new one starts clean.
//! * Recursive state is flushed below ~1e-25, so decaying tails never turn
//!   into denormals (which cost x86 ~100x per operation).
//! * A channel whose chain is off and whose tails are silent costs nothing.
//!
//! LFO effects lock to the beat grid (unwrapped beat position): a 4-beat
//! sweep starts on the beat the grid says, every time.
//!
//! * Echo: tempo-synced delay (beats), dub-style filtered feedback.
//! * Reverb: Freeverb-style stereo room (8 combs + 4 all-passes per side).
//! * Flanger: modulated short delay with feedback, one sweep per `beats`.
//! * Trans: gate chopping the channel on the beat grid (`beats` per cut).
//! * Crush: bit depth and sample-rate reduction.
//! * Roll: beat repeat — the first `beats` after it engages, repeated
//!   (the deck keeps playing underneath: slip).
//! * Brake: vinyl/tape stop over `beats`.
//! * PingPong: stereo delay bouncing left/right on the beat.
//! * Phaser: six all-pass stages swept once per `beats`.
//! * Noise: a filtered-noise riser sweeping up over `beats`.
//! * Sweep: resonant 4-pole ladder low-pass (zero-delay feedback, unity at
//!   DC) closing and reopening once per `beats`.
//! * Tremolo: level pulsing once per `beats`, loud on the beat; depth goes
//!   from a gentle sine to a hard square.
//! * Pan: equal-power auto-pan, one left-right swing per `beats`.
//! * Drive: soft-clip distortion with anti-derivative anti-aliasing and
//!   level matching (a -12 dBFS signal comes out about as loud).

use std::array;
use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::time::Duration;

/// Which effect a slot runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    #[default]
    Echo = 0,
    Reverb = 1,
    Flanger = 2,
    Trans = 3,
    Crush = 4,
    Roll = 5,
    Brake = 6,
    PingPong = 7,
    Phaser = 8,
    Noise = 9,
    Sweep = 10,
    Tremolo = 11,
    Pan = 12,
    Drive = 13,
}

impl Kind {
    pub const ALL: [Kind; 14] = [
        Kind::Echo,
        Kind::Reverb,
        Kind::Flanger,
        Kind::Trans,
        Kind::Crush,
        Kind::Roll,
        Kind::Brake,
        Kind::PingPong,
        Kind::Phaser,
        Kind::Noise,
        Kind::Sweep,
        Kind::Tremolo,
        Kind::Pan,
        Kind::Drive,
    ];

    pub fn from_u8(v: u8) -> Kind {
        Kind::ALL.get(v as usize).copied().unwrap_or_default()
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Echo => "echo",
            Kind::Reverb => "reverb",
            Kind::Flanger => "flanger",
            Kind::Trans => "trans",
            Kind::Crush => "crush",
            Kind::Roll => "roll",
            Kind::Brake => "brake",
            Kind::PingPong => "pingpong",
            Kind::Phaser => "phaser",
            Kind::Noise => "noise",
            Kind::Sweep => "sweep",
            Kind::Tremolo => "tremolo",
            Kind::Pan => "pan",
            Kind::Drive => "drive",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.name() == s)
    }

    /// Effects whose sound outlives their input.
    fn has_tail(self) -> bool {
        matches!(self, Kind::Echo | Kind::Reverb | Kind::PingPong)
    }
}

/// Timing for one block: how long a beat is in output frames, the beat
/// position at the first frame and its increase per frame.
#[derive(Clone, Copy, Debug)]
pub struct Beat {
    pub frames: f64,
    /// Beats since the grid's first beat, not wrapped: cycles longer than a
    /// beat (a 4-beat sweep, a 2-beat gate) lock to the grid too.
    pub phase: f64,
    pub per_frame: f64,
    /// From a beat grid; otherwise the effect keeps its own phase.
    pub locked: bool,
}

impl Beat {
    /// No grid: 120 BPM at `rate`, free-running phase.
    pub fn free(rate: u32) -> Beat {
        let frames = rate as f64 * 0.5;
        Beat {
            frames,
            phase: 0.0,
            per_frame: 1.0 / frames,
            locked: false,
        }
    }

    /// The same timing `frames` later.
    fn at(self, frames: usize) -> Beat {
        Beat {
            phase: self.phase + self.per_frame * frames as f64,
            ..self
        }
    }

    /// Position (0…1) in a cycle of `beats`, and its increase per frame.
    fn cycle(self, beats: f64) -> (f64, f64) {
        let beats = beats.max(1.0 / 64.0);
        ((self.phase / beats).rem_euclid(1.0), self.per_frame / beats)
    }
}

/// Effects in one chain.
pub const SLOTS: usize = 3;
/// The isolated stem bridge follows the regular mixer channels in the pool.
pub const BRIDGE: usize = crate::MAX_CHANNELS;
const POOL_SLOTS: usize = crate::MAX_CHANNELS + 1;

/// Transfer queue for prepared FX instances. Its fixed slots hold only the
/// small `Fx` handles; the large sample buffers remain owned by each value.
struct Mailbox<T> {
    slots: Box<[UnsafeCell<Option<T>>]>,
    mask: usize,
    head: AtomicUsize,
    tail: AtomicUsize,
}

unsafe impl<T: Send> Sync for Mailbox<T> {}

impl<T> Mailbox<T> {
    fn new(capacity: usize) -> Self {
        let capacity = capacity.next_power_of_two().max(2);
        Self {
            slots: (0..capacity).map(|_| UnsafeCell::new(None)).collect(),
            mask: capacity - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    fn push(&self, value: T) -> Result<(), T> {
        let head = self.head.load(Ordering::Relaxed);
        if head - self.tail.load(Ordering::Acquire) > self.mask {
            return Err(value);
        }
        // SAFETY: this producer owns the next free slot until publishing head.
        unsafe { *self.slots[head & self.mask].get() = Some(value) };
        self.head.store(head + 1, Ordering::Release);
        Ok(())
    }

    fn pop(&self) -> Option<T> {
        let tail = self.tail.load(Ordering::Relaxed);
        if tail == self.head.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: this consumer owns the published slot until advancing tail.
        let value = unsafe { (*self.slots[tail & self.mask].get()).take() };
        self.tail.store(tail + 1, Ordering::Release);
        value
    }
}

/// Background preparation and reclamation for the large per-chain buffers.
/// The callback only transfers `Fx` values through bounded lock-free queues.
pub struct Pool {
    requested: [AtomicBool; POOL_SLOTS],
    state: [AtomicU8; POOL_SLOTS],
    ready: Mailbox<(usize, Fx)>,
    retired: Mailbox<(usize, Fx)>,
}

impl Pool {
    pub fn new() -> Arc<Self> {
        let pool = Arc::new(Self {
            requested: array::from_fn(|_| AtomicBool::new(false)),
            state: array::from_fn(|_| AtomicU8::new(0)),
            ready: Mailbox::new(POOL_SLOTS * 2),
            retired: Mailbox::new(POOL_SLOTS * 2),
        });
        let weak = Arc::downgrade(&pool);
        std::thread::Builder::new()
            .name("patch-fx-memory".into())
            .spawn(move || {
                while let Some(pool) = weak.upgrade() {
                    pool.worker_step();
                    drop(pool);
                    std::thread::sleep(Duration::from_millis(10));
                }
            })
            .expect("start FX buffer worker");
        pool
    }

    pub fn request(&self, slot: usize, active: bool) {
        if let Some(requested) = self.requested.get(slot) {
            requested.store(active, Ordering::Release);
        }
    }

    pub(crate) fn wanted(&self, slot: usize) -> bool {
        self.requested[slot].load(Ordering::Acquire)
    }

    fn worker_step(&self) {
        while let Some((slot, fx)) = self.retired.pop() {
            drop(fx); // buffer frees happen on this worker, never the callback
            self.state[slot].store(0, Ordering::Release);
        }
        for slot in 0..POOL_SLOTS {
            if self.requested[slot].load(Ordering::Acquire)
                && self.state[slot]
                    .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Relaxed)
                    .is_ok()
            {
                let fx = Fx::new();
                if self.requested[slot].load(Ordering::Acquire) {
                    self.state[slot].store(2, Ordering::Release);
                    if let Err((_, fx)) = self.ready.push((slot, fx)) {
                        drop(fx);
                        self.state[slot].store(0, Ordering::Release);
                    }
                } else {
                    drop(fx);
                    self.state[slot].store(0, Ordering::Release);
                }
                // Spread simultaneous enables across worker turns instead of
                // faulting every channel's large buffers into memory at once.
                break;
            }
        }
    }

    /// Audio thread: adopt newly prepared instances at a block boundary.
    pub fn take_ready(&self) -> Option<(usize, Fx)> {
        let value = self.ready.pop()?;
        self.state[value.0].store(3, Ordering::Release);
        Some(value)
    }

    /// Audio thread: return an instance for destruction on the worker.
    #[allow(clippy::result_large_err)] // Queue pressure must preserve callback ownership.
    pub fn retire(&self, slot: usize, fx: Fx) -> Result<(), Fx> {
        self.state[slot].store(4, Ordering::Release);
        match self.retired.push((slot, fx)) {
            Ok(()) => Ok(()),
            Err((_, fx)) => {
                self.state[slot].store(3, Ordering::Release);
                Err(fx)
            }
        }
    }
}

/// Settings the control thread sets (read once per block).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// The first effect.
    pub kind: Kind,
    /// Effects after it, in order (packed: no gaps).
    pub chain: [Option<Kind>; SLOTS - 1],
    pub on: bool,
    /// Beat length of the effect (echo time, flanger sweep, trans cut).
    pub beats: f32,
    /// 0…1: the one knob, for every effect in the chain.
    pub depth: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            kind: Kind::Echo,
            chain: [None; SLOTS - 1],
            on: false,
            beats: 1.0,
            depth: 0.5,
        }
    }
}

impl Settings {
    /// What each slot runs.
    pub fn slots(&self) -> [Option<Kind>; SLOTS] {
        [Some(self.kind), self.chain[0], self.chain[1]]
    }

    /// The chain in order, for display and JSON.
    pub fn kinds(&self) -> impl Iterator<Item = Kind> {
        self.slots().into_iter().flatten()
    }

    /// Set the whole chain (the first 3 of `kinds`; empty keeps `kind`).
    pub fn set_kinds(&mut self, kinds: &[Kind]) {
        if let Some(&k) = kinds.first() {
            self.kind = k;
        }
        for (i, c) in self.chain.iter_mut().enumerate() {
            *c = kinds.get(i + 1).copied();
        }
    }

    /// The chain in one atomic word: byte 0 the first kind, bytes 1 and 2
    /// the next ones + 1 (0 = none). A lone kind packs as its own value.
    pub fn pack_kinds(&self) -> u32 {
        let next = |k: Option<Kind>| k.map_or(0, |k| k as u32 + 1);
        self.kind as u32 | next(self.chain[0]) << 8 | next(self.chain[1]) << 16
    }

    pub fn unpack_kinds(&mut self, v: u32) {
        let next = |b: u32| (b & 0xff).checked_sub(1).map(|k| Kind::from_u8(k as u8));
        self.kind = Kind::from_u8(v as u8);
        self.chain = [next(v >> 8), next(v >> 16)];
        if self.chain[0].is_none() {
            self.chain[1] = None;
        }
    }
}

/// The highest graph rate buffers are sized for.
pub const MAX_RATE: usize = 192_000;
/// Longest echo: 4 s (4 beats at 60 BPM).
const ECHO_MAX: usize = MAX_RATE * 4;
const FLANGE_MAX: usize = MAX_RATE / 50; // 20 ms

/// Frames per control step: ramps, glides and LFO coefficients.
const CHUNK: usize = 32;
/// On/off and effect-change fade.
const FADE_S: f32 = 0.006;
/// Depth glide time constant.
const GLIDE_S: f32 = 0.02;

/// Freeverb tunings at 44.1 kHz (scaled to the graph rate).
const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASSES: [usize; 4] = [556, 441, 341, 225];
const SPREAD: usize = 23;

/// Flush a recursive state to zero below ~1e-25 (two adds, no branch):
/// decaying tails never reach the denormal range.
#[inline(always)]
fn flush(x: f32) -> f32 {
    const BIAS: f32 = 1e-18;
    (x + BIAS) - BIAS
}

#[inline(always)]
fn approach(from: f32, to: f32, step: f32) -> f32 {
    if from < to {
        (from + step).min(to)
    } else {
        (from - step).max(to)
    }
}

/// sin(2π·turns), max error ~0.001: for LFOs.
#[inline(always)]
fn sin_turns(turns: f64) -> f32 {
    let x = (turns - turns.round()) as f32; // −0.5 … 0.5
    let y = 8.0 * x - 16.0 * x * x.abs();
    0.225 * (y * y.abs() - y) + y
}

#[inline(always)]
fn cos_turns(turns: f64) -> f32 {
    sin_turns(turns + 0.25)
}

/// A stereo delay line that reads silence where nothing was written since
/// its last reset, so resetting it is O(1).
struct Line {
    buf: [Box<[f32]>; 2],
    at: usize,
    fresh: usize,
}

impl Line {
    fn new(len: usize) -> Line {
        Line {
            buf: [
                vec![0.0; len].into_boxed_slice(),
                vec![0.0; len].into_boxed_slice(),
            ],
            at: 0,
            fresh: 0,
        }
    }

    fn len(&self) -> usize {
        self.buf[0].len()
    }

    fn reset(&mut self) {
        self.fresh = 0;
    }

    #[inline(always)]
    fn push(&mut self, l: f32, r: f32) {
        self.buf[0][self.at] = l;
        self.buf[1][self.at] = r;
        self.at += 1;
        if self.at == self.len() {
            self.at = 0;
        }
        if self.fresh < self.len() {
            self.fresh += 1;
        }
    }

    /// The signal `dist` (≥ 1) frames before the next write, interpolated.
    #[inline(always)]
    fn read(&self, side: usize, dist: f64) -> f32 {
        let len = self.len();
        let d0 = dist.ceil() as usize; // the older neighbour's distance
        if d0 > self.fresh + 1 || d0 >= len {
            return 0.0;
        }
        let frac = (d0 as f64 - dist) as f32; // weight of the newer one
        let i0 = if self.at >= d0 {
            self.at - d0
        } else {
            self.at + len - d0
        };
        let i1 = if i0 + 1 == len { 0 } else { i0 + 1 };
        let buf = &self.buf[side];
        let older = if d0 <= self.fresh { buf[i0] } else { 0.0 };
        older + (buf[i1] - older) * frac
    }
}

struct Comb {
    buf: Box<[f32]>,
    len: usize,
    at: usize,
    store: f32,
    /// Frames written since reset (reads before a full lap are silence).
    fresh: usize,
}

impl Comb {
    fn new(max: usize) -> Comb {
        Comb {
            buf: vec![0.0; max].into_boxed_slice(),
            len: max,
            at: 0,
            store: 0.0,
            fresh: 0,
        }
    }

    /// `WARM`: every slot was written since the reset (no check).
    #[inline(always)]
    fn run<const WARM: bool>(&mut self, x: f32, feedback: f32, damp: f32) -> f32 {
        let y = if WARM || self.fresh >= self.len {
            self.buf[self.at]
        } else {
            self.fresh += 1;
            0.0
        };
        // `store` is the latency-critical chain: it is flushed once per
        // chunk (see `reverb`); the write, off that chain, every sample.
        self.store = y * (1.0 - damp) + self.store * damp;
        self.buf[self.at] = flush(x + self.store * feedback);
        self.at += 1;
        if self.at >= self.len {
            self.at = 0;
        }
        y
    }
}

struct Allpass {
    buf: Box<[f32]>,
    len: usize,
    at: usize,
    fresh: usize,
}

impl Allpass {
    fn new(max: usize) -> Allpass {
        Allpass {
            buf: vec![0.0; max].into_boxed_slice(),
            len: max,
            at: 0,
            fresh: 0,
        }
    }

    #[inline(always)]
    fn run<const WARM: bool>(&mut self, x: f32) -> f32 {
        let b = if WARM || self.fresh >= self.len {
            self.buf[self.at]
        } else {
            self.fresh += 1;
            0.0
        };
        self.buf[self.at] = flush(x + b * 0.5);
        self.at += 1;
        if self.at >= self.len {
            self.at = 0;
        }
        b - x
    }
}

/// Drive's soft clipper, f(x) = 1.5x − 0.5x³ inside ±1, and its
/// anti-derivative (for first-order anti-aliasing).
#[inline(always)]
fn clip(x: f32) -> f32 {
    if x.abs() < 1.0 {
        1.5 * x - 0.5 * x * x * x
    } else {
        x.signum()
    }
}

#[inline(always)]
fn clip_integral(x: f32) -> f32 {
    let a = x.abs();
    if a < 1.0 {
        let x2 = x * x;
        0.75 * x2 - 0.125 * x2 * x2
    } else {
        a - 0.375
    }
}

/// One effect in a chain, with its own state.
struct Slot {
    kind: Kind,
    rate: u32,
    /// Output level of this effect: 1 while running, to 0 while switching.
    fade: f32,
    /// Send / engage level (ramped): 0 off, 1 on.
    wet: f32,
    /// Depth, glided.
    depth: f32,
    was_on: bool,
    /// Echo, ping-pong, roll and brake.
    line: Line,
    echo_len: f64,
    echo_lp: [f32; 2],
    echo_hp: [f32; 2],
    // Rate-dependent constants (set in `tune`).
    echo_lp_c: f32,
    echo_hp_c: f32,
    slew_2ms: f32,
    // Reverb.
    combs: [Vec<Comb>; 2],
    allpasses: [Vec<Allpass>; 2],
    /// Every comb and all-pass has lapped since the reset.
    room_warm: bool,
    // Flanger.
    flange: Line,
    // Crush: held sample and sample-and-hold phase.
    held: [f32; 2],
    hold_phase: f32,
    // Trans gate level (smoothed).
    gate: f32,
    // Roll / Brake / Noise: frames since it engaged, where it started in the
    // delay line, and the brake's read lag.
    engaged: usize,
    start: usize,
    lag: f64,
    // Phaser: all-pass states per side, feedback memory.
    ap: [[f32; 6]; 2],
    ph_fb: [f32; 2],
    // Noise: generator and band-pass (state-variable) per side.
    rng: u32,
    svf: [[f32; 2]; 2],
    // Sweep: ladder stage states per side.
    ladder: [[f32; 4]; 2],
    // Drive: last pre-gain input (anti-aliasing) and tone filter per side.
    drive_x: [f32; 2],
    drive_lp: [f32; 2],
    /// Frames since anything audible entered the tails, and how long they
    /// ring (to stop processing once silent).
    quiet: usize,
    ring: usize,
}

impl Slot {
    fn new() -> Slot {
        let scale = MAX_RATE as f64 / 44_100.0;
        let side = |s: usize| {
            (
                COMBS
                    .iter()
                    .map(|l| Comb::new(((l + s) as f64 * scale) as usize + 1))
                    .collect::<Vec<_>>(),
                ALLPASSES
                    .iter()
                    .map(|l| Allpass::new(((l + s) as f64 * scale) as usize + 1))
                    .collect::<Vec<_>>(),
            )
        };
        let (cl, al) = side(0);
        let (cr, ar) = side(SPREAD);
        Slot {
            kind: Kind::Echo,
            rate: 0,
            fade: 1.0,
            wet: 0.0,
            depth: 0.5,
            was_on: false,
            line: Line::new(ECHO_MAX),
            echo_len: 0.0,
            echo_lp: [0.0; 2],
            echo_hp: [0.0; 2],
            echo_lp_c: 0.0,
            echo_hp_c: 0.0,
            slew_2ms: 1.0,
            combs: [cl, cr],
            allpasses: [al, ar],
            room_warm: false,
            flange: Line::new(FLANGE_MAX),
            held: [0.0; 2],
            hold_phase: 1.0,
            gate: 1.0,
            engaged: 0,
            start: 0,
            lag: 0.0,
            ap: [[0.0; 6]; 2],
            ph_fb: [0.0; 2],
            rng: 0x9e37_79b9,
            svf: [[0.0; 2]; 2],
            ladder: [[0.0; 4]; 2],
            drive_x: [0.0; 2],
            drive_lp: [0.0; 2],
            quiet: usize::MAX,
            ring: 0,
        }
    }

    /// Loop lengths and filter constants for the graph rate (no
    /// allocation: the buffers are sized for [`MAX_RATE`]).
    fn tune(&mut self, rate: u32) {
        self.rate = rate;
        let scale = rate as f64 / 44_100.0;
        for (s, spread) in [(0, 0usize), (1, SPREAD)] {
            for (c, l) in self.combs[s].iter_mut().zip(COMBS) {
                c.len = (((l + spread) as f64 * scale) as usize).clamp(1, c.buf.len());
                c.at %= c.len;
            }
            for (a, l) in self.allpasses[s].iter_mut().zip(ALLPASSES) {
                a.len = (((l + spread) as f64 * scale) as usize).clamp(1, a.buf.len());
                a.at %= a.len;
            }
        }
        let pole = |hz: f32| (-2.0 * std::f32::consts::PI * hz / rate as f32).exp();
        self.echo_lp_c = pole(5000.0);
        self.echo_hp_c = pole(180.0);
        self.slew_2ms = 1.0 - (-1.0 / (0.002 * rate as f32)).exp();
    }

    /// Forget all sound, in O(1).
    fn reset(&mut self) {
        self.line.reset();
        self.flange.reset();
        for s in 0..2 {
            for c in &mut self.combs[s] {
                c.fresh = 0;
                c.store = 0.0;
            }
            for a in &mut self.allpasses[s] {
                a.fresh = 0;
            }
        }
        self.room_warm = false;
        self.echo_len = 0.0;
        self.echo_lp = [0.0; 2];
        self.echo_hp = [0.0; 2];
        self.ap = [[0.0; 6]; 2];
        self.ph_fb = [0.0; 2];
        self.svf = [[0.0; 2]; 2];
        self.ladder = [[0.0; 4]; 2];
        self.drive_x = [0.0; 2];
        self.drive_lp = [0.0; 2];
        self.hold_phase = 1.0;
        self.gate = 1.0;
        self.quiet = usize::MAX;
    }

    /// Anything to do? (on, fading, or a tail still ringing)
    fn busy(&self, on: bool) -> bool {
        on || self.wet > 0.0 || self.quiet < self.ring
    }

    /// Process one block in place. `want` is what this slot should run
    /// (None: nothing — its tail rings out).
    #[allow(clippy::too_many_arguments)]
    fn process(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        want: Option<Kind>,
        on: bool,
        beats: f64,
        depth: f32,
        beat: Beat,
    ) {
        let on = on && want.is_some();
        let want = want.unwrap_or(self.kind);
        if !self.busy(on) {
            // Silent: a different effect can start right away.
            if want != self.kind {
                self.kind = want;
                self.reset();
            }
            self.fade = 1.0;
            self.was_on = false;
            self.depth = depth;
            return;
        }
        let n = l.len().min(r.len());
        let rate = self.rate as f32;
        let fade_step = CHUNK as f32 / (FADE_S * rate);
        let glide = 1.0 - (-(CHUNK as f32) / (GLIDE_S * rate)).exp();
        let mut off = 0;
        while off < n {
            let m = CHUNK.min(n - off);
            let part = m as f32 / CHUNK as f32;
            let switching = want != self.kind;
            let engage = on && !switching;
            if engage && !self.was_on {
                // Roll, Brake and Noise lock onto the moment they engage.
                self.engaged = 0;
                self.start = self.line.at;
                self.lag = 0.0;
            }
            self.was_on = engage;
            let w0 = self.wet;
            let w1 = approach(w0, if engage { 1.0 } else { 0.0 }, fade_step * part);
            let f0 = self.fade;
            let f1 = approach(f0, if switching { 0.0 } else { 1.0 }, fade_step * part);
            self.depth += (depth - self.depth) * glide * part;
            let (cl, cr) = (&mut l[off..off + m], &mut r[off..off + m]);
            let fading = f0 < 1.0 || f1 < 1.0;
            let mut dry = [[0.0f32; CHUNK]; 2];
            if fading {
                dry[0][..m].copy_from_slice(cl);
                dry[1][..m].copy_from_slice(cr);
            }
            self.run(cl, cr, w0, (w1 - w0) / m as f32, beats, beat.at(off));
            if fading {
                let df = (f1 - f0) / m as f32;
                for i in 0..m {
                    let g = f0 + df * (i + 1) as f32;
                    cl[i] = dry[0][i] + (cl[i] - dry[0][i]) * g;
                    cr[i] = dry[1][i] + (cr[i] - dry[1][i]) * g;
                }
            }
            self.wet = w1;
            self.fade = f1;
            if switching && f1 == 0.0 {
                // The old effect is out: the new one starts clean.
                self.kind = want;
                self.reset();
                self.wet = 0.0;
                self.fade = 1.0;
                self.was_on = false;
            }
            off += m;
        }
        if !self.kind.has_tail() {
            self.quiet = usize::MAX;
        }
    }

    /// One chunk (≤ [`CHUNK`] frames) of the running effect.
    fn run(&mut self, l: &mut [f32], r: &mut [f32], w0: f32, dw: f32, beats: f64, beat: Beat) {
        let d = self.depth.clamp(0.0, 1.0);
        let len = beats * beat.frames;
        match self.kind {
            Kind::Echo => self.echo(l, r, w0, dw, d, len.min((ECHO_MAX - 2) as f64)),
            Kind::Reverb => self.reverb(l, r, w0, dw, d),
            Kind::Flanger => self.flanger(l, r, w0, dw, d, beat.cycle(beats)),
            Kind::Trans => self.trans(l, r, w0, dw, d, beat.cycle(beats)),
            Kind::Crush => self.crush(l, r, w0, dw, d),
            Kind::Roll => self.roll(l, r, w0, dw, (len as usize).clamp(64, ECHO_MAX - 1)),
            Kind::Brake => self.brake(l, r, w0, dw, len.clamp(256.0, (ECHO_MAX / 2) as f64)),
            Kind::PingPong => self.pingpong(l, r, w0, dw, d, len.clamp(1.0, (ECHO_MAX - 2) as f64)),
            Kind::Phaser => self.phaser(l, r, w0, dw, d, beat.cycle(beats)),
            Kind::Noise => self.noise(l, r, w0, dw, d, len),
            Kind::Sweep => self.sweep(l, r, w0, dw, d, beat.cycle(beats)),
            Kind::Tremolo => self.tremolo(l, r, w0, dw, d, beat.cycle(beats)),
            Kind::Pan => self.pan(l, r, w0, dw, d, beat.cycle(beats)),
            Kind::Drive => self.drive(l, r, w0, dw, d),
        }
    }

    fn echo(&mut self, l: &mut [f32], r: &mut [f32], w0: f32, dw: f32, d: f32, len: f64) {
        // Delay time glides (tempo changes don't click); feedback and
        // level follow the knob; the feedback path is band-limited like a
        // tape echo, so repeats get darker and thinner.
        if self.echo_len == 0.0 {
            self.echo_len = len;
        }
        let fb = 0.25 + 0.6 * d;
        let level = 0.55 + 0.45 * d;
        let (lp, hp) = (self.echo_lp_c, self.echo_hp_c);
        self.ring = len as usize + 1;
        for i in 0..l.len() {
            self.echo_len += (len - self.echo_len) * 0.0005;
            let dist = self.echo_len.max(1.0);
            let w = w0 + dw * i as f32;
            let mut written = [0.0; 2];
            for (side, x) in [&mut l[i], &mut r[i]].into_iter().enumerate() {
                let delayed = self.line.read(side, dist);
                // Feedback filters: low-pass, then high-pass.
                self.echo_lp[side] = flush(delayed * (1.0 - lp) + self.echo_lp[side] * lp);
                let lo = self.echo_lp[side];
                let hi = lo - self.echo_hp[side];
                self.echo_hp[side] = flush(lo * (1.0 - hp) + self.echo_hp[side] * hp);
                // Only the effect's "send" feeds the line: off = spill-over.
                written[side] = *x * w + hi * fb;
                if written[side].abs() > 1e-4 {
                    self.quiet = 0;
                }
                *x += delayed * level;
            }
            self.line.push(written[0], written[1]);
            self.quiet = self.quiet.saturating_add(1);
        }
    }

    fn reverb(&mut self, l: &mut [f32], r: &mut [f32], w0: f32, dw: f32, d: f32) {
        // Stop once the room has stayed below −100 dB for 250 ms (several
        // trips round its longest loop: it can't come back).
        self.ring = self.rate as usize / 4;
        if !self.room_warm {
            self.room_warm = self.combs.iter().flatten().all(|c| c.fresh >= c.len)
                && self.allpasses.iter().flatten().all(|a| a.fresh >= a.len);
        }
        if self.room_warm {
            self.room::<true>(l, r, w0, dw, d);
        } else {
            self.room::<false>(l, r, w0, dw, d);
        }
        for c in self.combs.iter_mut().flatten() {
            c.store = flush(c.store);
        }
    }

    fn room<const WARM: bool>(&mut self, l: &mut [f32], r: &mut [f32], w0: f32, dw: f32, d: f32) {
        let room = 0.72 + 0.24 * d;
        let damp = 0.3;
        let wet = 0.25 + 0.55 * d;
        for i in 0..l.len() {
            let w = w0 + dw * i as f32;
            let input = (l[i] + r[i]) * 0.015 * w;
            for (side, x) in [&mut l[i], &mut r[i]].into_iter().enumerate() {
                let mut acc = 0.0;
                for c in &mut self.combs[side] {
                    acc += c.run::<WARM>(input, room, damp);
                }
                for a in &mut self.allpasses[side] {
                    acc = a.run::<WARM>(acc);
                }
                if acc.abs() > 1e-5 || input != 0.0 {
                    self.quiet = 0;
                }
                *x += acc * wet;
            }
            self.quiet = self.quiet.saturating_add(1);
        }
    }

    fn flanger(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        w0: f32,
        dw: f32,
        d: f32,
        (ph0, inc): (f64, f64),
    ) {
        let rate = self.rate as f32;
        let (min, span) = (0.0008 * rate, (0.0006 + 0.0056 * d) * rate);
        let fb = 0.4 + 0.45 * d;
        for i in 0..l.len() {
            let w = w0 + dw * i as f32;
            let ph = ph0 + inc * i as f64;
            let mut written = [0.0; 2];
            // Triangle LFO on the grid, the right side a quarter turn ahead.
            for (side, x) in [&mut l[i], &mut r[i]].into_iter().enumerate() {
                let p = (ph + side as f64 * 0.25).fract();
                let tri = (1.0 - (2.0 * p - 1.0).abs()) as f32;
                let delay = (min + span * tri).clamp(1.0, (FLANGE_MAX - 2) as f32);
                let delayed = self.flange.read(side, delay as f64);
                // Normalised feedback: resonant, but never above unity.
                written[side] = flush(*x * (1.0 - fb) + delayed * fb);
                // Equal mix at full wet: the comb notches sweep.
                *x = *x * (1.0 - 0.5 * w) + delayed * 0.5 * w;
            }
            self.flange.push(written[0], written[1]);
        }
    }

    fn trans(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        w0: f32,
        dw: f32,
        d: f32,
        (ph0, inc): (f64, f64),
    ) {
        // Open for the first half of each cut, locked to the grid; edges
        // smoothed over ~2 ms so the chop is hard but click-free.
        let smooth = self.slew_2ms;
        for i in 0..l.len() {
            let w = w0 + dw * i as f32;
            let ph = (ph0 + inc * i as f64).fract();
            let open = if ph < 0.5 { 1.0 } else { 1.0 - d };
            self.gate += (open - self.gate) * smooth;
            let g = 1.0 - w * (1.0 - self.gate);
            l[i] *= g;
            r[i] *= g;
        }
    }

    fn crush(&mut self, l: &mut [f32], r: &mut [f32], w0: f32, dw: f32, d: f32) {
        // Depth 0 → 12 bits, full rate; 1 → 3 bits, 2 kHz. The hold rate is
        // in hertz, so it sounds the same at any graph rate.
        let bits = 12.0 - 9.0 * d;
        let steps = 2f32.powf(bits - 1.0);
        let step = (48_000.0 / self.rate as f32) / (1.0 + 23.0 * d);
        for i in 0..l.len() {
            let w = w0 + dw * i as f32;
            if self.hold_phase >= 1.0 {
                self.hold_phase -= self.hold_phase.floor();
                self.held = [
                    (l[i] * steps).round() / steps,
                    (r[i] * steps).round() / steps,
                ];
            }
            self.hold_phase += step;
            l[i] += (self.held[0] - l[i]) * w;
            r[i] += (self.held[1] - r[i]) * w;
        }
    }

    /// Beat repeat. The first `len` frames after engaging are recorded
    /// while they play; then that slice repeats, with 2 ms edges so the
    /// seams don't click. Off: the live deck (which kept playing) returns.
    fn roll(&mut self, l: &mut [f32], r: &mut [f32], w0: f32, dw: f32, len: usize) {
        let edge = (self.rate as usize / 500).clamp(8, len / 4);
        let buf = &mut self.line.buf;
        for i in 0..l.len() {
            let w = w0 + dw * i as f32;
            let k = self.engaged;
            let (yl, yr) = if k < len {
                let at = (self.start + k) % ECHO_MAX;
                buf[0][at] = l[i];
                buf[1][at] = r[i];
                (l[i], r[i])
            } else {
                let pos = k % len;
                let at = (self.start + pos) % ECHO_MAX;
                let g = if pos < edge {
                    pos as f32 / edge as f32
                } else if pos + edge > len {
                    (len - pos) as f32 / edge as f32
                } else {
                    1.0
                };
                (buf[0][at] * g, buf[1][at] * g)
            };
            if self.was_on {
                self.engaged += 1;
            }
            l[i] += (yl - l[i]) * w;
            r[i] += (yr - r[i]) * w;
        }
        self.line.at = (self.start + self.engaged.min(len)) % ECHO_MAX;
    }

    /// Vinyl brake: the read head slows from full speed to a stop over
    /// `len` frames (quadratic, like a platter losing momentum) while the
    /// input keeps being written; then silence until off.
    fn brake(&mut self, l: &mut [f32], r: &mut [f32], w0: f32, dw: f32, len: f64) {
        for i in 0..l.len() {
            let w = w0 + dw * i as f32;
            self.line.push(l[i], r[i]);
            let t = (self.engaged as f64 / len).min(1.0);
            let speed = (1.0 - t) * (1.0 - t);
            let (yl, yr) = if t >= 1.0 {
                (0.0, 0.0)
            } else {
                self.lag += 1.0 - speed;
                let fade = 1.0 - t as f32;
                (
                    self.line.read(0, self.lag + 1.0) * fade,
                    self.line.read(1, self.lag + 1.0) * fade,
                )
            };
            if self.was_on {
                self.engaged += 1;
            }
            l[i] += (yl - l[i]) * w;
            r[i] += (yr - r[i]) * w;
        }
    }

    /// Stereo ping-pong delay: the mono send enters the left line, the left
    /// line feeds the right, the right feeds back into the left.
    fn pingpong(&mut self, l: &mut [f32], r: &mut [f32], w0: f32, dw: f32, d: f32, len: f64) {
        let fb = 0.3 + 0.5 * d;
        let level = 0.6 + 0.4 * d;
        let delay = len.round().max(1.0);
        self.ring = delay as usize * 2 + 1;
        for i in 0..l.len() {
            let w = w0 + dw * i as f32;
            let (dl, dr) = (self.line.read(0, delay), self.line.read(1, delay));
            let send = (l[i] + r[i]) * 0.5 * w;
            self.line.push(flush(send + dr * fb), dl);
            if send.abs() > 1e-4 || dl.abs() > 1e-4 || dr.abs() > 1e-4 {
                self.quiet = 0;
            }
            self.quiet = self.quiet.saturating_add(1);
            l[i] += dl * level;
            r[i] += dr * level;
        }
    }

    /// Phaser: six first-order all-passes per side, their corner swept
    /// 200 Hz … 3.2 kHz (log) once per cycle on the grid; the right side a
    /// quarter turn ahead. Coefficients once per chunk.
    fn phaser(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        w0: f32,
        dw: f32,
        d: f32,
        (ph, _): (f64, f64),
    ) {
        let fb = 0.2 + 0.5 * d;
        let rate = self.rate as f64;
        let coef = |ph: f64| -> f32 {
            let tri = 1.0 - (2.0 * ph.fract() - 1.0).abs();
            let f = 200.0 * 16f64.powf(tri);
            let t = (std::f64::consts::PI * f / rate).tan();
            ((t - 1.0) / (t + 1.0)) as f32
        };
        let c = [coef(ph), coef(ph + 0.25)];
        for i in 0..l.len() {
            let w = w0 + dw * i as f32;
            for (side, x) in [&mut l[i], &mut r[i]].into_iter().enumerate() {
                let mut y = *x + self.ph_fb[side] * fb;
                for st in self.ap[side].iter_mut() {
                    // First-order all-pass: y = c·x + s; s = x − c·y.
                    let out = c[side] * y + *st;
                    *st = flush(y - c[side] * out);
                    y = out;
                }
                self.ph_fb[side] = flush(y);
                *x = *x * (1.0 - 0.5 * w) + y * 0.5 * w;
            }
        }
    }

    /// Noise riser: white noise through a band-pass whose centre climbs
    /// from 300 Hz to 12 kHz (and gets louder) over `period` frames after
    /// engaging, then holds at the top. Added on top of the music.
    fn noise(&mut self, l: &mut [f32], r: &mut [f32], w0: f32, dw: f32, d: f32, period: f64) {
        let level = 0.08 + 0.22 * d;
        let t = (self.engaged as f64 / period.max(1.0)).min(1.0);
        let f = 300.0 * 40f64.powf(t);
        let g = (2.0 * (std::f64::consts::PI * f / self.rate as f64).sin()).min(1.8) as f32;
        let gain = level * (0.3 + 0.7 * t as f32);
        let q = 0.35f32;
        for i in 0..l.len() {
            let w = w0 + dw * i as f32;
            for (side, x) in [&mut l[i], &mut r[i]].into_iter().enumerate() {
                self.rng ^= self.rng << 13;
                self.rng ^= self.rng >> 17;
                self.rng ^= self.rng << 5;
                let white = self.rng as f32 / u32::MAX as f32 * 2.0 - 1.0;
                // Chamberlin state-variable filter: band-pass output.
                let [low, band] = &mut self.svf[side];
                *low = flush(*low + g * *band);
                let high = white - *low - q * *band;
                *band = flush(*band + g * high);
                *x += *band * gain * w;
            }
            if self.was_on {
                self.engaged += 1;
            }
        }
    }

    /// Filter sweep: a 4-pole ladder low-pass (topology-preserving,
    /// zero-delay feedback, solved exactly per sample) whose cutoff falls
    /// from 16 kHz and rises back once per cycle on the grid (open on the
    /// cycle's first beat). Depth: how far down, and how resonant. The
    /// input is scaled by 1 + k so the pass band stays at unity.
    fn sweep(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        w0: f32,
        dw: f32,
        d: f32,
        (ph, _): (f64, f64),
    ) {
        let rate = self.rate as f32;
        let hi = 16_000f32.min(0.45 * rate);
        let lo = hi * (-(3.0 + 5.0 * d)).exp2();
        let open = 0.5 + 0.5 * cos_turns(ph);
        let fc = lo * (hi / lo).powf(open);
        let g = (std::f32::consts::PI * fc / rate).tan();
        let big_g = g / (1.0 + g);
        let (g2, g3) = (big_g * big_g, big_g * big_g * big_g);
        let g4 = g2 * g2;
        // Resonance: the compensated ladder peaks (1 + k) / (4 − k) at
        // cutoff; k ≤ 2.2 keeps that within +5 dB.
        let k = 0.3 + 1.9 * d;
        let inv = 1.0 / (1.0 + g);
        let norm = 1.0 / (1.0 + k * g4);
        for i in 0..l.len() {
            let w = w0 + dw * i as f32;
            for (side, x) in [&mut l[i], &mut r[i]].into_iter().enumerate() {
                let s = &mut self.ladder[side];
                let sum = (g3 * s[0] + g2 * s[1] + big_g * s[2] + s[3]) * inv;
                let mut y = (*x * (1.0 + k) - k * sum) * norm;
                for st in s.iter_mut() {
                    let v = (y - *st) * big_g;
                    let out = v + *st;
                    *st = flush(out + v);
                    y = out;
                }
                *x += (y - *x) * w;
            }
        }
    }

    /// Tremolo: level pulses once per cycle, loudest on the grid's beat.
    /// Depth: how deep, and how square.
    fn tremolo(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        w0: f32,
        dw: f32,
        d: f32,
        (ph0, inc): (f64, f64),
    ) {
        let amount = 0.35 + 0.65 * d;
        let sharp = 1.0 + 7.0 * d;
        for i in 0..l.len() {
            let w = w0 + dw * i as f32;
            let shape = (cos_turns(ph0 + inc * i as f64) * sharp).clamp(-1.0, 1.0);
            let g = 1.0 - w * amount * (0.5 - 0.5 * shape);
            l[i] *= g;
            r[i] *= g;
        }
    }

    /// Auto-pan: equal power, unity in the centre (engaging it doesn't
    /// change the level), one left-right swing per cycle. Gains are exact
    /// at chunk edges and interpolated between.
    fn pan(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        w0: f32,
        dw: f32,
        d: f32,
        (ph0, inc): (f64, f64),
    ) {
        let n = l.len();
        let width = 0.35 + 0.65 * d;
        let gains = |ph: f64, w: f32| {
            let p = w * width * sin_turns(ph);
            let theta = (p + 1.0) * std::f32::consts::FRAC_PI_4;
            (
                std::f32::consts::SQRT_2 * theta.cos(),
                std::f32::consts::SQRT_2 * theta.sin(),
            )
        };
        let (a, b) = (
            gains(ph0, w0),
            gains(ph0 + inc * n as f64, w0 + dw * n as f32),
        );
        let step = 1.0 / n as f32;
        for i in 0..n {
            let t = i as f32 * step;
            l[i] *= a.0 + (b.0 - a.0) * t;
            r[i] *= a.1 + (b.1 - a.1) * t;
        }
    }

    /// Drive: pre-gain up to +30 dB into a cubic soft clipper, anti-aliased
    /// with its anti-derivative (ADAA: far fewer aliases than clipping per
    /// sample, no oversampling), level-matched at -12 dBFS, then a tone
    /// filter that darkens as it drives harder.
    fn drive(&mut self, l: &mut [f32], r: &mut [f32], w0: f32, dw: f32, d: f32) {
        let pre = 10f32.powf(1.5 * d);
        let makeup = 0.25 / clip(0.25 * pre);
        let fc = 16_000.0 - 9_000.0 * d;
        let lp = (-2.0 * std::f32::consts::PI * fc.min(0.45 * self.rate as f32) / self.rate as f32)
            .exp();
        for i in 0..l.len() {
            let w = w0 + dw * i as f32;
            for (side, x) in [&mut l[i], &mut r[i]].into_iter().enumerate() {
                let xin = *x * pre;
                let prev = self.drive_x[side];
                let dx = xin - prev;
                let y = if dx.abs() > 1e-4 {
                    (clip_integral(xin) - clip_integral(prev)) / dx
                } else {
                    clip(0.5 * (xin + prev))
                };
                self.drive_x[side] = xin;
                let y = y * makeup;
                self.drive_lp[side] = flush(y * (1.0 - lp) + self.drive_lp[side] * lp);
                *x += (self.drive_lp[side] - *x) * w;
            }
        }
    }
}

/// One channel's effect chain.
pub struct Fx {
    rate: u32,
    slots: [Slot; SLOTS],
    /// Beat position when no grid is given.
    free_phase: f64,
}

impl Fx {
    pub fn new() -> Fx {
        Fx {
            rate: 0,
            slots: std::array::from_fn(|_| Slot::new()),
            free_phase: 0.0,
        }
    }

    /// Discard a previous playback identity's tails in constant time.
    pub fn reset(&mut self) {
        for slot in &mut self.slots {
            slot.reset();
            slot.wet = 0.0;
            slot.fade = 1.0;
            slot.was_on = false;
            slot.held = [0.0; 2];
        }
        self.free_phase = 0.0;
    }

    /// Anything to do this block? (on, fading, or a tail still ringing)
    pub fn active(&self, s: &Settings) -> bool {
        s.on || self.slots.iter().any(|sl| sl.busy(false))
    }

    /// Process one block in place (the channel after its fader).
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32], s: Settings, beat: Beat, rate: u32) {
        let n = l.len().min(r.len());
        let (l, r) = (&mut l[..n], &mut r[..n]);
        if rate != self.rate {
            self.rate = rate;
            for sl in &mut self.slots {
                sl.tune(rate);
                sl.reset();
            }
        }
        let beat = if beat.locked {
            beat
        } else {
            Beat {
                phase: self.free_phase,
                ..beat
            }
        };
        let beats = s.beats as f64;
        let depth = s.depth.clamp(0.0, 1.0);
        for (sl, want) in self.slots.iter_mut().zip(s.slots()) {
            sl.process(l, r, want, s.on, beats, depth, beat);
        }
        // Wrap on a common multiple of every cycle length (¾ … 32 beats).
        self.free_phase = (self.free_phase + beat.per_frame * n as f64).rem_euclid(384.0);
    }
}

impl Default for Fx {
    fn default() -> Self {
        Fx::new()
    }
}

#[cfg(test)]
#[path = "fx_tests.rs"]
mod tests;

#[cfg(test)]
mod level {
    use super::*;

    /// How much louder a steady signal comes out of each effect at the AI
    /// DJ's settings (cargo test --release -p patch-engine fx_levels -- --ignored --nocapture).
    #[test]
    #[ignore]
    fn fx_levels() {
        let rate = 48_000u32;
        for (kind, beats, depth) in [
            (Kind::Echo, 0.75f32, 0.55f32),
            (Kind::Echo, 0.5, 0.6),
            (Kind::Reverb, 1.0, 0.5),
        ] {
            let mut fx = Fx::new();
            let s = Settings {
                kind,
                on: true,
                beats,
                depth,
                ..Settings::default()
            };
            let beat = Beat {
                frames: rate as f64 * 60.0 / 126.0,
                phase: 0.0,
                per_frame: 126.0 / 60.0 / rate as f64,
                locked: true,
            };
            let n = rate as usize * 4;
            let (mut l, mut r): (Vec<f32>, Vec<f32>) = (0..n)
                .map(|i| {
                    let v =
                        (i as f32 * 0.03).sin() * 0.3 * if (i / 6000) % 4 == 0 { 1.0 } else { 0.2 };
                    (v, v)
                })
                .unzip();
            let input: f64 = l.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / n as f64;
            for c in (0..n).step_by(256) {
                let e = (c + 256).min(n);
                fx.process(
                    &mut l[c..e],
                    &mut r[c..e],
                    s,
                    Beat {
                        phase: beat.phase + c as f64 * beat.per_frame,
                        ..beat
                    },
                    rate,
                );
            }
            let out: f64 =
                l[n / 2..].iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / (n / 2) as f64;
            let peak_in = 0.3f64;
            let peak_out = l.iter().fold(0.0f32, |m, v| m.max(v.abs())) as f64;
            eprintln!(
                "{kind:?} beats {beats} depth {depth}: level {:+.1} dB, peak {:+.1} dB",
                10.0 * (out / input).log10(),
                20.0 * (peak_out / peak_in).log10()
            );
        }
    }
}
