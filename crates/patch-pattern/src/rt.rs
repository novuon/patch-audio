//! The hand-off to the audio thread: prepared, timed, plain events in a
//! preallocated single-producer / single-consumer queue.
//!
//! The scheduler (a control thread) pushes events in time order; the audio
//! callback takes those due before the end of its block. Neither side
//! allocates, locks or waits: a full queue drops the newest event and
//! counts it. The audio thread never sees a pattern, a string or an engine.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum RtKind {
    #[default]
    None = 0,
    /// A Sequencer voice (`index`), velocity in `value`.
    Voice = 1,
    /// A prepared sample (`index` into the consumer's handle table), gain
    /// in `value`.
    Sample = 2,
    /// A sound of the pattern instrument (`index`: [`crate::instrument`]'s
    /// sound number), level in `value`, its controls in `params`.
    Play = 3,
}

/// The controls an instrument event carries, plain numbers (the audio
/// thread sees no names). A negative time or size means "the sound's own"
/// (or, for the shared delay and room, "as it was").
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RtParams {
    /// MIDI pitch (synths).
    pub note: f32,
    /// −1 (left) … 1 (right).
    pub pan: f32,
    /// Playback rate (drums: its size; samples: below 0 plays backwards).
    pub speed: f32,
    /// Low-pass cutoff in Hz (0: open), its resonance 0 … 1.
    pub cutoff: f32,
    pub resonance: f32,
    /// High-pass cutoff in Hz (0: none).
    pub hcutoff: f32,
    /// Envelope, seconds.
    pub attack: f32,
    pub release: f32,
    /// How long the note holds before its release.
    pub sustain: f32,
    /// Waveshaping 0 … 1, bit depth (0: off).
    pub shape: f32,
    pub crush: f32,
    /// Send to the shared delay (0 … 1), its time (s) and feedback.
    pub delay: f32,
    pub delaytime: f32,
    pub delayfeedback: f32,
    /// Send to the shared room (0 … 1) and its size.
    pub room: f32,
    pub size: f32,
    /// Samples: the part played (fractions of the file, 0 … 1).
    pub begin: f32,
    pub end: f32,
    /// Samples: the rate's change over the sound (Tidal's `accelerate`).
    pub accelerate: f32,
    /// A cut group (0: none): a new sound in it ends the one before.
    pub cut: u32,
    /// Samples: times the part plays (Tidal's `loop`; 1: once).
    pub loops: f32,
    /// Sample-rate reduction: each value held this many frames (0: off).
    pub coarse: f32,
}

impl Default for RtParams {
    fn default() -> RtParams {
        RtParams {
            note: 60.0,
            pan: 0.0,
            speed: 1.0,
            cutoff: 0.0,
            resonance: 0.0,
            hcutoff: 0.0,
            attack: -1.0,
            release: -1.0,
            sustain: -1.0,
            shape: 0.0,
            crush: 0.0,
            delay: 0.0,
            delaytime: -1.0,
            delayfeedback: -1.0,
            room: 0.0,
            size: -1.0,
            begin: 0.0,
            end: 1.0,
            accelerate: 0.0,
            cut: 0,
            loops: 1.0,
            coarse: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RtEvent {
    /// Graph sample it starts at.
    pub sample: i64,
    pub kind: RtKind,
    pub index: u32,
    pub value: f32,
    /// Set by [`Queue::push`]: events from before a [`Queue::cancel`]
    /// are skipped.
    pub epoch: u32,
    /// What a [`RtKind::Play`] event plays with.
    pub params: RtParams,
}

pub struct Queue {
    slots: Box<[UnsafeCell<RtEvent>]>,
    mask: usize,
    head: AtomicUsize,
    tail: AtomicUsize,
    epoch: std::sync::atomic::AtomicU32,
    pub dropped: AtomicU64,
}

// SAFETY: one producer writes slots outside [tail, head) and publishes them
// with a release store of `head`; one consumer reads slots in [tail, head)
// after an acquire load. No slot is touched by both at once.
unsafe impl Sync for Queue {}
unsafe impl Send for Queue {}

impl Queue {
    pub fn new(capacity: usize) -> Queue {
        let cap = capacity.next_power_of_two().max(2);
        Queue {
            slots: (0..cap)
                .map(|_| UnsafeCell::new(RtEvent::default()))
                .collect(),
            mask: cap - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            epoch: std::sync::atomic::AtomicU32::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    /// Producer: false (and a drop counted) when full.
    ///
    /// # Safety
    /// No other thread may be producing at the same time.
    pub unsafe fn push(&self, mut e: RtEvent) -> bool {
        e.epoch = self.epoch.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head - tail > self.mask {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        // SAFETY: outside [tail, head): the consumer isn't reading it.
        unsafe { *self.slots[head & self.mask].get() = e };
        self.head.store(head + 1, Ordering::Release);
        true
    }

    /// Consumer: every event before `end`, in order, to `f`. Real-time safe.
    ///
    /// # Safety
    /// No other thread may be consuming at the same time.
    pub unsafe fn drain_before(&self, end: i64, mut f: impl FnMut(RtEvent)) {
        let head = self.head.load(Ordering::Acquire);
        let mut tail = self.tail.load(Ordering::Relaxed);
        let epoch = self.epoch.load(Ordering::Acquire);
        while tail < head {
            // SAFETY: inside [tail, head): published, and ours to read.
            let e = unsafe { *self.slots[tail & self.mask].get() };
            if e.epoch != epoch {
                tail += 1;
                continue;
            }
            if e.sample >= end {
                break;
            }
            f(e);
            tail += 1;
        }
        self.tail.store(tail, Ordering::Release);
    }

    /// Producer: everything queued so far is cancelled (the transport
    /// stopped or jumped); the consumer skips it.
    pub fn cancel(&self) {
        self.epoch.fetch_add(1, Ordering::Release);
    }

    pub fn len(&self) -> usize {
        self.head.load(Ordering::Acquire) - self.tail.load(Ordering::Acquire)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn events_come_out_in_their_blocks() {
        let q = Queue::new(8);
        for s in [10, 20, 130, 250] {
            assert!(unsafe { q.push(RtEvent {
                sample: s,
                kind: RtKind::Voice,
                index: 0,
                value: 1.0,
                epoch: 0,
                params: RtParams::default(),
            }) });
        }
        let mut got = vec![];
        unsafe { q.drain_before(128, |e| got.push(e.sample)) };
        assert_eq!(got, [10, 20]);
        unsafe { q.drain_before(256, |e| got.push(e.sample)) };
        assert_eq!(got, [10, 20, 130, 250]);
        assert!(q.is_empty());
    }

    #[test]
    fn full_drops_and_counts() {
        let q = Queue::new(2);
        let e = RtEvent::default();
        assert!(unsafe { q.push(e) } && unsafe { q.push(e) });
        assert!(!unsafe { q.push(e) });
        assert_eq!(q.dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn cancelled_events_are_skipped() {
        let q = Queue::new(8);
        let at = |s| RtEvent {
            sample: s,
            kind: RtKind::Voice,
            ..Default::default()
        };
        unsafe { q.push(at(10)) };
        unsafe { q.push(at(20)) };
        q.cancel();
        unsafe { q.push(at(30)) };
        let mut got = vec![];
        unsafe { q.drain_before(100, |e| got.push(e.sample)) };
        assert_eq!(got, [30]);
    }

    #[test]
    fn two_threads_lose_nothing() {
        let q = Arc::new(Queue::new(1024));
        let p = q.clone();
        let producer = std::thread::spawn(move || {
            let mut s = 0;
            while s < 100_000 {
                if unsafe { p.push(RtEvent {
                    sample: s,
                    kind: RtKind::Voice,
                    index: 0,
                    value: 0.0,
                    epoch: 0,
                    params: RtParams::default(),
                }) } {
                    s += 1;
                } else {
                    std::thread::yield_now();
                }
            }
        });
        let mut next = 0i64;
        let mut end = 0i64;
        while next < 100_000 {
            end += 64;
            unsafe { q.drain_before(end, |e| {
                assert_eq!(e.sample, next);
                next += 1;
            }) };
        }
        producer.join().unwrap();
        // Retries after a full queue aren't drops of distinct events.
        assert_eq!(next, 100_000);
    }
}
