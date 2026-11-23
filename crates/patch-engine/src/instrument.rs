//! The pattern instrument: what the Tidal Unit's events play.
//!
//! Drums from the Sequencer's bank (Patch's own designs) at any speed and
//! pitch, nine small synths (piano, saw, square, sine, FM, organ, pluck,
//! reese, pad), and samples from the sample folders ([`crate::sample_bank`])
//! in stereo, any part of them, backwards, looped, accelerating. Each voice
//! has an envelope, a low- and high-pass filter, waveshaping, bit crush,
//! sample-rate reduction and pan, and may sit in a cut group; then a shared
//! delay and room the voices send to (Tidal's `delay` and `room`). Events
//! arrive in their own real-time queue as `RtEvent { kind: Play }` and
//! start on their sample.
//!
//! Real-time safe: everything is allocated in [`Instrument::new`]; a block
//! only reads the queue, writes preallocated state and sums into the
//! channel's buffers. A full voice table steals the quietest voice.

use crate::sample_bank::{Bank, Load, SampleBuf};
use patch_pattern::instrument::{SAMPLE_BASE, SYNTH_BASE};
use patch_pattern::rt::{Queue, RtEvent, RtKind, RtParams};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Simultaneous voices.
pub const VOICES: usize = 64;
/// Loads the audio thread applies per block (the rest wait a block).
const LOADS: usize = 64;
/// A sample's start and end fade, seconds (no clicks; SuperDirt's 1 ms).
const SAMPLE_FADE: f32 = 0.001;
/// Events taken from the queue per block (more are dropped and counted).
const DUE: usize = 256;
/// The delay line's longest time, seconds, at the highest rate.
const DELAY_S: usize = 2;
const MAX_RATE: usize = 192_000;
/// The instrument's level before the channel fader: room for chords.
const HEADROOM: f32 = 0.5;

/// Drums are one-shots: a short decay each (the envelope's time constant is
/// a quarter of this) instead of a full-level sustain for the whole sample.
const DRUM_DECAY: [f32; 8] = [1.2, 0.8, 0.16, 1.0, 0.7, 0.7, 0.2, 0.16];
/// A default high-pass for the noisy voices, so hats and shakers leave the
/// kick's and bass's room. An explicit `# hcutoff` still wins.
const DRUM_HP: [f32; 8] = [0.0, 0.0, 6000.0, 4500.0, 0.0, 0.0, 1500.0, 5000.0];

/// The instrument's own gentle saturation before the channel fader: dense
/// chords keep their shape instead of hitting the master limiter together.
#[inline]
fn saturate(x: f32) -> f32 {
    x / (1.0 + 0.35 * x * x)
}

/// What a voice is worth to keep when the table is full. A hit in its
/// attack is worth far more than a decaying tail or a released note.
fn steal_score(v: &Voice) -> f32 {
    match v.stage {
        Stage::Attack => 1000.0 + v.env * v.level,
        Stage::Decay => v.env * v.level,
        Stage::Release => v.env * v.level * 0.5,
        Stage::Off => f32::NEG_INFINITY,
    }
}

/// What the control side shares with the instrument.
pub struct Shared {
    /// The Tidal Unit's events (its service is the only producer).
    pub queue: Queue,
    /// Events that found the block's table full.
    pub dropped: AtomicU64,
    /// Voices sounding after the last block.
    pub active: AtomicU32,
    /// Bumped to let every voice go (the pattern stopped).
    pub release: AtomicU32,
    /// The samples, and the mailboxes that carry them.
    pub bank: Bank,
    /// Sample events whose slot was empty when they started.
    pub missing: AtomicU64,
    /// Replaced buffers the audio thread couldn't give back (kept, not
    /// freed there).
    pub leaked: AtomicU64,
}

impl Default for Shared {
    fn default() -> Shared {
        Shared {
            queue: Queue::new(2048),
            dropped: AtomicU64::new(0),
            active: AtomicU32::new(0),
            release: AtomicU32::new(0),
            bank: Bank::default(),
            missing: AtomicU64::new(0),
            leaked: AtomicU64::new(0),
        }
    }
}

/// A sine of a phase in cycles (0 … 1), by polynomial: cheap, ~-90 dB.
#[inline]
fn sin01(x: f64) -> f32 {
    let x = (x - x.floor()) as f32;
    // Fold to −0.5 … 0.5, then an odd polynomial for sin(2πx).
    let x = if x > 0.5 { x - 1.0 } else { x };
    let y = x * std::f32::consts::TAU;
    let (y, s) = if y > std::f32::consts::FRAC_PI_2 {
        (std::f32::consts::PI - y, 1.0)
    } else if y < -std::f32::consts::FRAC_PI_2 {
        (-std::f32::consts::PI - y, 1.0)
    } else {
        (y, 1.0)
    };
    let y2 = y * y;
    s * y * (1.0 - y2 / 6.0 * (1.0 - y2 / 20.0 * (1.0 - y2 / 42.0 * (1.0 - y2 / 72.0))))
}

/// PolyBLEP: the step's correction near a discontinuity (anti-aliasing).
#[inline]
fn blep(t: f64, dt: f64) -> f64 {
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

#[inline]
fn saw(phase: f64, dt: f64) -> f32 {
    let t = phase - phase.floor();
    (2.0 * t - 1.0 - blep(t, dt)) as f32
}

#[inline]
fn square(phase: f64, dt: f64, width: f64) -> f32 {
    let t = phase - phase.floor();
    let mut v = if t < width { 1.0 } else { -1.0 };
    v += blep(t, dt);
    let t2 = (t + 1.0 - width) % 1.0;
    v -= blep(t2, dt);
    v as f32
}

/// A state-variable filter (trapezoidal): low- or high-pass.
#[derive(Clone, Copy, Default)]
struct Svf {
    g: f32,
    k: f32,
    a1: f32,
    a2: f32,
    a3: f32,
    ic1: f32,
    ic2: f32,
    on: bool,
}

impl Svf {
    fn set(&mut self, hz: f32, res: f32, rate: f32) {
        if hz <= 0.0 {
            self.on = false;
            return;
        }
        let hz = hz.clamp(20.0, rate * 0.45);
        self.g = (std::f32::consts::PI * hz / rate).tan();
        self.k = 2.0 - 1.96 * res.clamp(0.0, 1.0);
        self.a1 = 1.0 / (1.0 + self.g * (self.g + self.k));
        self.a2 = self.g * self.a1;
        self.a3 = self.g * self.a2;
        self.on = true;
    }

    #[inline]
    fn run(&mut self, x: f32, high: bool) -> f32 {
        if !self.on {
            return x;
        }
        let v3 = x - self.ic2;
        let v1 = self.a1 * self.ic1 + self.a2 * v3;
        let v2 = self.ic2 + self.a2 * self.ic1 + self.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        if high { x - self.k * v1 - v2 } else { v2 }
    }
}

/// A synth's character: envelope (attack, decay, sustain level, release)
/// and a filter of its own (Hz, 0: none).
struct Model {
    attack: f32,
    decay: f32,
    sustain: f32,
    release: f32,
    cutoff: f32,
}

const MODELS: [Model; 9] = [
    // piano: struck, decays while held
    Model { attack: 0.002, decay: 2.2, sustain: 0.0, release: 0.3, cutoff: 0.0 },
    // saw
    Model { attack: 0.004, decay: 0.12, sustain: 0.8, release: 0.12, cutoff: 6000.0 },
    // square
    Model { attack: 0.003, decay: 0.1, sustain: 0.7, release: 0.1, cutoff: 5000.0 },
    // sine
    Model { attack: 0.005, decay: 0.05, sustain: 0.9, release: 0.18, cutoff: 0.0 },
    // fm
    Model { attack: 0.002, decay: 0.7, sustain: 0.25, release: 0.3, cutoff: 0.0 },
    // organ
    Model { attack: 0.008, decay: 0.05, sustain: 1.0, release: 0.08, cutoff: 0.0 },
    // pluck: its own sweep (see below)
    Model { attack: 0.001, decay: 0.55, sustain: 0.0, release: 0.2, cutoff: 0.0 },
    // reese
    Model { attack: 0.005, decay: 0.2, sustain: 0.85, release: 0.14, cutoff: 900.0 },
    // pad
    Model { attack: 0.35, decay: 0.6, sustain: 0.8, release: 1.1, cutoff: 2500.0 },
];

#[derive(Clone, Copy, Default, PartialEq)]
enum Stage {
    #[default]
    Off,
    Attack,
    Decay,
    Release,
}

#[derive(Clone, Copy, Default)]
struct Voice {
    stage: Stage,
    sound: u32,
    /// Frames since it started.
    age: u64,
    /// Drums and samples: position (frames) and step per frame.
    pos: f64,
    step: f64,
    /// Samples: the step's change per frame (`accelerate`), the part
    /// played (frames), plays left, the edge fade (frames).
    dstep: f64,
    seg_begin: f64,
    seg_end: f64,
    loops: f32,
    fade: f32,
    stereo: bool,
    /// Its cut group (0: none).
    cut: u32,
    /// Sample-rate reduction: frames each value is held, frames left, the value.
    coarse: f32,
    coarse_left: f32,
    held: [f32; 2],
    /// Synths: oscillator phases (cycles), frequency (Hz).
    phase: [f64; 4],
    freq: f64,
    env: f32,
    attack: f32,
    decay: f32,
    sustain_level: f32,
    release: f32,
    /// Frames left before the note lets go (−1: rings out).
    hold: i64,
    level: f32,
    gl: f32,
    gr: f32,
    lp: Svf,
    hp: Svf,
    /// The right side's filters (stereo samples).
    lp2: Svf,
    hp2: Svf,
    cutoff: f32,
    res: f32,
    shape: f32,
    crush: f32,
    delay: f32,
    room: f32,
}

impl Voice {
    fn sounding(&self) -> bool {
        self.stage != Stage::Off
    }
}

/// A comb filter with a damped feedback path (the room's building block).
struct Comb {
    buf: Box<[f32]>,
    len: usize,
    i: usize,
    store: f32,
}

impl Comb {
    fn new() -> Comb {
        Comb {
            buf: vec![0.0; 8192].into_boxed_slice(),
            len: 1,
            i: 0,
            store: 0.0,
        }
    }

    #[inline]
    fn run(&mut self, x: f32, feedback: f32, damp: f32) -> f32 {
        let y = self.buf[self.i];
        self.store = y * (1.0 - damp) + self.store * damp;
        self.buf[self.i] = x + self.store * feedback;
        self.i += 1;
        if self.i >= self.len {
            self.i = 0;
        }
        y
    }
}

struct Allpass {
    buf: Box<[f32]>,
    len: usize,
    i: usize,
}

impl Allpass {
    fn new() -> Allpass {
        Allpass {
            buf: vec![0.0; 4096].into_boxed_slice(),
            len: 1,
            i: 0,
        }
    }

    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        let b = self.buf[self.i];
        let y = b - x;
        self.buf[self.i] = x + b * 0.5;
        self.i += 1;
        if self.i >= self.len {
            self.i = 0;
        }
        y
    }
}

/// The shared room: parallel combs into series all-passes, per side
/// (the classic Schroeder–Moorer design, tuned for 44.1 kHz and scaled).
struct Room {
    combs: [[Comb; 8]; 2],
    passes: [[Allpass; 4]; 2],
    size: f32,
}

const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const PASSES: [usize; 4] = [556, 441, 341, 225];
const SPREAD: usize = 23;

impl Room {
    fn new() -> Room {
        Room {
            combs: std::array::from_fn(|_| std::array::from_fn(|_| Comb::new())),
            passes: std::array::from_fn(|_| std::array::from_fn(|_| Allpass::new())),
            size: 0.5,
        }
    }

    fn tune(&mut self, rate: f32) {
        let k = rate / 44_100.0;
        for side in 0..2 {
            for (c, len) in self.combs[side].iter_mut().zip(COMBS) {
                c.len = (((len + side * SPREAD) as f32 * k) as usize).clamp(1, c.buf.len());
                c.i %= c.len;
            }
            for (a, len) in self.passes[side].iter_mut().zip(PASSES) {
                a.len = (((len + side * SPREAD) as f32 * k) as usize).clamp(1, a.buf.len());
                a.i %= a.len;
            }
        }
    }

    #[inline]
    fn run(&mut self, x: [f32; 2]) -> [f32; 2] {
        let feedback = 0.6 + 0.22 * self.size;
        let input = (x[0] + x[1]) * 0.015;
        let mut out = [0.0; 2];
        for side in 0..2 {
            let mut s = 0.0;
            for c in &mut self.combs[side] {
                s += c.run(input, feedback, 0.25);
            }
            for a in &mut self.passes[side] {
                s = a.run(s);
            }
            out[side] = s;
        }
        out
    }
}

/// The shared stereo delay (ping-free, feedback through each side).
struct Delay {
    buf: [Box<[f32]>; 2],
    i: usize,
    /// Read distance in frames, followed smoothly toward `target`.
    time: f32,
    target: f32,
    feedback: f32,
}

impl Delay {
    fn new() -> Delay {
        Delay {
            buf: std::array::from_fn(|_| vec![0.0; DELAY_S * MAX_RATE].into_boxed_slice()),
            i: 0,
            time: 12_000.0,
            target: 12_000.0,
            feedback: 0.5,
        }
    }

    #[inline]
    fn run(&mut self, x: [f32; 2]) -> [f32; 2] {
        let n = self.buf[0].len();
        self.time += (self.target - self.time) * 0.0005;
        let t = self.time.clamp(1.0, (n - 2) as f32);
        let back = t.floor();
        let frac = t - back;
        let mut out = [0.0; 2];
        for side in 0..2 {
            let b = &mut self.buf[side];
            let a = b[(self.i + n - back as usize) % n];
            let c = b[(self.i + n - back as usize - 1) % n];
            let y = a + (c - a) * frac;
            b[self.i] = x[side] + y * self.feedback;
            out[side] = y;
        }
        self.i = (self.i + 1) % n;
        out
    }
}

/// A buffer the audio thread is done with, back to be freed elsewhere
/// (kept, never freed here, if the mailbox is full).
fn give_back(shared: &Shared, buf: SampleBuf) {
    if let Err(buf) = shared.bank.back.push(buf) {
        std::mem::forget(buf);
        shared.leaked.fetch_add(1, Ordering::Relaxed);
    }
}

/// 4-point Hermite interpolation between `x0` and `x1` at `t` (0 … 1).
#[inline]
fn hermite(xm1: f32, x0: f32, x1: f32, x2: f32, t: f32) -> f32 {
    let c = (x1 - xm1) * 0.5;
    let v = x0 - x1;
    let w = c + v;
    let a = w + v + (x2 - x0) * 0.5;
    let b = w + a;
    ((a * t - b) * t + c) * t + x0
}

/// A buffer's frame at a fractional position (left, right; mono twice).
#[inline]
fn read(b: &SampleBuf, pos: f64) -> [f32; 2] {
    let last = b.frames as isize - 1;
    let i = pos.floor() as isize;
    let t = (pos - i as f64) as f32;
    let ch = b.channels;
    let at = |k: isize, c: usize| b.data[k.clamp(0, last) as usize * ch + c];
    let side = |c: usize| {
        if t == 0.0 {
            at(i, c)
        } else {
            hermite(at(i - 1, c), at(i, c), at(i + 1, c), at(i + 2, c), t)
        }
    };
    if ch > 1 { [side(0), side(1)] } else { let x = side(0); [x, x] }
}

pub struct Instrument {
    bank: [Vec<f32>; 8],
    samples: Box<[Option<SampleBuf>]>,
    voices: Box<[Voice; VOICES]>,
    due: Box<[RtEvent; DUE]>,
    due_n: usize,
    rate: f32,
    delay: Delay,
    room: Room,
    /// Frames the shared effects still ring after the last voice ended.
    tail: u64,
    delay_on: bool,
    room_on: bool,
    released: u32,
}

impl Default for Instrument {
    fn default() -> Instrument {
        Instrument::new()
    }
}

impl Instrument {
    pub fn new() -> Instrument {
        let mut room = Room::new();
        room.tune(48_000.0);
        Instrument {
            bank: std::array::from_fn(crate::sequencer::sound),
            samples: (0..crate::sample_bank::SLOTS).map(|_| None).collect(),
            voices: Box::new([Voice::default(); VOICES]),
            due: Box::new([RtEvent::default(); DUE]),
            due_n: 0,
            rate: 48_000.0,
            delay: Delay::new(),
            room,
            tail: 0,
            delay_on: false,
            room_on: false,
            released: 0,
        }
    }

    fn start(&mut self, e: &RtEvent, shared: &Shared) {
        let p: RtParams = e.params;
        let rate = self.rate;
        let sample = if e.index >= SAMPLE_BASE {
            let slot = (e.index - SAMPLE_BASE) as usize;
            match self.samples.get(slot).and_then(Option::as_ref) {
                Some(b) => Some((slot, b.frames as f64, b.rate, b.channels > 1)),
                None => {
                    shared.missing.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            }
        } else {
            None
        };
        // A cut group: the sound before in it ends now.
        if p.cut > 0 {
            for o in self.voices.iter_mut() {
                if o.sounding() && o.cut == p.cut {
                    o.release = o.release.min(0.01);
                    o.stage = Stage::Release;
                    o.hold = -1;
                }
            }
        }
        // A free voice, else the quietest that is past its attack: stealing
        // a note that just started is what made dense patterns sound chopped.
        let slot = match self.voices.iter().position(|v| !v.sounding()) {
            Some(i) => i,
            None => self
                .voices
                .iter()
                .enumerate()
                .min_by(|a, b| steal_score(a.1).total_cmp(&steal_score(b.1)))
                .map_or(0, |x| x.0),
        };
        let synth = (SYNTH_BASE..SAMPLE_BASE).contains(&e.index);
        let model = &MODELS[((e.index.saturating_sub(SYNTH_BASE)) as usize).min(MODELS.len() - 1)];
        let pan = (p.pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
        let mut v = Voice {
            stage: Stage::Attack,
            sound: e.index,
            level: e.value.clamp(0.0, 4.0) * HEADROOM,
            gl: pan.cos(),
            gr: pan.sin(),
            cutoff: if p.cutoff > 0.0 { p.cutoff } else if synth { model.cutoff } else { 0.0 },
            res: p.resonance,
            shape: p.shape,
            crush: p.crush,
            delay: p.delay,
            room: p.room,
            cut: p.cut,
            coarse: if p.coarse > 1.0 { p.coarse } else { 0.0 },
            ..Voice::default()
        };
        if let Some((_, frames, file_rate, stereo)) = sample {
            let (b, end) = (p.begin.clamp(0.0, 1.0) as f64 * frames, p.end.clamp(0.0, 1.0) as f64 * frames);
            if end - b < 1.0 {
                return;
            }
            let step = p.speed as f64 * file_rate as f64 / rate as f64;
            if step.abs() < 1e-6 {
                return;
            }
            v.seg_begin = b;
            v.seg_end = end;
            v.step = step;
            v.pos = if step < 0.0 { end - 1.0 } else { b };
            // `accelerate`: the speed changes by that much over the sound.
            let out_frames = (end - b) / step.abs();
            v.dstep = p.accelerate as f64 * file_rate as f64 / rate as f64 / out_frames.max(1.0);
            v.loops = p.loops.max(1.0);
            v.fade = (SAMPLE_FADE * rate).max(1.0);
            v.stereo = stereo;
            v.attack = p.attack.max(0.0);
            v.decay = 0.0;
            v.sustain_level = 1.0;
            v.env = 0.0;
            v.release = if p.release >= 0.0 { p.release } else { 0.01 };
            v.hold = if p.sustain >= 0.0 { (p.sustain * rate) as i64 } else { -1 };
        } else if synth {
            v.freq = 440.0 * 2f64.powf((p.note as f64 - 69.0) / 12.0);
            v.attack = if p.attack >= 0.0 { p.attack } else { model.attack };
            v.decay = model.decay;
            v.sustain_level = model.sustain;
            v.release = if p.release >= 0.0 { p.release } else { model.release };
            v.hold = if p.sustain >= 0.0 { (p.sustain * rate) as i64 } else { (0.25 * rate) as i64 };
            // Detuned partners start apart (no phasey start).
            v.phase = [0.0, 0.31, 0.67, 0.13];
        } else {
            let bank = (v.sound as usize).min(7);
            v.step = p.speed.max(0.01) as f64 * 48_000.0 / rate as f64;
            v.attack = p.attack.max(0.0);
            v.decay = DRUM_DECAY[bank];
            v.sustain_level = 0.0;
            v.release = if p.release >= 0.0 { p.release } else { 0.01 };
            v.hold = if p.sustain >= 0.0 { (p.sustain * rate) as i64 } else { -1 };
        }
        v.lp.set(v.cutoff, v.res, rate);
        let hp = if p.hcutoff > 0.0 {
            p.hcutoff
        } else if synth || sample.is_some() {
            0.0
        } else {
            DRUM_HP[(v.sound as usize).min(7)]
        };
        v.hp.set(hp, 0.0, rate);
        v.lp2 = v.lp;
        v.hp2 = v.hp;
        if p.delaytime > 0.0 {
            self.delay.target = (p.delaytime * rate).min((self.delay.buf[0].len() - 2) as f32);
        }
        if p.delayfeedback >= 0.0 {
            self.delay.feedback = p.delayfeedback.min(0.95);
        }
        if p.size >= 0.0 {
            self.room.size = p.size;
        }
        if p.delay > 0.0 {
            self.delay_on = true;
        }
        if p.room > 0.0 {
            self.room_on = true;
        }
        self.voices[slot] = v;
    }

    /// One voice's next frame (left, right; before pan), advancing it.
    #[inline]
    fn voice(bank: &[Vec<f32>; 8], samples: &[Option<SampleBuf>], v: &mut Voice, rate: f32) -> [f32; 2] {
        let dt_s = 1.0 / rate;
        // The envelope.
        match v.stage {
            Stage::Attack => {
                v.env += if v.attack > 0.0 { dt_s / v.attack } else { 1.0 };
                if v.env >= 1.0 {
                    v.env = 1.0;
                    v.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                if v.decay > 0.0 {
                    let k = dt_s / v.decay * 4.0;
                    v.env += (v.sustain_level - v.env) * k.min(1.0);
                }
                if v.sustain_level <= 0.0 && v.env < 1e-4 {
                    v.stage = Stage::Off;
                }
            }
            Stage::Release => {
                let k = if v.release > 0.0 { dt_s / v.release * 5.0 } else { 1.0 };
                v.env -= v.env * k.min(1.0);
                if v.env < 1e-4 {
                    v.stage = Stage::Off;
                }
            }
            Stage::Off => return [0.0; 2],
        }
        if v.hold >= 0 {
            if v.hold == 0 && v.stage != Stage::Release {
                v.stage = Stage::Release;
            }
            v.hold -= 1;
        }
        v.age += 1;
        let [mut a, mut b] = if v.sound >= SAMPLE_BASE {
            let buf = samples.get((v.sound - SAMPLE_BASE) as usize).and_then(Option::as_ref);
            match buf.and_then(|buf| Instrument::sample(buf, v)) {
                Some(x) => x,
                None => {
                    v.stage = Stage::Off;
                    return [0.0; 2];
                }
            }
        } else {
            match Instrument::source(bank, v, rate) {
                Some(x) => [x, x],
                None => return [0.0; 2],
            }
        };
        if v.coarse > 1.0 {
            if v.coarse_left <= 0.0 {
                v.held = [a, b];
                v.coarse_left += v.coarse;
            }
            v.coarse_left -= 1.0;
            [a, b] = v.held;
        }
        a = v.lp.run(a, false);
        a = v.hp.run(a, true);
        if v.stereo {
            b = v.lp2.run(b, false);
            b = v.hp2.run(b, true);
        } else {
            b = a;
        }
        let shape = |x: f32| {
            let mut x = x;
            if v.shape > 0.0 {
                let k = 2.0 * v.shape / (1.0 - v.shape);
                x = (1.0 + k) * x / (1.0 + k * x.abs());
            }
            if v.crush >= 1.0 {
                let steps = 2f32.powf(v.crush - 1.0);
                x = (x * steps).round() / steps;
            }
            x
        };
        let g = v.env * v.level;
        if v.stereo {
            [shape(a) * g, shape(b) * g]
        } else {
            let a = shape(a) * g;
            [a, a]
        }
    }

    /// A sample voice's next frame from its buffer (`None`: it ended).
    #[inline]
    fn sample(b: &SampleBuf, v: &mut Voice) -> Option<[f32; 2]> {
        let fwd = v.step >= 0.0;
        // The end of the part: again (`loop`), or done.
        if (fwd && v.pos >= v.seg_end) || (!fwd && v.pos < v.seg_begin) {
            if v.loops > 1.0 {
                v.loops -= 1.0;
                v.pos = if fwd { v.seg_begin + (v.pos - v.seg_end) } else { v.seg_end - (v.seg_begin - v.pos) };
                v.pos = v.pos.clamp(v.seg_begin, (v.seg_end - 1e-6).max(v.seg_begin));
            } else {
                return None;
            }
        }
        if v.step.abs() < 1e-6 {
            return None;
        }
        let [l, r] = read(b, v.pos);
        // Fades at the edges: in over the first ms, out over the last.
        let left = if v.loops <= 1.0 {
            ((if fwd { v.seg_end - v.pos } else { v.pos - v.seg_begin + 1.0 }) / v.step.abs()) as f32
        } else {
            f32::MAX
        };
        let g = (v.age as f32 / v.fade).min(left / v.fade).clamp(0.0, 1.0);
        v.pos += v.step;
        v.step += v.dstep;
        Some([l * g, r * g])
    }

    /// A drum's or synth's next value (mono), advancing it (`None`: silent
    /// this frame, the voice ended).
    #[inline]
    fn source(bank: &[Vec<f32>; 8], v: &mut Voice, rate: f32) -> Option<f32> {
        let dt_s = 1.0 / rate;
        let x = if v.sound < SYNTH_BASE {
            let b = &bank[(v.sound as usize).min(7)];
            let i = v.pos as usize;
            if i + 1 >= b.len() {
                v.stage = Stage::Off;
                return None;
            }
            let s = b[i] + (b[i + 1] - b[i]) * (v.pos - i as f64) as f32;
            v.pos += v.step;
            s
        } else {
            let f = v.freq;
            let dt = f / rate as f64;
            let t = v.age as f32 * dt_s;
            let adv = |p: &mut f64, by: f64| *p += by;
            match v.sound - SYNTH_BASE {
                // piano: partials that fade faster the higher they are
                0 => {
                    let mut s = 0.0;
                    for (k, amp) in [(1.0, 1.0), (2.0, 0.45), (3.0, 0.22), (4.0, 0.12)] {
                        let decay = (-t * (0.6 + 1.4 * k as f32)).exp();
                        s += sin01(v.phase[0] * k * (1.0 + 0.0004 * k * k)) * amp * decay;
                    }
                    adv(&mut v.phase[0], dt);
                    s * 0.6
                }
                // saw: three, detuned
                1 | 7 | 8 => {
                    let det: [f64; 3] = match v.sound - SYNTH_BASE {
                        7 => [1.0, 1.0173, 0.9829],
                        8 => [1.0, 1.006, 0.994],
                        _ => [1.0, 1.007, 0.993],
                    };
                    let mut s = 0.0;
                    for k in 0..3 {
                        s += saw(v.phase[k], dt * det[k]);
                        adv(&mut v.phase[k], dt * det[k]);
                    }
                    s * 0.33
                }
                2 => {
                    let s = square(v.phase[0], dt, 0.5);
                    adv(&mut v.phase[0], dt);
                    s * 0.45
                }
                3 => {
                    let s = sin01(v.phase[0]);
                    adv(&mut v.phase[0], dt);
                    s * 0.8
                }
                // fm: a modulator at twice the pitch, its depth decaying
                4 => {
                    let index = 0.6 + 3.0 * (-t * 6.0).exp();
                    let m = sin01(v.phase[1]) as f64 * index as f64;
                    let s = sin01(v.phase[0] + m / std::f64::consts::TAU);
                    adv(&mut v.phase[0], dt);
                    adv(&mut v.phase[1], dt * 2.0);
                    s * 0.7
                }
                // organ: drawbars 16', 8', 4', 2 2/3', 2'
                5 => {
                    let mut s = 0.0;
                    for (k, amp) in [(0.5, 0.5), (1.0, 1.0), (2.0, 0.6), (3.0, 0.4), (4.0, 0.3)] {
                        s += sin01(v.phase[0] * k) * amp;
                    }
                    adv(&mut v.phase[0], dt);
                    s * 0.3
                }
                // pluck: bright, its filter closing fast
                _ => {
                    let s = saw(v.phase[0], dt) * 0.6 + square(v.phase[1], dt, 0.3) * 0.4;
                    adv(&mut v.phase[0], dt);
                    adv(&mut v.phase[1], dt);
                    if v.age % 32 == 1 {
                        let sweep = 300.0 + 7000.0 * (-t * 9.0).exp();
                        let cutoff = if v.cutoff > 0.0 { v.cutoff.min(sweep) } else { sweep };
                        v.lp.set(cutoff, v.res.max(0.15), rate);
                    }
                    s * 0.7
                }
            }
        };
        Some(x)
    }

    /// One block starting at graph sample `at`: the queue's events due in it
    /// start on their frame; the voices and the shared effects are summed
    /// into `out_l`/`out_r`.
    pub fn render(&mut self, shared: &Shared, rate: u32, at: i64, out_l: &mut [f32], out_r: &mut [f32]) {
        let frames = out_l.len().min(out_r.len());
        let rate_f = rate.max(1) as f32;
        if rate_f != self.rate {
            self.rate = rate_f;
            self.room.tune(rate_f);
        }
        // Samples installed or let go by the control thread.
        for _ in 0..LOADS {
            let Some(load) = shared.bank.loads.pop() else { break };
            let (slot, buf) = match load {
                Load::Put(slot, buf) => (slot as usize, Some(buf)),
                Load::Clear(slot) => (slot as usize, None),
            };
            let Some(cell) = self.samples.get_mut(slot) else {
                if let Some(buf) = buf {
                    give_back(shared, buf);
                }
                continue;
            };
            if let Some(old) = std::mem::replace(cell, buf) {
                give_back(shared, old);
            }
        }
        let release = shared.release.load(Ordering::Acquire);
        if release != self.released {
            self.released = release;
            self.release_all();
        }
        self.due_n = 0;
        let end = at + frames as i64;
        let (due, n) = (&mut self.due, &mut self.due_n);
        let mut dropped = 0;
        // SAFETY: the engine callback is the instrument queue's only consumer.
        unsafe {
            shared.queue.drain_before(end, |e| {
                if e.kind != RtKind::Play {
                    return;
                }
                if *n < due.len() {
                    due[*n] = e;
                    *n += 1;
                } else {
                    dropped += 1;
                }
            })
        };
        if dropped > 0 {
            shared.dropped.fetch_add(dropped, Ordering::Relaxed);
        }
        let any = self.voices.iter().any(Voice::sounding);
        if self.due_n == 0 && !any && self.tail == 0 {
            shared.active.store(0, Ordering::Relaxed);
            return;
        }
        let mut next = 0;
        for f in 0..frames {
            while next < self.due_n && self.due[next].sample - at <= f as i64 {
                let e = self.due[next];
                self.start(&e, shared);
                next += 1;
            }
            let (mut l, mut r) = (0.0f32, 0.0f32);
            let (mut dl, mut dr, mut rl, mut rr) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            let mut sounding = false;
            for v in self.voices.iter_mut() {
                if !v.sounding() {
                    continue;
                }
                sounding = true;
                let [xa, xb] = Instrument::voice(&self.bank, &self.samples, v, self.rate);
                let (a, b) = (xa * v.gl, xb * v.gr);
                l += a;
                r += b;
                if v.delay > 0.0 {
                    dl += a * v.delay;
                    dr += b * v.delay;
                }
                if v.room > 0.0 {
                    rl += a * v.room;
                    rr += b * v.room;
                }
            }
            if self.delay_on {
                let y = self.delay.run([dl, dr]);
                l += y[0];
                r += y[1];
            }
            if self.room_on {
                let y = self.room.run([rl, rr]);
                l += y[0];
                r += y[1];
            }
            if sounding {
                // The shared effects ring on for a few seconds.
                self.tail = (self.rate * 6.0) as u64;
            } else if self.tail > 0 {
                self.tail -= 1;
                if self.tail == 0 {
                    self.delay_on = false;
                    self.room_on = false;
                }
            }
            out_l[f] += saturate(l);
            out_r[f] += saturate(r);
        }
        let active = self.voices.iter().filter(|v| v.sounding()).count() as u32;
        shared.active.store(active, Ordering::Relaxed);
    }

    /// Every voice let go (the pattern stopped): a short release each.
    pub fn release_all(&mut self) {
        for v in self.voices.iter_mut() {
            if v.sounding() {
                v.release = v.release.min(0.05);
                v.stage = Stage::Release;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn play(sound: u32, params: RtParams, at: i64) -> RtEvent {
        RtEvent {
            sample: at,
            kind: RtKind::Play,
            index: sound,
            value: 1.0,
            epoch: 0,
            params,
        }
    }

    fn run(events: &[RtEvent], frames: usize, block: usize) -> (Vec<f32>, Vec<f32>) {
        let shared = Shared::default();
        for e in events {
            assert!(unsafe { shared.queue.push(*e) });
        }
        let mut inst = Instrument::new();
        let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
        let mut at = 0;
        while at < frames {
            let n = block.min(frames - at);
            inst.render(&shared, 48_000, at as i64, &mut l[at..at + n], &mut r[at..at + n]);
            at += n;
        }
        (l, r)
    }

    #[test]
    fn every_sound_starts_on_its_sample_and_stays_finite() {
        for sound in (0..8).chain(SYNTH_BASE..SYNTH_BASE + 9) {
            let p = RtParams {
                sustain: 0.1,
                ..RtParams::default()
            };
            let (l, r) = run(&[play(sound, p, 1000)], 48_000, 256);
            assert!(l[..1000].iter().all(|x| *x == 0.0), "{sound}: early");
            let first = l.iter().position(|x| x.abs() > 1e-6).unwrap_or(usize::MAX);
            assert!((1000..1010).contains(&first), "{sound}: starts at {first}");
            assert!(l.iter().chain(&r).all(|x| x.is_finite() && x.abs() < 4.0), "{sound}");
            let peak = l.iter().fold(0.0f32, |a, x| a.max(x.abs()));
            assert!(peak > 0.01, "{sound}: silent ({peak})");
            // A held note lets go and fades: quiet a second later.
            let end = l[40_000..].iter().fold(0.0f32, |a, x| a.max(x.abs()));
            assert!(end < 0.01, "{sound}: rings on ({end})");
        }
    }

    #[test]
    fn a_dense_chord_keeps_its_shape() {
        let events: Vec<RtEvent> = (0..12)
            .map(|i| {
                play(
                    SYNTH_BASE + 8,
                    RtParams { note: 55.0 + i as f32 * 3.0, sustain: 1.0, ..RtParams::default() },
                    0,
                )
            })
            .collect();
        let (l, r) = run(&events, 24_000, 256);
        let peak = l.iter().chain(&r).fold(0.0f32, |a, x| a.max(x.abs()));
        assert!(peak > 0.05, "the chord is inaudible ({peak})");
        assert!(peak < 1.0, "the chord clips ({peak})");
    }

    #[test]
    fn the_sum_saturates_gently() {
        assert_eq!(saturate(0.0), 0.0);
        assert!(saturate(1.0) < 1.0);
        assert!(saturate(4.0) < saturate(2.0));
        assert!(saturate(4.0) < 1.0);
    }

    #[test]
    fn timing_is_independent_of_block_size() {
        let events = [
            play(0, RtParams::default(), 300),
            play(SYNTH_BASE + 1, RtParams { sustain: 0.05, ..RtParams::default() }, 777),
        ];
        let a = run(&events, 8_000, 64);
        let b = run(&events, 8_000, 1000);
        for (x, y) in a.0.iter().zip(&b.0) {
            assert!((x - y).abs() < 1e-6);
        }
    }

    #[test]
    fn pitch_follows_the_note() {
        // A sine at A4 (69): 440 zero crossings up per second.
        let p = RtParams {
            note: 69.0,
            sustain: 1.0,
            ..RtParams::default()
        };
        let (l, _) = run(&[play(SYNTH_BASE + 3, p, 0)], 48_000, 512);
        let ups = l[4_800..43_200].windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        assert!((350..355).contains(&ups), "{ups} in 0.8 s");
    }

    #[test]
    fn pan_and_sends() {
        let left = RtParams {
            pan: -1.0,
            ..RtParams::default()
        };
        let (l, r) = run(&[play(0, left, 0)], 4_800, 256);
        let (el, er) = (l.iter().map(|x| x * x).sum::<f32>(), r.iter().map(|x| x * x).sum::<f32>());
        assert!(er < el * 1e-6, "hard left: {el} {er}");
        // A delay send repeats the hit after the delay time.
        let echo = RtParams {
            delay: 1.0,
            delaytime: 0.25,
            delayfeedback: 0.0,
            ..RtParams::default()
        };
        let (l, _) = run(&[play(6, echo, 0)], 24_000, 256);
        let late = l[12_000..13_000].iter().fold(0.0f32, |a, x| a.max(x.abs()));
        assert!(late > 0.01, "no echo at 0.25 s ({late})");
    }

    /// A ramp 0 … 1 over `frames` (mono), or + and − ramps (stereo).
    fn ramp(frames: usize, channels: usize) -> SampleBuf {
        let mut data = Vec::with_capacity(frames * channels);
        for f in 0..frames {
            let x = f as f32 / frames as f32;
            data.push(x);
            if channels == 2 {
                data.push(-x);
            }
        }
        SampleBuf { data: data.into_boxed_slice(), channels, frames, rate: 48_000 }
    }

    fn sampled(bufs: Vec<(u32, SampleBuf)>, events: &[RtEvent], frames: usize) -> (Vec<f32>, Vec<f32>, Shared) {
        let shared = Shared::default();
        for (slot, b) in bufs {
            assert!(shared.bank.loads.push(Load::Put(slot, b)).is_ok());
        }
        for e in events {
            assert!(unsafe { shared.queue.push(*e) });
        }
        let mut inst = Instrument::new();
        let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
        let mut at = 0;
        while at < frames {
            let n = 256.min(frames - at);
            inst.render(&shared, 48_000, at as i64, &mut l[at..at + n], &mut r[at..at + n]);
            at += n;
        }
        (l, r, shared)
    }

    fn sample(slot: u32, at: i64, f: impl FnOnce(&mut RtParams)) -> RtEvent {
        let mut p = RtParams::default();
        f(&mut p);
        play(SAMPLE_BASE + slot, p, at)
    }

    #[test]
    fn samples_play_their_part_either_way_and_end() {
        let (l, _, _) = sampled(vec![(0, ramp(4800, 1))], &[sample(0, 100, |_| {})], 12_000);
        assert!(l[..100].iter().all(|x| *x == 0.0));
        assert!(l[2000] > l[1000] && l[4000] > l[3000], "forwards: rising");
        assert!(l[5000..].iter().all(|x| *x == 0.0), "ends with the file");
        // Backwards: falling, from the end.
        let (l, _, _) = sampled(vec![(0, ramp(4800, 1))], &[sample(0, 0, |p| p.speed = -1.0)], 6000);
        assert!(l[1000] > l[2000] && l[3000] > l[4000], "backwards: falling");
        assert!(l[4900..].iter().all(|x| *x == 0.0));
        // begin 0.5: the second half, so half as long, starting halfway up.
        let (l, _, _) = sampled(vec![(0, ramp(4800, 1))], &[sample(0, 0, |p| p.begin = 0.5)], 6000);
        let (full, _, _) = sampled(vec![(0, ramp(4800, 1))], &[sample(0, 0, |_| {})], 6000);
        assert!((l[500] - full[2900]).abs() < 1e-4, "{} {}", l[500], full[2900]);
        assert!(l[2500..].iter().all(|x| *x == 0.0));
        // Twice as fast: half as long.
        let (l, _, _) = sampled(vec![(0, ramp(4800, 1))], &[sample(0, 0, |p| p.speed = 2.0)], 6000);
        assert!(l[2300] != 0.0 && l[2500..].iter().all(|x| *x == 0.0));
        // loop 3: three times as long.
        let (l, _, _) = sampled(vec![(0, ramp(4800, 1))], &[sample(0, 0, |p| p.loops = 3.0)], 16_000);
        assert!(l[14_000] != 0.0 && l[14_500..].iter().all(|x| *x == 0.0));
        assert!(l.iter().all(|x| x.is_finite() && x.abs() < 1.0));
    }

    #[test]
    fn stereo_samples_cut_groups_and_missing_slots() {
        let (l, r, _) = sampled(vec![(7, ramp(4800, 2))], &[sample(7, 0, |_| {})], 4800);
        assert!(l[3000] > 0.05 && r[3000] < -0.05, "{} {}", l[3000], r[3000]);
        // A cut group: the second hit ends the first.
        let long = ramp(48_000, 1);
        let events = [sample(1, 0, |p| p.cut = 1), sample(2, 4800, |p| p.cut = 1)];
        let (_, _, shared) = sampled(vec![(1, long), (2, ramp(48_000, 1))], &events, 9600);
        assert_eq!(shared.active.load(Ordering::Relaxed), 1);
        let events = [sample(1, 0, |_| {}), sample(2, 4800, |_| {})];
        let (_, _, shared) = sampled(vec![(1, ramp(48_000, 1)), (2, ramp(48_000, 1))], &events, 9600);
        assert_eq!(shared.active.load(Ordering::Relaxed), 2, "no group: both ring");
        // An empty slot: nothing, counted.
        let (l, _, shared) = sampled(vec![], &[sample(9, 0, |_| {})], 1000);
        assert!(l.iter().all(|x| *x == 0.0));
        assert_eq!(shared.missing.load(Ordering::Relaxed), 1);
        // A slot replaced: the old buffer comes back to be freed elsewhere.
        let shared = Shared::default();
        let mut inst = Instrument::new();
        let (mut l, mut r) = (vec![0.0; 64], vec![0.0; 64]);
        assert!(shared.bank.loads.push(Load::Put(3, ramp(10, 1))).is_ok());
        inst.render(&shared, 48_000, 0, &mut l, &mut r);
        assert!(shared.bank.loads.push(Load::Put(3, ramp(20, 1))).is_ok());
        assert!(shared.bank.loads.push(Load::Clear(3)).is_ok());
        inst.render(&shared, 48_000, 64, &mut l, &mut r);
        let mut back = 0;
        while shared.bank.back.pop().is_some() {
            back += 1;
        }
        assert_eq!(back, 2);
    }

    #[test]
    fn a_full_table_steals_and_counts() {
        let events: Vec<RtEvent> = (0..300).map(|i| play(0, RtParams::default(), i)).collect();
        let shared = Shared::default();
        for e in &events {
            unsafe { shared.queue.push(*e) };
        }
        let mut inst = Instrument::new();
        let (mut l, mut r) = (vec![0.0; 512], vec![0.0; 512]);
        inst.render(&shared, 48_000, 0, &mut l, &mut r);
        assert_eq!(shared.dropped.load(Ordering::Relaxed), 300 - DUE as u64);
        assert_eq!(shared.active.load(Ordering::Relaxed), VOICES as u32);
        assert!(l.iter().all(|x| x.is_finite()));
    }
}
