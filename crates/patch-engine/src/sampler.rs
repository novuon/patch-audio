//! The sampler: prepared derived material played on the audio thread.
//!
//! A fixed table of slots holds immutable buffers, loaded by the control
//! thread while a slot has no voices. Pattern's `Sample` events arrive in the
//! same real-time queue as Sequencer voices as `RtEvent { kind: Sample,
//! index: slot, value: gain }`; the sampler starts a voice on the event's
//! exact sample with a short raised-cosine fade (no clicks), loops when the
//! asset is a loop, and hands slots back to the control thread only once no
//! voice uses them, then after a two-block grace period.
//!
//! Nothing here allocates, locks or frees on the audio thread: buffers are
//! `Arc<[f32]>` published before a slot is ready, and the RT thread only reads.

use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

/// Slots in the fixed table.
pub const SLOTS: usize = 64;
/// Simultaneous voices (a busy table steals the oldest).
pub const VOICES: usize = 32;
/// Start/stop/steal fade, ms.
pub const FADE_MS: f64 = 2.0;
/// Blocks an unused slot waits before the control thread retires it.
pub const RETIRE_BLOCKS: u64 = 2;

/// A prepared, immutable stereo buffer at the graph rate (interleaved).
#[derive(Clone, Debug)]
pub struct Buffer {
    pub samples: Arc<[f32]>,
    pub looped: bool,
    /// The asset id, for provenance (the control thread's bookkeeping).
    pub asset: String,
}

impl Buffer {
    pub fn frames(&self) -> usize {
        self.samples.len() / 2
    }
}

/// A single-producer, single-consumer mailbox for values that own memory
/// (buffers): moving one in or out never allocates or frees.
pub(crate) struct Mailbox<T> {
    slots: Box<[UnsafeCell<Option<T>>]>,
    mask: usize,
    head: AtomicUsize,
    tail: AtomicUsize,
}

// SAFETY: as `patch_pattern::rt::Queue`: the producer writes only outside
// [tail, head) and publishes with a release store of `head`; the consumer
// reads only inside it after an acquire load.
unsafe impl<T: Send> Sync for Mailbox<T> {}

impl<T> Mailbox<T> {
    pub(crate) fn new(capacity: usize) -> Mailbox<T> {
        let cap = capacity.next_power_of_two().max(2);
        Mailbox {
            slots: (0..cap).map(|_| UnsafeCell::new(None)).collect(),
            mask: cap - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    pub(crate) fn push(&self, v: T) -> Result<(), T> {
        let head = self.head.load(Ordering::Relaxed);
        if head - self.tail.load(Ordering::Acquire) > self.mask {
            return Err(v);
        }
        // SAFETY: outside [tail, head): the consumer isn't reading it, and
        // the slot was emptied by `pop` (no drop happens here).
        unsafe { *self.slots[head & self.mask].get() = Some(v) };
        self.head.store(head + 1, Ordering::Release);
        Ok(())
    }

    pub(crate) fn pop(&self) -> Option<T> {
        let tail = self.tail.load(Ordering::Relaxed);
        if tail == self.head.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: inside [tail, head): published, and ours to take.
        let v = unsafe { (*self.slots[tail & self.mask].get()).take() };
        self.tail.store(tail + 1, Ordering::Release);
        v
    }
}

enum Request {
    Install(usize, Buffer),
    Retire(usize),
}

/// What the control thread and the audio thread share. The control thread
/// installs and retires slots through it; the audio thread applies them at
/// the start of a block, gives every replaced or refused buffer back (it is
/// freed on the control thread, never in the callback) and publishes which
/// slots are loaded and how many voices play each.
pub struct Shared {
    requests: Mailbox<Request>,
    back: Mailbox<Buffer>,
    active: [AtomicU32; SLOTS],
    loaded: [AtomicBool; SLOTS],
    stop: AtomicU32,
    pub stolen: AtomicU64,
    pub missing: AtomicU64,
    /// Installs refused because a voice still played the slot.
    pub refused: AtomicU64,
    /// Buffers the audio thread couldn't give back (leaked, not freed there).
    pub leaked: AtomicU64,
}

impl Default for Shared {
    fn default() -> Shared {
        Shared {
            requests: Mailbox::new(SLOTS * 2),
            back: Mailbox::new(SLOTS * 4),
            active: std::array::from_fn(|_| AtomicU32::new(0)),
            loaded: std::array::from_fn(|_| AtomicBool::new(false)),
            stop: AtomicU32::new(0),
            stolen: AtomicU64::new(0),
            missing: AtomicU64::new(0),
            refused: AtomicU64::new(0),
            leaked: AtomicU64::new(0),
        }
    }
}

impl Shared {
    /// Control thread: free what the audio thread gave back.
    pub fn collect(&self) -> usize {
        let mut n = 0;
        while self.back.pop().is_some() {
            n += 1;
        }
        n
    }

    /// Control thread: load `slot` from the next block on. The audio thread
    /// refuses it (and gives it back) while a voice plays the slot.
    pub fn install(&self, slot: usize, buffer: Buffer) -> Result<(), String> {
        if slot >= SLOTS {
            return Err("slot out of range".into());
        }
        self.collect();
        self.requests
            .push(Request::Install(slot, buffer))
            .map_err(|_| "the sampler is busy".to_string())
    }

    /// Control thread: empty `slot` once nothing plays it (after the grace).
    pub fn retire(&self, slot: usize) -> Result<(), String> {
        self.collect();
        self.requests
            .push(Request::Retire(slot))
            .map_err(|_| "the sampler is busy".to_string())
    }

    /// Control thread: fade every voice out (the pattern stopped).
    pub fn stop_all(&self) {
        self.stop.fetch_add(1, Ordering::Release);
    }

    pub fn active(&self, slot: usize) -> u32 {
        self.active
            .get(slot)
            .map_or(0, |a| a.load(Ordering::Acquire))
    }

    pub fn loaded(&self, slot: usize) -> bool {
        self.loaded
            .get(slot)
            .is_some_and(|a| a.load(Ordering::Acquire))
    }
}

#[derive(Default)]
struct Slot {
    buffer: Option<Buffer>,
    active: u32,
    /// Block at which it last became idle (for the grace period).
    idle_since: Option<u64>,
}

#[derive(Default)]
struct Voice {
    slot: Option<usize>,
    /// Next frame to read.
    pos: usize,
    gain: f32,
    /// Frames to wait before the first sample (a late-in-block event).
    delay: u32,
    /// Frames of the start fade still to run.
    fade_in: u32,
    /// When set, the voice fades out and ends by this frame.
    stop_at: Option<usize>,
    /// The block it started in (for stealing the oldest).
    started: u64,
}

impl Voice {
    fn busy(&self) -> bool {
        self.slot.is_some()
    }
}

fn raised_cosine(x: f64) -> f64 {
    0.5 - 0.5 * (std::f64::consts::PI * x.clamp(0.0, 1.0)).cos()
}

/// The sampler's real-time state.
pub struct Sampler {
    slots: Vec<Slot>,
    voices: Vec<Voice>,
    rate: u32,
    fade_frames: u32,
    /// Monotonic block counter (the grace clock).
    block: u64,
    /// Voices stolen because every voice was busy.
    pub stolen: u64,
    /// Events that named a slot with nothing in it.
    pub missing: u64,
    /// Slots the control thread asked to retire, waiting for their grace.
    retiring: [bool; SLOTS],
    /// The last stop request seen (`Shared::stop_all`).
    stop_seen: u32,
}

impl Default for Sampler {
    fn default() -> Sampler {
        Sampler::new(48_000)
    }
}

impl Sampler {
    pub fn new(rate: u32) -> Sampler {
        let rate = rate.max(1);
        Sampler {
            slots: (0..SLOTS).map(|_| Slot::default()).collect(),
            voices: (0..VOICES).map(|_| Voice::default()).collect(),
            rate,
            fade_frames: fade_frames(rate),
            block: 0,
            stolen: 0,
            missing: 0,
            retiring: [false; SLOTS],
            stop_seen: 0,
        }
    }

    pub fn set_rate(&mut self, rate: u32) {
        self.rate = rate.max(1);
        self.fade_frames = fade_frames(self.rate);
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Control thread: load a slot. Refused while a voice plays it.
    pub fn install(&mut self, slot: usize, buffer: Buffer) -> Result<(), String> {
        let s = self.slots.get_mut(slot).ok_or("slot out of range")?;
        if s.active > 0 {
            return Err(format!("slot {slot} is playing"));
        }
        s.buffer = Some(buffer);
        s.idle_since = None;
        Ok(())
    }

    /// Control thread: hand a slot back once no voice uses it, after the
    /// grace period. Returns whether it was retired now.
    pub fn release(&mut self, slot: usize) -> bool {
        self.take_idle(slot).is_some()
    }

    /// The slot's buffer, taken out once no voice used it for the grace
    /// period (the caller decides where it is freed).
    fn take_idle(&mut self, slot: usize) -> Option<Buffer> {
        let block = self.block;
        let s = self.slots.get_mut(slot)?;
        if s.active > 0 || s.buffer.is_none() {
            return None;
        }
        let idled = *s.idle_since.get_or_insert(block);
        if block.saturating_sub(idled) < RETIRE_BLOCKS {
            return None;
        }
        s.idle_since = None;
        s.buffer.take()
    }

    /// Audio thread, at the start of a block: apply the control thread's
    /// installs, retires and stops. Every buffer that leaves a slot goes back
    /// to the control thread to be freed there.
    pub fn sync(&mut self, shared: &Shared) {
        let give_back = |b: Buffer| {
            if let Err(b) = shared.back.push(b) {
                shared.leaked.fetch_add(1, Ordering::Relaxed);
                std::mem::forget(b);
            }
        };
        let stop = shared.stop.load(Ordering::Acquire);
        if stop != self.stop_seen {
            self.stop_seen = stop;
            for slot in 0..self.slots.len() {
                self.stop_slot(slot);
            }
        }
        while let Some(r) = shared.requests.pop() {
            match r {
                Request::Install(slot, buffer) => {
                    if self.slots[slot].active > 0 {
                        shared.refused.fetch_add(1, Ordering::Relaxed);
                        give_back(buffer);
                        continue;
                    }
                    self.retiring[slot] = false;
                    self.slots[slot].idle_since = None;
                    if let Some(old) = self.slots[slot].buffer.replace(buffer) {
                        give_back(old);
                    }
                }
                Request::Retire(slot) if slot < SLOTS => self.retiring[slot] = true,
                Request::Retire(_) => {}
            }
        }
        for slot in 0..SLOTS {
            if self.retiring[slot]
                && let Some(b) = self.take_idle(slot)
            {
                self.retiring[slot] = false;
                give_back(b);
            }
        }
    }

    /// Audio thread, at the end of a block: what the control thread reads.
    pub fn publish(&self, shared: &Shared) {
        for (i, s) in self.slots.iter().enumerate() {
            shared.active[i].store(s.active, Ordering::Release);
            shared.loaded[i].store(s.buffer.is_some(), Ordering::Release);
        }
        shared.stolen.store(self.stolen, Ordering::Relaxed);
        shared.missing.store(self.missing, Ordering::Relaxed);
    }

    /// Control thread: slots with nothing in them and no voices.
    pub fn free_slots(&self) -> Vec<usize> {
        (0..self.slots.len())
            .filter(|i| self.slots[*i].buffer.is_none() && self.slots[*i].active == 0)
            .collect()
    }

    pub fn loaded(&self) -> usize {
        self.slots.iter().filter(|s| s.buffer.is_some()).count()
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.busy()).count()
    }

    pub fn slot_voices(&self, slot: usize) -> u32 {
        self.slots.get(slot).map_or(0, |s| s.active)
    }

    pub fn loaded_asset(&self, slot: usize) -> Option<&str> {
        self.slots
            .get(slot)
            .and_then(|s| s.buffer.as_ref())
            .map(|b| b.asset.as_str())
    }

    pub fn current_block(&self) -> u64 {
        self.block
    }

    /// Audio thread: a `Sample` event `offset` frames into the current block
    /// (a late event, negative, starts immediately).
    pub fn trigger(&mut self, slot: usize, offset: i64, gain: f32) {
        if slot >= self.slots.len() || self.slots[slot].buffer.is_none() {
            self.missing += 1;
            return;
        }
        // A loop retriggered restarts in phase: the running one fades out.
        if self.slots[slot].buffer.as_ref().is_some_and(|b| b.looped) {
            self.stop_slot(slot);
        }
        let idx = self
            .voices
            .iter()
            .position(|v| !v.busy())
            .unwrap_or_else(|| {
                self.stolen += 1;
                self.voices
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, v)| v.started)
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            });
        self.end_voice(idx);
        self.voices[idx] = Voice {
            slot: Some(slot),
            pos: 0,
            gain: gain.clamp(0.0, 4.0),
            delay: offset.max(0) as u32,
            fade_in: self.fade_frames,
            stop_at: None,
            started: self.block,
        };
        self.slots[slot].active += 1;
        self.slots[slot].idle_since = None;
    }

    /// Audio thread: fade every voice on `slot` out and end it.
    pub fn stop_slot(&mut self, slot: usize) {
        let total = self
            .slots
            .get(slot)
            .and_then(|s| s.buffer.as_ref())
            .map(Buffer::frames)
            .unwrap_or(0);
        for v in self.voices.iter_mut() {
            if v.slot == Some(slot) && v.stop_at.is_none() {
                v.stop_at = Some((v.pos + self.fade_frames as usize).min(total));
            }
        }
    }

    /// Audio thread: mix every live voice into the block. Returns the number
    /// of voices that were live during it.
    pub fn render(&mut self, frames: usize, out_l: &mut [f32], out_r: &mut [f32]) -> usize {
        self.render_gain(frames, out_l, out_r, [1.0, 1.0])
    }

    /// As [`Sampler::render`], scaled per side (the Sequencer's master level).
    pub fn render_gain(
        &mut self,
        frames: usize,
        out_l: &mut [f32],
        out_r: &mut [f32],
        gain: [f32; 2],
    ) -> usize {
        let frames = frames.min(out_l.len()).min(out_r.len());
        let mut live = 0;
        for vi in 0..self.voices.len() {
            if !self.voices[vi].busy() {
                continue;
            }
            live += 1;
            let slot = self.voices[vi].slot.unwrap_or(0);
            // Only the samples' `Arc` (a count, no allocation): the slot
            // keeps its own reference, so nothing is freed here.
            let Some((samples, looped)) = self.slots[slot]
                .buffer
                .as_ref()
                .map(|b| (b.samples.clone(), b.looped))
            else {
                self.end_voice(vi);
                continue;
            };
            let total = samples.len() / 2;
            if total == 0 {
                self.end_voice(vi);
                continue;
            }
            let mut done = false;
            for i in 0..frames {
                let v = &mut self.voices[vi];
                if v.delay > 0 {
                    v.delay -= 1;
                    continue;
                }
                if v.pos >= total {
                    if looped {
                        v.pos = 0;
                    } else {
                        done = true;
                        break;
                    }
                }
                let mut g = v.gain;
                if v.fade_in > 0 {
                    let t = 1.0 - (v.fade_in as f64 / self.fade_frames as f64);
                    g *= raised_cosine(t) as f32;
                    v.fade_in -= 1;
                }
                if let Some(stop) = v.stop_at {
                    if v.pos + self.fade_frames as usize >= stop {
                        let rem = stop.saturating_sub(v.pos);
                        g *= raised_cosine(rem as f64 / self.fade_frames as f64) as f32;
                    }
                    if v.pos >= stop {
                        done = true;
                        break;
                    }
                }
                out_l[i] += samples[v.pos * 2] * g * gain[0];
                out_r[i] += samples[v.pos * 2 + 1] * g * gain[1];
                v.pos += 1;
            }
            if done {
                self.end_voice(vi);
            }
        }
        live
    }

    /// The grace clock ticks once per block (the audio thread calls it at the
    /// end of `render`).
    pub fn tick(&mut self) {
        self.block += 1;
    }

    fn end_voice(&mut self, vi: usize) {
        if let Some(slot) = self.voices[vi].slot {
            let s = &mut self.slots[slot];
            s.active = s.active.saturating_sub(1);
            if s.active == 0 && s.idle_since.is_none() {
                s.idle_since = Some(self.block);
            }
        }
        self.voices[vi] = Voice::default();
    }
}

fn fade_frames(rate: u32) -> u32 {
    ((FADE_MS / 1000.0) * rate.max(1) as f64).round().max(1.0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(frames: usize) -> Buffer {
        let samples: Vec<f32> = (0..frames * 2)
            .map(|i| (i as f32 / (frames * 2) as f32) * 0.5)
            .collect();
        Buffer {
            samples: samples.into(),
            looped: false,
            asset: "dm-test".into(),
        }
    }

    fn looped(frames: usize) -> Buffer {
        Buffer {
            samples: vec![0.5f32; frames * 2].into(),
            looped: true,
            asset: "dm-loop".into(),
        }
    }

    #[test]
    fn a_trigger_plays_from_its_own_sample_with_a_fade() {
        let mut s = Sampler::new(48_000);
        s.install(0, ramp(4800)).unwrap();
        let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        s.trigger(0, 0, 1.0);
        let live = s.render(512, &mut l, &mut r);
        assert_eq!(live, 1);
        // The fade starts at zero and rises: no click at the trigger.
        assert!(l[0].abs() < 1e-6);
        assert!(l[511] > l[10], "the fade rises");
        // A trigger later in the block delays the sound.
        let mut s = Sampler::new(48_000);
        s.install(0, ramp(4800)).unwrap();
        let (mut l2, mut r2) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        s.trigger(0, 100, 1.0);
        s.render(512, &mut l2, &mut r2);
        assert!(
            l2[..100].iter().all(|x| *x == 0.0),
            "silent until its sample"
        );
        assert!(l2[101..].iter().any(|x| *x != 0.0));
    }

    #[test]
    fn a_loop_wraps_and_a_one_shot_ends() {
        let mut s = Sampler::new(48_000);
        s.install(0, looped(100)).unwrap();
        s.trigger(0, 0, 1.0);
        for _ in 0..4 {
            let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
            s.render(256, &mut l, &mut r);
            assert!(l.iter().any(|x| *x != 0.0));
        }
        assert_eq!(s.active_voices(), 1, "a loop keeps playing");

        let mut s = Sampler::new(48_000);
        s.install(0, ramp(100)).unwrap();
        s.trigger(0, 0, 1.0);
        let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        s.render(256, &mut l, &mut r);
        assert_eq!(s.active_voices(), 0, "a one-shot ends");
    }

    #[test]
    fn the_table_steals_the_oldest_when_full() {
        // Long one-shots stack (a loop retriggered chokes itself instead).
        let mut s = Sampler::new(48_000);
        s.install(0, ramp(100_000)).unwrap();
        for i in 0..VOICES + 4 {
            s.tick();
            s.trigger(0, 0, 1.0);
            let (mut l, mut r) = (vec![0.0f32; 64], vec![0.0f32; 64]);
            s.render(64, &mut l, &mut r);
            let _ = i;
        }
        assert!(s.stolen >= 4);
        assert!(s.active_voices() <= VOICES);
    }

    #[test]
    fn a_slot_is_released_only_after_the_grace_and_when_unused() {
        let mut s = Sampler::new(48_000);
        s.install(0, ramp(16)).unwrap();
        s.trigger(0, 0, 1.0);
        assert!(!s.release(0), "a playing slot is never retired");
        let (mut l, mut r) = (vec![0.0f32; 64], vec![0.0f32; 64]);
        s.render(64, &mut l, &mut r);
        assert_eq!(s.slot_voices(0), 0);
        // Idle now, but within the grace period.
        assert!(!s.release(0), "the grace period has not passed");
        s.tick();
        s.tick();
        assert!(s.release(0), "retired after two blocks");
        assert!(s.free_slots().contains(&0));
        assert_eq!(s.loaded(), 0);
    }

    #[test]
    fn installing_into_a_playing_slot_is_refused() {
        let mut s = Sampler::new(48_000);
        s.install(0, looped(1024)).unwrap();
        s.trigger(0, 0, 1.0);
        assert!(s.install(0, ramp(10)).is_err());
        assert!(s.missing == 0);
        s.trigger(5, 0, 1.0); // nothing loaded there
        assert_eq!(s.missing, 1);
    }

    #[test]
    fn stop_slot_fades_a_loop_out() {
        let mut s = Sampler::new(48_000);
        s.install(0, looped(4096)).unwrap();
        s.trigger(0, 0, 1.0);
        let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        s.render(256, &mut l, &mut r);
        s.stop_slot(0);
        let (mut l2, mut r2) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        s.render(256, &mut l2, &mut r2);
        assert_eq!(s.active_voices(), 0);
        // The end of the stop block is silent (faded), no hard cut.
        assert!(l2[255].abs() < 0.01, "{}", l2[255]);
    }

    #[test]
    fn the_control_thread_installs_and_retires_through_the_handoff() {
        let shared = Shared::default();
        let mut s = Sampler::new(48_000);
        shared.install(3, ramp(1_000)).unwrap();
        assert!(!shared.loaded(3), "applied at the next block");
        s.sync(&shared);
        s.publish(&shared);
        assert!(shared.loaded(3));
        s.trigger(3, 0, 1.0);
        s.publish(&shared);
        assert_eq!(shared.active(3), 1);
        // Refused while it plays; the buffer comes back to be freed here.
        shared.install(3, ramp(10)).unwrap();
        s.sync(&shared);
        assert_eq!(shared.refused.load(Ordering::Relaxed), 1);
        assert_eq!(shared.collect(), 1);
        // Retire: waits for the voice to end, then the grace, then gives back.
        shared.retire(3).unwrap();
        let (mut l, mut r) = (vec![0.0; 2_000], vec![0.0; 2_000]);
        for _ in 0..4 {
            s.sync(&shared);
            s.render(2_000, &mut l, &mut r);
            s.tick();
            s.publish(&shared);
        }
        assert!(!shared.loaded(3));
        assert_eq!(
            shared.collect(),
            1,
            "the retired buffer, freed off the audio thread"
        );
    }

    #[test]
    fn a_loop_retriggered_restarts_and_stop_all_fades_everything() {
        let shared = Shared::default();
        let mut s = Sampler::new(48_000);
        shared.install(0, looped(500)).unwrap();
        s.sync(&shared);
        s.trigger(0, 0, 1.0);
        let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
        s.render(256, &mut l, &mut r);
        s.trigger(0, 0, 1.0);
        // The old voice fades within the block; one remains.
        let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
        s.render(256, &mut l, &mut r);
        assert_eq!(s.slot_voices(0), 1);
        shared.stop_all();
        s.sync(&shared);
        let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
        s.render(256, &mut l, &mut r);
        assert_eq!(s.active_voices(), 0);
    }
}
