//! Prepared bundle transport and RT stem DSP. No model runtime or cache I/O.
use super::{
    pcm::Pcm,
    transport::{CHUNK, Tag, Transport},
};
use patch_core::stems::{CANONICAL, StemValue};
use std::sync::{
    Arc, Mutex, RwLock,
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};
pub const CAPACITY: usize = 8;
pub const STRIDE: usize = 2 * (CAPACITY + 1);
/// Fixed per-stem automation horizon. Control code may enqueue this many
/// events; the mixer consumes them without allocation or locking.
pub const MAX_RAMPS_PER_GAIN_CONTROL: usize = 16;
pub struct Audio {
    pub source_path: String,
    pub sources: Vec<Arc<Pcm>>,
    pub ids: Vec<String>,
    pub set: patch_core::stems::StemSet,
    pub lease: patch_stems::cache::Lease,
    pub render: Option<Renderer>,
}
impl std::fmt::Debug for Audio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StemAudio")
            .field("id", &self.set.id)
            .finish()
    }
}
pub struct Renderer {
    stretch: super::stretch::MultiStretcher,
    feed: f64,
    discard: usize,
    primed: bool,
    /// Times the stretcher was (re)started at the playhead.
    primes: u32,
    sinc: super::sinc::Sinc,
    /// What it was built for: (rate, channels).
    built: (u32, usize),
}
impl Renderer {
    pub fn new(rate: u32, channels: usize) -> Self {
        Self {
            stretch: super::stretch::MultiStretcher::new(rate, channels),
            feed: 0.0,
            discard: 0,
            primed: false,
            primes: 0,
            sinc: super::sinc::Sinc::new(),
            built: (rate, channels),
        }
    }

    /// The transport jumped: the stretcher starts over at the playhead on
    /// the next chunk (`render` resets and primes it), keeping its buffers.
    pub fn restart(&mut self) {
        self.primed = false;
    }
}
fn at(t: &Transport, audio: &Audio, position: i64, c: usize) -> f32 {
    let position = t.map(position);
    if position < 0 {
        return 0.0;
    }
    if c < 2 {
        t.pcm.as_ref().map_or(0.0, |p| p.get(position as usize, c))
    } else {
        audio.sources[(c - 2) / 2].get(position as usize, (c - 2) % 2)
    }
}

/// Fill `channels` planar buffers from `from`, reusing their allocation.
fn write_planes(
    planes: &mut [Vec<f32>],
    channels: usize,
    n: usize,
    from: i64,
    mut sample: impl FnMut(i64, usize) -> f32,
) {
    for c in 0..channels {
        if planes[c].len() != n {
            planes[c].resize(n, 0.0);
        }
        for (i, sample_out) in planes[c].iter_mut().enumerate() {
            *sample_out = sample(from + i as i64, c);
        }
    }
}

pub fn render(t: &mut Transport, emit: &mut impl FnMut(Tag, &[f32])) {
    let mut audio = t.stems.take().unwrap();
    let speed = t.speed();
    let channels = 2 * (audio.sources.len() + 1);
    let mut renderer = match audio.render.take() {
        Some(r) if r.built == (t.rate, channels) => r,
        _ => Renderer::new(t.rate, channels),
    };
    let mut out = std::mem::take(&mut t.bundle);
    let mut planes = std::mem::take(&mut t.planes);
    if out.len() < CHUNK * STRIDE {
        out.resize(CHUNK * STRIDE, 0.0);
    }
    out[..CHUNK * STRIDE].fill(0.0);
    let stretch = t.playing
        && speed > 0.0
        && !t.reverse
        && t.scratching.is_none()
        // Keylock stretches only an audible pitch change, as on the plain
        // path: an AI fine match of a few hundredths of a percent plays
        // through the resampler. Stretching every stem for it cost a third
        // of a core per deck (2026-11-12: −0.0001 % → 31 % CPU).
        && (t.keylock && t.pitch_would_show_while(renderer.primed) || t.key != 0);
    if stretch {
        renderer.stretch.set(speed, t.pitch());
        // How far the stretcher's input runs ahead of the playhead. In a
        // loop it wraps to the start first: that is still ahead, not 8192
        // frames behind (re-priming every chunk until the playhead wrapped
        // too starved the worker: underruns at each wrap, 2026-10-26).
        let mut ahead = renderer.feed - t.pos;
        if let (true, Some((a, b)), true) = (t.loop_on, t.looping, ahead < 0.0) {
            ahead += b.round() - a.round();
        }
        if !renderer.primed || !(-8192.0..=16384.0).contains(&ahead) {
            renderer.primes += 1;
            renderer.stretch.reset();
            renderer.stretch.set(speed, t.pitch());
            let n = renderer.stretch.start_pad();
            let from = t.pos - n as f64;
            write_planes(
                &mut planes,
                channels,
                n,
                from.floor() as i64,
                |position, c| at(t, &audio, position, c),
            );
            renderer.stretch.process(&planes[..channels]);
            renderer.discard = renderer.stretch.start_delay();
            renderer.feed = t.pos;
            renderer.primed = true;
        }
        for _ in 0..64 {
            if renderer.stretch.available() >= CHUNK + renderer.discard {
                break;
            }
            let n = renderer.stretch.required().clamp(64, 2048);
            let from = renderer.feed.floor() as i64;
            write_planes(&mut planes, channels, n, from, |position, c| {
                at(t, &audio, position, c)
            });
            renderer.stretch.process(&planes[..channels]);
            renderer.feed = t.wrap(renderer.feed + n as f64);
        }
        while renderer.discard > 0 {
            let n = renderer.discard.min(4096);
            write_planes(&mut planes, channels, n, 0, |_, _| 0.0);
            let got = renderer.stretch.retrieve(&mut planes[..channels]);
            if got == 0 {
                break;
            }
            renderer.discard -= got;
        }
        write_planes(&mut planes, channels, CHUNK, 0, |_, _| 0.0);
        let got = renderer.stretch.retrieve(&mut planes[..channels]);
        for i in 0..got {
            for c in 0..channels {
                out[i * STRIDE + c] = planes[c][i];
            }
        }
    } else {
        renderer.primed = false;
        for i in 0..CHUNK {
            if !t.playing {
                break;
            }
            let pos = t.wrap(t.pos + i as f64 * speed);
            for c in 0..channels {
                out[i * STRIDE + c] = if (speed - 1.0).abs() < 1e-7 {
                    at(t, &audio, pos.floor() as i64, c)
                } else {
                    renderer.sinc.at(pos, speed, |p| at(t, &audio, p, c))
                };
            }
        }
    }
    let mut done = 0;
    while done < CHUNK {
        let until = if !t.playing || speed == 0.0 {
            CHUNK
        } else if speed > 0.0 {
            ((t.limit() - t.pos) / speed).ceil().max(0.0) as usize
        } else {
            let low = if t.loop_on {
                t.looping.map_or(0.0, |p| p.0)
            } else {
                0.0
            };
            ((t.pos - low) / (-speed)).floor().max(0.0) as usize + 1
        };
        let n = until.min(CHUNK - done);
        if n == 0 {
            if t.loop_on {
                t.pos = t.wrap(t.pos);
                continue;
            }
            let silence = (CHUNK - done) * STRIDE;
            out[..silence].fill(0.0);
            emit(
                Tag {
                    generation: t.generation,
                    frames: (CHUNK - done) as u32,
                    src: t.pos,
                    step: 0.0,
                    bundle: true,
                },
                &out[..silence],
            );
            break;
        }
        emit(
            Tag {
                generation: t.generation,
                frames: n as u32,
                src: t.pos,
                step: if t.playing { speed } else { 0.0 },
                bundle: true,
            },
            &out[done * STRIDE..(done + n) * STRIDE],
        );
        if t.playing {
            t.pos = t.wrap(t.pos + n as f64 * speed);
        }
        done += n;
    }
    t.shadow += if t.playing {
        (1.0 + t.tempo) * (1.0 + t.bend) * CHUNK as f64
    } else {
        0.0
    };
    t.bundle = out;
    t.planes = planes;
    audio.render = Some(renderer);
    t.stems = Some(audio);
    if t.playing && !t.loop_on && (t.pos >= t.len() || t.pos < 0.0) {
        t.playing = false;
        t.end = true;
    }
}
pub struct Slot {
    gain: AtomicU64,
    mute: AtomicBool,
    solo: AtomicBool,
    epoch: AtomicU64,
    human: AtomicBool,
    ramps: RampQueue,
    pub meter: AtomicU32,
    applied_sample: AtomicU64,
    applied_revision: AtomicU64,
}

struct RampEvent {
    at: AtomicU64,
    duration: AtomicU64,
    from: AtomicU32,
    to: AtomicU32,
    epoch: AtomicU64,
}

impl RampEvent {
    fn new() -> Self {
        Self {
            at: AtomicU64::new(0),
            duration: AtomicU64::new(0),
            from: AtomicU32::new(0),
            to: AtomicU32::new(0),
            epoch: AtomicU64::new(0),
        }
    }
}

struct RampQueue {
    events: [RampEvent; MAX_RAMPS_PER_GAIN_CONTROL],
    head: AtomicU64,
    tail: AtomicU64,
    planned_end: AtomicU64,
    planned_gain: AtomicU32,
}

impl RampQueue {
    fn new() -> Self {
        Self {
            events: std::array::from_fn(|_| RampEvent::new()),
            head: AtomicU64::new(0),
            tail: AtomicU64::new(0),
            planned_end: AtomicU64::new(0),
            planned_gain: AtomicU32::new(1.0f32.to_bits()),
        }
    }

    fn cancel(&self, gain: f32) {
        let tail = self.tail.load(Ordering::Acquire);
        self.head.store(tail, Ordering::Release);
        self.planned_end.store(0, Ordering::Release);
        self.planned_gain.store(gain.to_bits(), Ordering::Release);
    }

    fn push(&self, epoch: u64, at: u64, duration: u64, to: f32) -> Result<(), String> {
        let end = at
            .checked_add(duration)
            .ok_or("gain ramp exceeds engine sample clock")?;
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Relaxed);
        if tail.saturating_sub(head) >= MAX_RAMPS_PER_GAIN_CONTROL as u64 {
            return Err("gain automation queue is full".into());
        }
        if at < self.planned_end.load(Ordering::Acquire) {
            return Err("gain ramps must be scheduled in non-overlapping sample order".into());
        }
        let from = f32::from_bits(self.planned_gain.load(Ordering::Acquire));
        let event = &self.events[tail as usize % MAX_RAMPS_PER_GAIN_CONTROL];
        event.at.store(at, Ordering::Relaxed);
        event.duration.store(duration, Ordering::Relaxed);
        event.from.store(from.to_bits(), Ordering::Relaxed);
        event.to.store(to.to_bits(), Ordering::Relaxed);
        event.epoch.store(epoch, Ordering::Relaxed);
        self.planned_gain.store(to.to_bits(), Ordering::Relaxed);
        self.planned_end.store(end, Ordering::Relaxed);
        self.tail.store(tail + 1, Ordering::Release);
        Ok(())
    }
}
pub struct Controls {
    identities: RwLock<Vec<String>>,
    pub enabled: AtomicBool,
    pub ready: AtomicBool,
    pub slots: [Slot; CAPACITY],
    original_gain: AtomicU64,
    original_epoch: AtomicU64,
    original_ramps: RampQueue,
    writer: Mutex<()>,
}
impl Default for Controls {
    fn default() -> Self {
        Self {
            writer: Mutex::new(()),
            identities: RwLock::new(CANONICAL.iter().map(|s| s.to_string()).collect()),
            enabled: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            original_gain: AtomicU64::new(1.0f32.to_bits() as u64),
            original_epoch: AtomicU64::new(0),
            original_ramps: RampQueue::new(),
            slots: std::array::from_fn(|_| Slot {
                gain: AtomicU64::new(1.0f32.to_bits() as u64),
                mute: AtomicBool::new(false),
                solo: AtomicBool::new(false),
                epoch: AtomicU64::new(0),
                human: AtomicBool::new(false),
                ramps: RampQueue::new(),
                meter: AtomicU32::new(0),
                applied_sample: AtomicU64::new(0),
                applied_revision: AtomicU64::new(u64::MAX),
            }),
        }
    }
}
impl Controls {
    pub fn set_layout(&self, ids: &[String]) -> Result<(), String> {
        if ids.is_empty()
            || ids.len() > CAPACITY
            || ids.iter().any(|id| !patch_core::stems::valid_id(id))
        {
            return Err("Invalid stem layout".into());
        }
        if ids.iter().enumerate().any(|(i, id)| ids[..i].contains(id)) {
            return Err("Duplicate stem in layout".into());
        }
        let _writer = self.writer.lock().unwrap();
        let mut current = self.identities.write().unwrap();
        if *current != ids {
            self.cancel_locked();
            for slot in &self.slots {
                let _ = slot
                    .gain
                    .try_update(Ordering::AcqRel, Ordering::Acquire, |v| {
                        Some((v & 0xffff_ffff_0000_0000) | 1.0f32.to_bits() as u64)
                    });
                slot.mute.store(false, Ordering::Release);
                slot.solo.store(false, Ordering::Release);
                slot.human.store(false, Ordering::Release);
                slot.meter.store(0.0f32.to_bits(), Ordering::Release);
                slot.ramps.cancel(1.0);
            }
            *current = ids.to_vec();
            self.enabled.store(false, Ordering::Release);
        }
        for i in ids.len()..CAPACITY {
            let s = &self.slots[i];
            s.gain.store(
                (s.gain.load(Ordering::Acquire) & 0xffff_ffff_0000_0000) | 1.0f32.to_bits() as u64,
                Ordering::Release,
            );
            s.mute.store(false, Ordering::Release);
            s.solo.store(false, Ordering::Release);
            s.human.store(false, Ordering::Release);
            s.ramps.cancel(1.0);
        }
        Ok(())
    }
    pub fn index(&self, id: &str) -> Option<usize> {
        self.identities.read().unwrap().iter().position(|s| s == id)
    }
    pub fn set(&self, index: usize, value: StemValue, human: bool) -> Result<(), String> {
        let _writer = self.writer.lock().unwrap();
        value.validate()?;
        if index >= self.identities.read().unwrap().len() {
            return Err("Unavailable stem".into());
        }
        let s = self.slots.get(index).ok_or("Unavailable stem")?;
        if !self.ready.load(Ordering::Acquire) {
            return Err("Prepare and load stems first".into());
        }
        if !human && s.human.load(Ordering::Acquire) {
            return Err("This stem is controlled by the DJ; return it to Auto first".into());
        }
        s.epoch.fetch_add(1, Ordering::AcqRel);
        let _ = s
            .gain
            .try_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                Some(
                    ((old >> 32).wrapping_add(1) << 32)
                        | match value.gain() {
                            Some(g) => g.to_bits() as u64,
                            None => old & 0xffff_ffff,
                        },
                )
            });
        let gain = f32::from_bits(s.gain.load(Ordering::Acquire) as u32);
        s.ramps.cancel(value.gain().unwrap_or(gain));
        if human {
            s.human.store(true, Ordering::Release);
        }
        match value {
            StemValue::Gain(_) | StemValue::Knob(_) => {}
            StemValue::Mute(v) => s.mute.store(v, Ordering::Release),
            StemValue::Solo(v) => s.solo.store(v, Ordering::Release),
        }
        self.enabled.store(true, Ordering::Release);
        Ok(())
    }
    pub fn ramp(&self, index: usize, at: u64, duration: u64, to: f32) -> Result<(), String> {
        let _writer = self.writer.lock().unwrap();
        StemValue::Gain(to).validate()?;
        if index >= self.identities.read().unwrap().len() {
            return Err("Unavailable stem".into());
        }
        let s = self.slots.get(index).ok_or("Unavailable stem")?;
        if !self.ready.load(Ordering::Acquire) || s.human.load(Ordering::Acquire) {
            return Err("Stem is unavailable or human-owned".into());
        }
        s.ramps
            .push(s.epoch.load(Ordering::Acquire), at, duration, to)?;
        self.enabled.store(true, Ordering::Release);
        Ok(())
    }
    pub fn ramp_original(&self, at: u64, duration: u64, to: f32) -> Result<(), String> {
        let _writer = self.writer.lock().unwrap();
        StemValue::Gain(to).validate()?;
        self.original_ramps.push(
            self.original_epoch.load(Ordering::Acquire),
            at,
            duration,
            to,
        )
    }

    pub fn set_original_gain(&self, gain: f32) -> Result<(), String> {
        let _writer = self.writer.lock().unwrap();
        StemValue::Gain(gain).validate()?;
        self.original_epoch.fetch_add(1, Ordering::AcqRel);
        let _ = self
            .original_gain
            .try_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                Some(((old >> 32).wrapping_add(1) << 32) | gain.to_bits() as u64)
            });
        self.original_ramps.cancel(gain);
        Ok(())
    }

    pub fn cancel_original_automation(&self) {
        let _writer = self.writer.lock().unwrap();
        self.original_epoch.fetch_add(1, Ordering::AcqRel);
        self.original_gain.fetch_add(1u64 << 32, Ordering::AcqRel);
        self.original_ramps.cancel(f32::from_bits(
            self.original_gain.load(Ordering::Acquire) as u32
        ));
    }
    pub fn cancel(&self) {
        let _writer = self.writer.lock().unwrap();
        self.cancel_locked();
    }
    fn cancel_locked(&self) {
        for s in &self.slots {
            s.epoch.fetch_add(1, Ordering::AcqRel);
            s.gain.fetch_add(1u64 << 32, Ordering::AcqRel);
            s.ramps
                .cancel(f32::from_bits(s.gain.load(Ordering::Acquire) as u32));
        }
        self.original_epoch.fetch_add(1, Ordering::AcqRel);
        self.original_gain.fetch_add(1u64 << 32, Ordering::AcqRel);
        self.original_ramps.cancel(f32::from_bits(
            self.original_gain.load(Ordering::Acquire) as u32
        ));
    }
    pub fn reset(&self) {
        let _writer = self.writer.lock().unwrap();
        self.cancel_locked();
        self.enabled.store(false, Ordering::Release);
        for s in &self.slots {
            let _ = s.gain.try_update(Ordering::AcqRel, Ordering::Acquire, |v| {
                Some((v & 0xffff_ffff_0000_0000) | 1.0f32.to_bits() as u64)
            });
            s.ramps.cancel(1.0);
            s.mute.store(false, Ordering::Release);
            s.solo.store(false, Ordering::Release);
            s.human.store(false, Ordering::Release);
        }
        let _ = self
            .original_gain
            .try_update(Ordering::AcqRel, Ordering::Acquire, |v| {
                Some((v & 0xffff_ffff_0000_0000) | 1.0f32.to_bits() as u64)
            });
        self.original_ramps.cancel(1.0);
    }
    pub fn human_reset(&self) {
        self.reset();
        let _writer = self.writer.lock().unwrap();
        let len = self.identities.read().unwrap().len();
        for s in self.slots.iter().take(len) {
            s.human.store(true, Ordering::Release);
        }
    }
    pub fn allow_auto(&self) {
        let _writer = self.writer.lock().unwrap();
        let len = self.identities.read().unwrap().len();
        for s in self.slots.iter().take(len) {
            s.human.store(false, Ordering::Release);
        }
    }
    /// Gains and levels of the canonical stems (vocals, drums, bass,
    /// other) as they are now, for the shared-memory frame: no allocation.
    /// None until stems are attached.
    pub fn display(&self) -> Option<([f32; 4], [f32; 4])> {
        if !self.ready.load(Ordering::Acquire) {
            return None;
        }
        let ids = self.identities.read().unwrap();
        let (mut gain, mut level) = ([f32::NAN; 4], [0.0; 4]);
        for (c, id) in CANONICAL.iter().enumerate() {
            if let Some(i) = ids.iter().position(|x| x == id).filter(|i| *i < CAPACITY) {
                let s = &self.slots[i];
                gain[c] = if s.mute.load(Ordering::Acquire) {
                    0.0
                } else {
                    f32::from_bits(s.gain.load(Ordering::Acquire) as u32)
                };
                level[c] = f32::from_bits(s.meter.load(Ordering::Acquire));
            }
        }
        Some((gain, level))
    }
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "ready": self.ready.load(Ordering::Acquire),
            "enabled": self.enabled.load(Ordering::Acquire),
            "original_mix": {
                "gain": f32::from_bits(self.original_gain.load(Ordering::Acquire) as u32),
                "automation_queued": self.original_ramps.tail.load(Ordering::Acquire)
                    .saturating_sub(self.original_ramps.head.load(Ordering::Acquire)),
                "automation_end_sample": self.original_ramps.planned_end.load(Ordering::Acquire)
            },
            "controls": self.identities.read().unwrap().iter().enumerate().map(|(i, id)| {
                let s = &self.slots[i];
                let head = s.ramps.head.load(Ordering::Acquire);
                let tail = s.ramps.tail.load(Ordering::Acquire);
                let next = (head < tail).then(|| &s.ramps.events[head as usize % MAX_RAMPS_PER_GAIN_CONTROL]);
                let observed = s.gain.load(Ordering::Acquire);
                serde_json::json!({
                    "id": id,
                    "gain": f32::from_bits(observed as u32),
                    "mute": s.mute.load(Ordering::Acquire),
                    "solo": s.solo.load(Ordering::Acquire),
                    "human": s.human.load(Ordering::Acquire),
                    "scheduled_sample": next.map_or(0, |event| event.at.load(Ordering::Acquire)),
                    "ramp_frames": next.map_or(0, |event| event.duration.load(Ordering::Acquire)),
                    "target": next.map_or(f32::from_bits(observed as u32), |event| f32::from_bits(event.to.load(Ordering::Acquire))),
                    "automation_armed": head < tail,
                    "automation_queued": tail.saturating_sub(head),
                    "automation_end_sample": s.ramps.planned_end.load(Ordering::Acquire),
                    "epoch": observed >> 32,
                    "last_applied_sample": s.applied_sample.load(Ordering::Acquire),
                    "meter": f32::from_bits(s.meter.load(Ordering::Acquire))
                })
            }).collect::<Vec<_>>()
        })
    }
}

fn apply_ramps(queue: &RampQueue, epoch: &AtomicU64, packed_gain: &AtomicU64, sample: u64) -> f32 {
    let mut packed = packed_gain.load(Ordering::Acquire);
    let mut gain = f32::from_bits(packed as u32);
    let mut head = queue.head.load(Ordering::Relaxed);
    let tail = queue.tail.load(Ordering::Acquire);
    while head < tail {
        let event = &queue.events[head as usize % MAX_RAMPS_PER_GAIN_CONTROL];
        if event.epoch.load(Ordering::Acquire) != epoch.load(Ordering::Acquire) {
            head += 1;
            queue.head.store(head, Ordering::Release);
            continue;
        }
        let at = event.at.load(Ordering::Relaxed);
        if sample < at {
            break;
        }
        let duration = event.duration.load(Ordering::Relaxed);
        let fraction = if duration == 0 {
            1.0
        } else {
            (sample - at).min(duration) as f32 / duration as f32
        };
        let from = f32::from_bits(event.from.load(Ordering::Relaxed));
        let to = f32::from_bits(event.to.load(Ordering::Relaxed));
        gain = from + (to - from) * fraction;
        packed = packed_gain.load(Ordering::Acquire);
        if let Err(current) = packed_gain.compare_exchange(
            packed,
            (packed & 0xffff_ffff_0000_0000) | gain.to_bits() as u64,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            gain = f32::from_bits(current as u32);
        }
        if fraction >= 1.0 {
            head += 1;
            queue.head.store(head, Ordering::Release);
            continue;
        }
        break;
    }
    gain
}

pub struct Mix {
    gains: [f32; CAPACITY],
    blend: f32,
}
impl Mix {
    /// How loud stem `i` plays now, relative to the original track (1.0
    /// while the deck plays the original).
    pub fn level(&self, i: usize) -> f32 {
        self.gains[i] * self.blend + (1.0 - self.blend)
    }
}

impl Default for Mix {
    fn default() -> Self {
        Self {
            gains: [1.0; CAPACITY],
            blend: 0.0,
        }
    }
}
impl Mix {
    pub fn frame(&mut self, p: &Controls, a: &[f32], bundle: bool, sample: u64) -> [f32; 2] {
        let f = self.frame_with_send(p, a, bundle, sample, None);
        [f[0], f[1]]
    }
    pub fn frame_with_send(
        &mut self,
        p: &Controls,
        a: &[f32],
        bundle: bool,
        sample: u64,
        send: Option<usize>,
    ) -> [f32; 4] {
        let original_gain = if !bundle {
            apply_ramps(
                &p.original_ramps,
                &p.original_epoch,
                &p.original_gain,
                sample,
            )
        } else {
            1.0
        };
        if !bundle && self.blend == 0.0 {
            return [a[0] * original_gain, a[1] * original_gain, 0.0, 0.0];
        }
        let enabled = bundle && p.enabled.load(Ordering::Acquire);
        let solo = p.slots.iter().any(|s| s.solo.load(Ordering::Acquire));
        // Untouched stems are the original track: play the original then,
        // free of separation artifacts, and glide to the stem sum (aligned
        // to it sample for sample) only once a stem is changed.
        let untouched = !solo
            && p.slots.iter().all(|s| {
                !s.mute.load(Ordering::Acquire)
                    && (f32::from_bits(s.gain.load(Ordering::Acquire) as u32) - 1.0).abs() < 1e-3
                    && s.ramps.head.load(Ordering::Acquire) >= s.ramps.tail.load(Ordering::Acquire)
            });
        let stems_heard = enabled && !untouched;
        self.blend +=
            (if stems_heard { 1.0 } else { 0.0 } - self.blend).clamp(-1.0 / 64.0, 1.0 / 64.0);
        let mut sum = [0.0f32; 2];
        let mut tap = [0.0f32; 2];
        for (i, s) in p.slots.iter().enumerate() {
            let gain = apply_ramps(&s.ramps, &s.epoch, &s.gain, sample);
            let observed = s.gain.load(Ordering::Acquire);
            if s.ramps.head.load(Ordering::Acquire) < s.ramps.tail.load(Ordering::Acquire) {
                s.applied_sample.store(sample, Ordering::Release);
                s.applied_revision.store(observed >> 32, Ordering::Release);
            }
            let target =
                if s.mute.load(Ordering::Acquire) || solo && !s.solo.load(Ordering::Acquire) {
                    0.0
                } else {
                    gain
                };
            self.gains[i] += (target - self.gains[i]).clamp(-1.0 / 64.0, 1.0 / 64.0);
            let l = a[(i + 1) * 2] * self.gains[i];
            let r = a[(i + 1) * 2 + 1] * self.gains[i];
            sum[0] += l;
            sum[1] += r;
            // A send takes the stem itself, whether or not the deck plays
            // its stems or its original.
            if send == Some(i) && enabled {
                tap[0] = l;
                tap[1] = r;
            }
            s.meter.store(
                (l.abs()
                    .max(r.abs())
                    .max(f32::from_bits(s.meter.load(Ordering::Relaxed)) * 0.99993))
                .to_bits(),
                Ordering::Relaxed,
            );
        }
        let dry = [
            a[0] * (1.0 - self.blend) + sum[0] * self.blend,
            a[1] * (1.0 - self.blend) + sum[1] * self.blend,
        ];
        [
            dry[0] * original_gain,
            dry[1] * original_gain,
            tap[0],
            tap[1],
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::super::transport::{Cmd, Grid};
    use super::*;
    fn audio(original: &Arc<Pcm>) -> (Audio, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "patch-stem-sync-{}-{}",
            std::process::id(),
            crate::now_ns()
        ));
        let cache =
            patch_stems::cache::Cache::new(root.join("audio"), root.join("meta"), 1000).unwrap();
        let id = "b".repeat(64);
        let staging = root.join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        let mut set = patch_core::stems::StemSet {
            version: 1,
            id: id.clone(),
            source_hash: String::new(),
            track_id: String::new(),
            model_id: "fixture".into(),
            model_version: "1".into(),
            weights_digest: String::new(),
            profile: String::new(),
            pipeline_version: 1,
            backend: "fixture".into(),
            sample_rate: original.rate,
            frames: original.frames() as u64,
            duration: original.frames() as f64 / original.rate as f64,
            layout: CANONICAL.iter().map(|s| s.to_string()).collect(),
            stems: Default::default(),
            created: 0,
        };
        cache.publish(&set, &staging).unwrap();
        let lease = cache.acquire(&id).unwrap();
        set.stems = CANONICAL
            .iter()
            .map(|stem| {
                (
                    stem.to_string(),
                    patch_core::stems::StemArtifact {
                        chunks: vec![],
                        id: format!("{id}:{stem}"),
                        cache_ref: format!("fixture:{stem}"),
                        codec: "fixture".into(),
                        checksum: String::new(),
                        bytes: 0,
                        frames: original.frames() as u64,
                        start_frame: 0,
                        peak: 0.12,
                        activity_hz: 1,
                        activity: vec![0.12; 3],
                        low_activity: vec![],
                        waveform: vec![],
                    },
                )
            })
            .collect();
        (
            Audio {
                sources: vec![original.clone(); 4],
                ids: CANONICAL.iter().map(|s| s.to_string()).collect(),
                source_path: String::new(),
                set,
                lease,
                render: None,
            },
            root,
        )
    }
    /// A deck playing its stems sounds when its original would: the stem
    /// bundle's stretcher has the same output timing as the plain path.
    #[test]
    fn stems_play_in_time_with_the_original_under_tempo_and_keylock() {
        let rate = 48000;
        // A click every half second.
        let x: Vec<f32> = (0..rate * 8)
            .flat_map(|i| {
                let v = if i % (rate / 2) < 24 { 0.8 } else { 0.0 };
                [v, v]
            })
            .collect();
        let p = Pcm::from_samples(rate as u32, x);
        let clicks = |t: &mut Transport, bundle_stem: Option<usize>, tempo: f64| -> Vec<usize> {
            t.apply(Cmd::Keylock(true), None);
            t.apply(Cmd::Tempo(tempo), None);
            t.apply(Cmd::Play, None);
            let mut out: Vec<f32> = vec![];
            while out.len() < rate * 6 {
                t.render(&mut |tag, audio| {
                    if tag.bundle {
                        let c = bundle_stem.map_or(0, |s| (s + 1) * 2);
                        out.extend(audio.chunks_exact(STRIDE).map(|f| f[c]));
                    } else {
                        out.extend(audio.chunks_exact(2).map(|f| f[0]));
                    }
                });
            }
            // Onsets: rising through half the click level, after silence.
            (1..out.len())
                .filter(|&i| out[i].abs() > 0.3 && out[i - 1].abs() <= 0.3)
                .filter(|&i| out[i.saturating_sub(2000)..i].iter().all(|v| v.abs() < 0.3))
                .collect()
        };
        let lag_of = |plain_tempo: f64, stems_tempo: f64| {
            let mut plain = Transport::new(rate as u32);
            plain.apply(Cmd::Load(p.clone()), None);
            let a = clicks(&mut plain, None, plain_tempo);
            let (audio, root) = audio(&p);
            let mut stems = Transport::new(rate as u32);
            stems.apply(Cmd::Load(p.clone()), None);
            stems.apply(Cmd::AttachStems(audio.into()), None);
            let b = clicks(&mut stems, Some(1), stems_tempo);
            let _ = std::fs::remove_dir_all(root);
            assert!(a.len() >= 8 && b.len() >= 8, "{a:?} {b:?}");
            a.iter()
                .zip(&b)
                .map(|(x, y)| *y as i64 - *x as i64)
                .collect::<Vec<i64>>()
        };
        // Both stretched (a synced pair), and one stretched by a hair against
        // one at its own tempo (two tracks at the same BPM).
        for (pt, st) in [(0.03, 0.03), (0.0, 2e-6), (2e-6, 0.0)] {
            let lag = lag_of(pt, st);
            assert!(
                lag.iter().all(|l| l.abs() <= 48),
                "tempo {pt} vs stems at {st}: onsets (samples) {lag:?}"
            );
        }
    }

    /// A keylocked deck with stems loops without restarting its stretcher:
    /// its input wraps to the loop start before the playhead does, which is
    /// still ahead of it, not behind.
    #[test]
    fn a_keylocked_stem_loop_keeps_its_stretcher_across_wraps() {
        let rate = 48000usize;
        let x: Vec<f32> = (0..rate * 12).flat_map(|i| [(i as f32 * 0.01).sin() * 0.5; 2]).collect();
        let p = Pcm::from_samples(rate as u32, x);
        let (audio, root) = audio(&p);
        let mut t = Transport::new(rate as u32);
        t.apply(Cmd::Load(p.clone()), None);
        t.apply(Cmd::AttachStems(audio.into()), None);
        t.apply(Cmd::Grid(Some(super::super::transport::Grid { bpm: 120.0, offset: 0.0, downbeat: Some(0) })), None);
        t.apply(Cmd::Keylock(true), None);
        t.apply(Cmd::Tempo(0.024), None);
        t.apply(Cmd::Seek(1.0), None);
        t.apply(Cmd::Play, None);
        t.render(&mut |_, _| {});
        t.apply(Cmd::AutoLoop(2.0), None);
        let mut wraps = 0;
        let mut last = t.pos;
        // Two beats at 120 BPM: 1 s, wrapping about five times.
        for _ in 0..(rate * 5 / CHUNK) {
            t.render(&mut |_, _| {});
            if t.pos < last {
                wraps += 1;
            }
            last = t.pos;
        }
        let _ = std::fs::remove_dir_all(root);
        let primes = t.stems.as_ref().unwrap().render.as_ref().unwrap().primes;
        assert!(wraps >= 4, "looped {wraps} times");
        assert!(primes <= 1, "the stretcher restarted {primes} times over {wraps} wraps");
    }

    /// A jump on a keylocked stems deck re-primes the stretcher it has (no
    /// new ~3 MB stretcher on the render thread), and plays exactly what a
    /// fresh one would from there.
    #[test]
    fn a_jump_keeps_the_stem_stretcher_and_sounds_the_same() {
        let rate = 48000usize;
        let x: Vec<f32> = (0..rate * 8).flat_map(|i| [(i as f32 * 0.013).sin() * 0.5; 2]).collect();
        let p = Pcm::from_samples(rate as u32, x);
        let deck = || {
            let (audio, root) = audio(&p);
            let mut t = Transport::new(rate as u32);
            t.apply(Cmd::Load(p.clone()), None);
            t.apply(Cmd::AttachStems(audio.into()), None);
            t.apply(Cmd::Keylock(true), None);
            t.apply(Cmd::Tempo(0.03), None);
            (t, root)
        };
        let take = |t: &mut Transport| {
            let mut out = vec![];
            for _ in 0..8 {
                t.render(&mut |tag, a| {
                    if tag.bundle {
                        out.extend_from_slice(a);
                    }
                });
            }
            out
        };
        let (mut jumped, root_a) = deck();
        jumped.apply(Cmd::Play, None);
        let _ = take(&mut jumped);
        jumped.apply(Cmd::Seek(3.0), None);
        let after_jump = take(&mut jumped);
        let (mut fresh, root_b) = deck();
        fresh.apply(Cmd::Seek(3.0), None);
        fresh.apply(Cmd::Play, None);
        let from_fresh = take(&mut fresh);
        let _ = std::fs::remove_dir_all(root_a);
        let _ = std::fs::remove_dir_all(root_b);
        let primes = jumped.stems.as_ref().unwrap().render.as_ref().unwrap().primes;
        assert_eq!(primes, 2, "the same stretcher, primed again at the jump");
        assert_eq!(after_jump.len(), from_fresh.len());
        assert!(after_jump.iter().any(|v| *v != 0.0));
        // A reset stretcher settles from slightly different smoothing state
        // than a new one (the plain path's reset does the same): measured
        // 5e-5 at most (−80 dB) over the first ~20 ms, then sample-exact.
        let diff: Vec<f32> = after_jump.iter().zip(&from_fresh).map(|(a, b)| (a - b).abs()).collect();
        let worst = diff.iter().copied().fold(0.0f32, f32::max);
        assert!(worst < 1e-3, "differs from a fresh stretcher by {worst}");
        let tail = &diff[diff.len() * 3 / 4..];
        assert!(tail.iter().all(|d| *d == 0.0), "and then plays exactly the same");
    }

    /// Keylock on a stems deck stretches only an audible pitch change, as
    /// the plain path does: a fine tempo match plays through the resampler.
    #[test]
    fn a_keylocked_stem_deck_stretches_only_an_audible_change() {
        let rate = 48000usize;
        let x: Vec<f32> = (0..rate * 4).flat_map(|i| [(i as f32 * 0.01).sin() * 0.5; 2]).collect();
        let p = Pcm::from_samples(rate as u32, x);
        let primes = |tempo: f64| {
            let (audio, root) = audio(&p);
            let mut t = Transport::new(rate as u32);
            t.apply(Cmd::Load(p.clone()), None);
            t.apply(Cmd::AttachStems(audio.into()), None);
            t.apply(Cmd::Keylock(true), None);
            t.apply(Cmd::Tempo(tempo), None);
            t.apply(Cmd::Play, None);
            for _ in 0..(rate / CHUNK) {
                t.render(&mut |_, _| {});
            }
            let _ = std::fs::remove_dir_all(root);
            t.stems.as_ref().unwrap().render.as_ref().unwrap().primes
        };
        assert_eq!(primes(-1e-6), 0, "a −0.0001 % match must not stretch");
        assert_eq!(primes(0.002), 0, "0.2 % is below the audible threshold");
        assert_eq!(primes(0.01), 1, "1 % keylocked stretches");
    }

    /// A tempo change while playing (a sync), with the key shifted, reaches
    /// the stems' stretcher: their clicks come at the new rate, as the
    /// original's would.
    #[test]
    fn stems_follow_a_tempo_change_while_playing() {
        let rate = 48000usize;
        let x: Vec<f32> = (0..rate * 12)
            .flat_map(|i| {
                let v = if i % (rate / 2) < 24 { 0.8 } else { 0.0 };
                [v, v]
            })
            .collect();
        let p = Pcm::from_samples(rate as u32, x);
        let (audio, root) = audio(&p);
        let mut t = Transport::new(rate as u32);
        t.apply(Cmd::Load(p.clone()), None);
        t.apply(Cmd::AttachStems(audio.into()), None);
        t.apply(Cmd::Keylock(true), None);
        t.apply(Cmd::Key(-2), None);
        t.apply(Cmd::Play, None);
        let mut out: Vec<f32> = vec![];
        let mut synced = false;
        while out.len() < rate * 8 {
            if !synced && out.len() > rate * 2 {
                t.apply(Cmd::Tempo(-0.05), None);
                synced = true;
            }
            t.render(&mut |tag, a| {
                assert!(tag.bundle);
                out.extend(a.chunks_exact(STRIDE).map(|f| f[4]));
            });
        }
        let _ = std::fs::remove_dir_all(root);
        let on: Vec<usize> = (1..out.len())
            .filter(|&i| out[i].abs() > 0.3 && out[i - 1].abs() <= 0.3)
            .filter(|&i| out[i.saturating_sub(2000)..i].iter().all(|v| v.abs() < 0.3))
            .filter(|&i| i > rate * 4)
            .collect();
        let gaps: Vec<i64> = on.windows(2).map(|w| (w[1] - w[0]) as i64).collect();
        let want = (rate as f64 / 2.0 / 0.95) as i64;
        assert!(
            gaps.len() >= 4 && gaps.iter().all(|g| (g - want).abs() < 48),
            "want {want}: {gaps:?}"
        );
    }

    #[test]
    fn one_transport_keeps_every_source_sample_aligned_through_changes() {
        let x = (0..48000 * 3)
            .flat_map(|i| {
                let x = (i as f32 * 0.061).sin() * 0.12;
                [x, x]
            })
            .collect();
        let p = Pcm::from_samples(48000, x);
        let (a, root) = audio(&p);
        let mut t = Transport::new(48000);
        t.apply(Cmd::Load(p), None);
        t.apply(Cmd::AttachStems(a.into()), None);
        t.apply(
            Cmd::Grid(Some(Grid {
                bpm: 120.0,
                offset: 0.0,
                downbeat: Some(0),
            })),
            None,
        );
        t.apply(Cmd::Play, None);
        for command in [
            Cmd::Tempo(0.2),
            Cmd::Key(2),
            Cmd::Seek(0.5),
            Cmd::AutoLoop(1.0),
            Cmd::Keylock(false),
            Cmd::Reverse(true),
            Cmd::Scratch {
                seconds: 0.7,
                velocity: -0.8,
            },
            Cmd::ScratchEnd,
            Cmd::Reverse(false),
            Cmd::Pause,
            Cmd::Play,
        ] {
            t.apply(command, None);
            for _ in 0..6 {
                t.render(&mut |tag, a| {
                    assert!(tag.bundle);
                    assert_eq!(a.len(), tag.frames as usize * STRIDE);
                    for frame in a.chunks_exact(STRIDE) {
                        for c in 2..10 {
                            assert!(
                                (frame[c] - frame[c % 2]).abs() < 1e-5,
                                "source offset: {}",
                                frame[c] - frame[c % 2]
                            );
                        }
                    }
                });
            }
        }
        drop(t);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn timed_ramps_human_override_multi_solo_and_all_mute() {
        let p = Controls::default();
        p.ready.store(true, Ordering::Release);
        let mut mix = Mix::default();
        let mut a = [0.0; STRIDE];
        a[2..10].fill(0.1);
        p.ramp(0, 100, 100, 0.0).unwrap();
        for sample in 0..151 {
            mix.frame(&p, &a, true, sample);
        }
        let halfway = f32::from_bits(p.slots[0].gain.load(Ordering::Acquire) as u32);
        assert!((halfway - 0.5).abs() < 1e-5);
        p.set(0, StemValue::Gain(0.8), true).unwrap();
        for sample in 151..260 {
            mix.frame(&p, &a, true, sample);
        }
        assert!(
            (f32::from_bits(p.slots[0].gain.load(Ordering::Acquire) as u32) - 0.8).abs() < 1e-6
        );
        assert!(p.ramp(0, 300, 100, 0.0).is_err());
        p.set(0, StemValue::Solo(true), true).unwrap();
        p.set(1, StemValue::Solo(true), true).unwrap();
        for sample in 260..400 {
            mix.frame(&p, &a, true, sample);
        }
        let out = mix.frame(&p, &a, true, 401);
        assert!((out[0] - 0.18).abs() < 1e-4);
        for i in 0..4 {
            p.set(i, StemValue::Mute(true), true).unwrap();
        }
        for sample in 402..550 {
            mix.frame(&p, &a, true, sample);
        }
        assert_eq!(mix.frame(&p, &a, true, 551), [0.0, 0.0]);
    }

    #[test]
    fn bounded_automation_queue_runs_sequential_stem_changes() {
        let controls = Controls::default();
        controls.ready.store(true, Ordering::Release);
        let mut mix = Mix::default();
        let audio = [0.0; STRIDE];
        controls.ramp(0, 10, 10, 0.25).unwrap();
        controls.ramp(0, 30, 10, 0.75).unwrap();
        assert!(controls.ramp(0, 35, 10, 0.5).is_err());

        for sample in 0..=15 {
            mix.frame(&controls, &audio, true, sample);
        }
        let gain = || f32::from_bits(controls.slots[0].gain.load(Ordering::Acquire) as u32);
        assert!((gain() - 0.625).abs() < 1e-6);
        for sample in 16..=35 {
            mix.frame(&controls, &audio, true, sample);
        }
        assert!((gain() - 0.5).abs() < 1e-6);
        assert_eq!(controls.snapshot()["controls"][0]["automation_queued"], 1);
        for sample in 36..=50 {
            mix.frame(&controls, &audio, true, sample);
        }
        assert!((gain() - 0.75).abs() < 1e-6);
        assert_eq!(controls.snapshot()["controls"][0]["automation_queued"], 0);
    }

    #[test]
    fn original_mix_automation_runs_without_stems() {
        let controls = Controls::default();
        let mut mix = Mix::default();
        let mut audio = [0.0; STRIDE];
        audio[0] = 0.4;
        audio[1] = -0.4;
        controls.ramp_original(10, 10, 0.0).unwrap();
        let start = mix.frame(&controls, &audio, false, 10);
        let middle = mix.frame(&controls, &audio, false, 15);
        let end = mix.frame(&controls, &audio, false, 20);
        assert!((start[0] - 0.4).abs() < 1e-6);
        assert!((middle[0] - 0.2).abs() < 1e-6);
        assert!(end[0].abs() < 1e-6);
    }

    #[test]
    fn automation_ring_is_bounded_and_reuses_consumed_slots() {
        let controls = Controls::default();
        controls.ready.store(true, Ordering::Release);
        let mut mix = Mix::default();
        let audio = [0.0; STRIDE];
        for index in 0..MAX_RAMPS_PER_GAIN_CONTROL {
            controls
                .ramp(0, (index * 2) as u64, 1, (index as f32 + 1.0) / 16.0)
                .unwrap();
        }
        assert!(controls.ramp(0, 40, 1, 0.5).is_err());
        for sample in 0..=31 {
            mix.frame(&controls, &audio, true, sample);
        }
        assert_eq!(controls.snapshot()["controls"][0]["automation_queued"], 0);
        controls.ramp(0, 32, 1, 0.75).unwrap();
        mix.frame(&controls, &audio, true, 32);
        mix.frame(&controls, &audio, true, 33);
        assert_eq!(
            f32::from_bits(controls.slots[0].gain.load(Ordering::Acquire) as u32),
            0.75
        );
    }

    #[test]
    fn bridge_tap_isolated_and_obeys_same_stem_controls() {
        let p = Controls::default();
        p.ready.store(true, Ordering::Release);
        p.enabled.store(true, Ordering::Release);
        let mut mix = Mix::default();
        let mut a = [0.0; STRIDE];
        a[2..10].fill(0.1);
        for sample in 0..128 {
            mix.frame_with_send(&p, &a, true, sample, Some(0));
        }
        let tap = mix.frame_with_send(&p, &a, true, 128, Some(0));
        assert!((tap[2] - 0.1).abs() < 1e-5);
        assert!((tap[3] - 0.1).abs() < 1e-5);
        p.set(0, StemValue::Mute(true), true).unwrap();
        for sample in 129..220 {
            mix.frame_with_send(&p, &a, true, sample, Some(0));
        }
        let muted = mix.frame_with_send(&p, &a, true, 220, Some(0));
        assert!(muted[2].abs() < 1e-5 && muted[3].abs() < 1e-5);
    }

    /// R3: stem render reuses the transport's bundle and planar scratch.
    #[test]
    fn stem_render_reuses_transport_buffers() {
        let rate = 48_000u32;
        let x: Vec<f32> = (0..rate as usize * 4)
            .flat_map(|i| {
                let v = (i as f32 * 0.02).sin() * 0.2;
                [v, v]
            })
            .collect();
        let p = Pcm::from_samples(rate, x);
        let (audio, root) = audio(&p);
        let mut t = Transport::new(rate);
        t.apply(Cmd::Load(p), None);
        t.apply(Cmd::AttachStems(audio.into()), None);
        t.apply(Cmd::Keylock(true), None);
        t.apply(Cmd::Tempo(0.03), None);
        t.apply(Cmd::Play, None);
        t.render(&mut |_, a| assert_eq!(a.len() % STRIDE, 0));
        let bundle = t.bundle.capacity();
        let planes: Vec<usize> = t.planes.iter().map(Vec::capacity).collect();
        for _ in 0..12 {
            t.render(&mut |tag, a| {
                assert!(tag.bundle);
                assert_eq!(a.len(), tag.frames as usize * STRIDE);
            });
        }
        assert_eq!(t.bundle.capacity(), bundle);
        assert_eq!(
            t.planes.iter().map(Vec::capacity).collect::<Vec<_>>(),
            planes
        );
        drop(t);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A stopped deck with stems takes every seek (live, the second seek on a
    /// paused deck with a stem set was lost: it played on from the first).
    #[test]
    fn a_stopped_deck_with_stems_takes_every_seek() {
        let rate = 48_000u32;
        let p = Pcm::from_samples(rate, [0.2f32, -0.2].repeat(rate as usize * 20));
        let sh = super::super::worker::Shared::new(rate);
        sh.spawn("stem-seek");
        sh.send(Cmd::Load(p.clone()));
        let (audio, root) = audio(&p);
        sh.send(Cmd::AttachStems(audio.into()));
        let mut c = super::super::worker::Consumer::new();
        let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        let mut settle = |c: &mut super::super::worker::Consumer| {
            for _ in 0..8 {
                std::thread::sleep(std::time::Duration::from_millis(10));
                c.pull(&sh, &mut l, &mut r);
            }
            sh.playhead().0 / rate as f64
        };
        settle(&mut c);
        sh.send(Cmd::Play);
        for _ in 0..4 {
            settle(&mut c);
        }
        sh.send(Cmd::Pause);
        settle(&mut c);
        for at in [10.0, 3.0, 15.0, 3.0] {
            sh.send(Cmd::Seek(at));
            let here = settle(&mut c);
            assert!((here - at).abs() < 0.05, "stopped at {here} s after a seek to {at} s");
        }
        sh.send(Cmd::Play);
        let here = settle(&mut c);
        assert!((3.0..3.5).contains(&here), "played from {here} s, not 3 s");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// R1: attaching stems while a control thread holds `stem_set` still renders.
    #[test]
    fn attaching_stems_does_not_block_the_worker_on_stem_set() {
        let rate = 48_000u32;
        let p = Pcm::from_samples(rate, [0.2f32, -0.2].repeat(rate as usize));
        let sh = super::super::worker::Shared::new(rate);
        sh.spawn("stem-attach");
        sh.send(Cmd::Load(p.clone()));
        sh.send(Cmd::Play);
        std::thread::sleep(std::time::Duration::from_millis(40));
        let (audio, root) = audio(&p);
        let id = audio.set.id.clone();
        let hold = sh.stem_set.lock().unwrap();
        sh.send(Cmd::AttachStems(audio.into()));
        let mut consumer = super::super::worker::Consumer::new();
        let (mut left, mut right) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        let start = std::time::Instant::now();
        let mut heard = false;
        while start.elapsed() < std::time::Duration::from_millis(400) {
            consumer.pull(&sh, &mut left, &mut right);
            if left.iter().any(|sample| sample.abs() > 0.05) {
                heard = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(heard, "deck worker stalled while stem_set was held");
        drop(hold);
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_millis(500) {
            let published = sh
                .stem_set
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|set| set.id == id);
            if published && sh.stems.ready.load(Ordering::Acquire) {
                let _ = std::fs::remove_dir_all(root);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let _ = std::fs::remove_dir_all(&root);
        panic!("stem set was not published after the lock was released");
    }

    /// A load clears `stem_error`. If that lock is held, a following attach
    /// in the same turn must not drop the clear.
    #[test]
    fn attaching_after_a_blocked_load_still_clears_stem_error() {
        let rate = 48_000u32;
        let p = Pcm::from_samples(rate, [0.2f32, -0.2].repeat(rate as usize));
        let sh = super::super::worker::Shared::new(rate);
        *sh.stem_error.lock().unwrap() = Some(Arc::from("old"));
        sh.spawn("stem-error-clear");
        let (audio, root) = audio(&p);
        let id = audio.set.id.clone();
        let hold = sh.stem_error.lock().unwrap();
        sh.send(Cmd::Load(p));
        sh.send(Cmd::AttachStems(audio.into()));
        std::thread::sleep(std::time::Duration::from_millis(80));
        assert_eq!(hold.as_deref(), Some("old"));
        drop(hold);
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_millis(500) {
            let error_cleared = sh.stem_error.lock().unwrap().is_none();
            let published = sh
                .stem_set
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|set| set.id == id);
            if error_cleared && published && sh.stems.ready.load(Ordering::Acquire) {
                let _ = std::fs::remove_dir_all(root);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let error = sh.stem_error.lock().unwrap().clone();
        let _ = std::fs::remove_dir_all(&root);
        panic!("stem error {error:?} was not cleared when the attached set landed");
    }
}
