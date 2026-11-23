//! The deck worker (one thread per deck) and its real-time consumer.
//!
//! The worker owns a [`Transport`], applies commands and keeps 43 ms (or
//! four graph cycles, at most 4,096 frames; see [`target_frames`]) rendered
//! ahead in two lock-free rings: interleaved
//! audio and the [`Tag`]s describing it. While playing it checks every 4 ms;
//! a command also wakes it. Stopped, it waits up to one second.
//!
//! The consumer runs in the engine's RT callback: it pops exactly the
//! frames the cycle needs, drops audio of superseded generations (a jump
//! takes effect on the next cycle), counts underruns and publishes the
//! source position of the next frame — the deck's position on the engine
//! clock. It never allocates, locks or blocks.

use super::pcm::Pcm;
use super::stems::{Controls, Mix, STRIDE};
use super::transport::{CHUNK, Cmd, Grid, Tag, Transport};
use crate::ring::Ring;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// The most audio kept rendered ahead (≈ 85 ms at 48 kHz); scheduled
/// starts allow for it. See [`target_frames`].
pub const TARGET_FRAMES: usize = 4096;
/// The least time kept rendered ahead.
const TARGET_SECONDS: f64 = 0.043;

/// Audio kept rendered ahead: the latency of tempo and bend changes (jumps
/// skip it: a new generation). 43 ms, or four graph cycles when those are
/// longer, at most [`TARGET_FRAMES`]. Measured (audit 2026-10-13, two
/// keylocked decks seeking every 2 s, every core busy): a deck refilled
/// within 4.8 ms at worst (p99.9 3 ms), against the 43 ms kept; until 0.7.7
/// the target was a fixed 4,096 frames, 85 ms at the usual 256-frame
/// quantum. `render_headroom_under_load` measures it again.
pub fn target_frames(rate: u32, quantum: usize) -> usize {
    let time = (rate as f64 * TARGET_SECONDS).ceil() as usize;
    time.max(4 * quantum).min(TARGET_FRAMES)
}
const RING_FRAMES: usize = 16384;
/// The headphone audition's ring (stereo frames) and its fade-in.
const AUDITION_FRAMES: usize = 8192;
const AUDITION_FADE: usize = 256;
/// How often the worker tops the buffer up (it is also woken by commands).
const POLL: Duration = Duration::from_millis(4);

/// A snapshot of the deck for status (worker side, not RT).
///
/// Text is an `Arc<str>` built when the track was loaded. Publishing a
/// snapshot clones those arcs; the render worker does not format strings.
#[derive(Clone, Debug)]
pub struct Status {
    pub loaded: bool,
    pub path: Arc<str>,
    pub title: Arc<str>,
    pub artist: Arc<str>,
    pub codec: Arc<str>,
    pub native_rate: u32,
    pub bits: u32,
    pub duration: f64,
    pub decoded: f64,
    pub decode_done: bool,
    pub decoded_audio: bool,
    pub decode_error: Option<Arc<str>>,
    pub playing: bool,
    pub scheduled_start_sample: Option<u64>,
    pub end: bool,
    pub tempo: f64,
    pub bend: f64,
    pub keylock: bool,
    pub key: i32,
    pub loop_in: Option<f64>,
    pub loop_out: Option<f64>,
    pub loop_on: bool,
    pub slip: bool,
    pub reverse: bool,
    pub censor: bool,
    pub quantize: bool,
    /// `None` means no cue point has been set; `Some(0.0)` is a cue at the very
    /// start of the track. The distinction matters to a controller's cue light.
    pub cue_point: Option<f64>,
    pub previewing: bool,
    pub grid: Option<Grid>,
}

fn blank_label() -> Arc<str> {
    static BLANK: OnceLock<Arc<str>> = OnceLock::new();
    Arc::clone(BLANK.get_or_init(|| Arc::from("")))
}

impl Default for Status {
    fn default() -> Self {
        Self {
            loaded: false,
            path: blank_label(),
            title: blank_label(),
            artist: blank_label(),
            codec: blank_label(),
            native_rate: 0,
            bits: 0,
            duration: 0.0,
            decoded: 0.0,
            decode_done: false,
            decoded_audio: false,
            decode_error: None,
            playing: false,
            scheduled_start_sample: None,
            end: false,
            tempo: 0.0,
            bend: 0.0,
            keylock: false,
            key: 0,
            loop_in: None,
            loop_out: None,
            loop_on: false,
            slip: false,
            reverse: false,
            censor: false,
            quantize: false,
            cue_point: None,
            previewing: false,
            grid: None,
        }
    }
}

/// Shared between the control thread, the worker and the RT consumer.
pub struct Shared {
    pub rate: AtomicU32,
    audio: Ring<f32>,
    pub stems: Controls,
    /// Last stem failure. The render worker only `try_lock`s this.
    pub stem_error: Mutex<Option<Arc<str>>>,
    /// Attached stem identity. The render worker only `try_lock`s this;
    /// control threads copy the `Arc` out and drop the guard before using it.
    pub stem_set: Mutex<Option<Arc<patch_core::stems::StemSet>>>,
    tags: Ring<Tag>,
    /// Tracks loaded (and ejected) so far: tells a playhead's track apart
    /// from the one before it (a jump keeps it, unlike the generation).
    pub loads: AtomicU32,
    /// The generation the worker renders now; older audio is dropped.
    generation: AtomicU32,
    /// Published by the RT consumer after every cycle: source position of
    /// the next frame (f64 bits), its step, and the generation it played.
    next_src: AtomicU64,
    step: AtomicU64,
    cursor_seq: AtomicU64,
    cursor_sample: AtomicU64,
    rt_generation: AtomicU32,
    /// The consumer's output-frame counter at `next_src` (published with
    /// it): when that frame plays.
    next_out: AtomicU64,
    /// Latest engine graph-clock sample observed by this deck's RT consumer.
    graph_sample: AtomicU64,
    /// A worker may render a future start in advance; status remains
    /// "prepared" until the shared clock reaches this sample.
    scheduled_start: AtomicU64,
    /// The RT consumer silences this source at the exact graph sample; the
    /// worker then pauses transport and retires its queued audio.
    scheduled_stop: AtomicU64,
    /// For the current generation: the output frame its first frame was
    /// meant for, and how many frames of it are rendered. The consumer
    /// keeps playing the old audio until the new is ready (no gap), then
    /// skips what it is late by (the jump lands on time, phase-exact).
    gen_out: AtomicU64,
    gen_frames: AtomicU64,
    pub underruns: AtomicU64,
    /// Underruns since the engine started (`underruns` resets on pause).
    pub underruns_total: AtomicU64,
    /// Frames kept rendered ahead; 0: [`target_frames`] (set for the
    /// headroom measurement only).
    pub target_frames: std::sync::atomic::AtomicUsize,
    /// The frames the callback takes per cycle (the graph quantum).
    pub quantum: std::sync::atomic::AtomicUsize,
    /// The fewest frames rendered ahead seen by the callback since the
    /// status last read it (usize::MAX: not playing meanwhile).
    pub fill_min: std::sync::atomic::AtomicUsize,
    /// Whether empty audio is an actual underrun condition. Transport state
    /// is mirrored here so the RT consumer never needs the worker mutex.
    pub rendering: AtomicBool,
    /// A track is loaded: the engine plays this deck instead of its port.
    pub active: AtomicBool,
    /// The channel's input selector is on LINE: the engine plays the
    /// channel's port (an application routed into the deck's soundcard)
    /// and the track, if any, runs on unheard, as a CDJ does behind a DJM
    /// set to LINE. Loading a track puts the channel back on its deck.
    pub line: AtomicBool,
    /// Headphone audition (touch preview, previewed hot cues): interleaved
    /// stereo the worker reads from the track at a second playhead, which
    /// the callback adds to the headphones only — the program never hears
    /// it. While `audition_flush` is set the callback drops what is queued
    /// (fading it out) and the worker queues nothing, so old and new audio
    /// never mix and neither side waits.
    audition: Ring<f32>,
    audition_flush: AtomicBool,
    /// An audition is playing (for status).
    pub auditioning: AtomicBool,
    cmds: Mutex<VecDeque<Cmd>>,
    pub status: Mutex<Status>,
    thread: OnceLock<std::thread::Thread>,
}

fn f64_load(a: &AtomicU64) -> f64 {
    f64::from_bits(a.load(Ordering::Acquire))
}

impl Shared {
    pub fn new(rate: u32) -> Arc<Shared> {
        Arc::new(Shared {
            rate: AtomicU32::new(rate),
            audio: Ring::new(RING_FRAMES * STRIDE, 0.0),
            stems: Controls::default(),
            stem_error: Mutex::new(None),
            stem_set: Mutex::new(None),
            tags: Ring::new(1024, Tag::default()),
            loads: AtomicU32::new(0),
            generation: AtomicU32::new(0),
            next_src: AtomicU64::new(0f64.to_bits()),
            step: AtomicU64::new(0f64.to_bits()),
            cursor_seq: AtomicU64::new(0),
            cursor_sample: AtomicU64::new(0),
            rt_generation: AtomicU32::new(0),
            next_out: AtomicU64::new(0),
            graph_sample: AtomicU64::new(0),
            scheduled_start: AtomicU64::new(u64::MAX),
            scheduled_stop: AtomicU64::new(u64::MAX),
            gen_out: AtomicU64::new(0),
            gen_frames: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            underruns_total: AtomicU64::new(0),
            target_frames: std::sync::atomic::AtomicUsize::new(0),
            // Until the first cycle: the largest the target may need.
            quantum: std::sync::atomic::AtomicUsize::new(TARGET_FRAMES / 4),
            fill_min: std::sync::atomic::AtomicUsize::new(usize::MAX),
            rendering: AtomicBool::new(false),
            active: AtomicBool::new(false),
            line: AtomicBool::new(false),
            audition: Ring::new(AUDITION_FRAMES * 2, 0.0),
            audition_flush: AtomicBool::new(false),
            auditioning: AtomicBool::new(false),
            cmds: Mutex::default(),
            status: Mutex::default(),
            thread: OnceLock::new(),
        })
    }

    /// Queue a command for the worker and wake it. A track or stems handed
    /// to the deck are locked in RAM first, here on the sender's thread:
    /// only what a deck plays spends the lock budget, so a rehearsal's
    /// stems can't leave the deck's own swappable (a swapped page stalls the
    /// worker: underruns, 2026-10-26).
    pub fn send(&self, cmd: Cmd) {
        match &cmd {
            Cmd::Load(pcm) if pcm.done.load(Ordering::Acquire) => pcm.keep_resident(),
            Cmd::AttachStems(audio) => audio.sources.iter().for_each(|s| s.keep_resident()),
            _ => {}
        }
        self.cmds.lock().unwrap().push_back(cmd);
        self.wake();
    }

    fn wake(&self) {
        if let Some(t) = self.thread.get() {
            t.unpark();
        }
    }

    /// The playhead on the engine clock: source position (frames at the
    /// graph rate) of the next frame the engine plays, and source frames
    /// per output frame (0 when stopped).
    pub fn playhead(&self) -> (f64, f64) {
        (f64_load(&self.next_src), f64_load(&self.step))
    }

    /// Coherent transport cursor paired with its absolute engine sample.
    /// Control-plane callers use this to schedule work against a live deck.
    pub fn timed_playhead(&self) -> (f64, f64, u64) {
        loop {
            let before = self.cursor_seq.load(Ordering::Acquire);
            if before & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let frame = f64_load(&self.next_src);
            let step = f64_load(&self.step);
            let sample = self.cursor_sample.load(Ordering::Relaxed);
            let after = self.cursor_seq.load(Ordering::Acquire);
            if before == after {
                return (frame, step, sample);
            }
        }
    }

    /// Frames rendered and waiting for the RT consumer. Tests that drive
    /// `Rt::run` faster than real time pace themselves on it, so a slow
    /// (debug) worker shows up as a wait, not as an underrun.
    #[cfg(test)]
    pub(crate) fn buffered_frames(&self) -> usize {
        self.audio.len() / STRIDE
    }

    /// Output-clock position used to align scheduled starts across workers.
    pub fn output_sample(&self) -> u64 {
        self.next_out.load(Ordering::Acquire)
    }

    pub fn graph_sample(&self) -> u64 {
        self.graph_sample.load(Ordering::Acquire)
    }

    pub fn publish_graph_sample(&self, sample: u64) {
        self.graph_sample.store(sample, Ordering::Release);
    }

    /// Start the worker thread.
    pub fn spawn(self: &Arc<Self>, name: &str) {
        let sh = self.clone();
        let handle = std::thread::Builder::new()
            .name(format!("patch-deck-{name}"))
            .spawn(move || {
                render_priority();
                run(sh)
            })
            .expect("deck worker");
        let _ = self.thread.set(handle.thread().clone());
    }
}

/// Deck rendering keeps about 85 ms ahead of the audio callback. Analysis,
/// rehearsals and stem separation load every core at times; at normal
/// priority a deck could fall behind and the music stutter. Its work is
/// bounded per chunk and it sleeps on a full ring, so it runs real-time,
/// below the audio callback (FIFO 83): FIFO 70, else the highest nice level
/// this user may take.
fn render_priority() {
    // Denormals flushed: the resampler and stretcher states decay at the end
    // of a track or a stem's silence.
    patch_dsp::cpu::flush_denormals();
    let param = libc::sched_param { sched_priority: 70 };
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

/// Expand stereo into the fixed stem bundle, preserving unused lanes as zero.
/// # Safety
/// Only the deck's render worker (the ring's producer) may call this.
unsafe fn push_stereo(
    audio: &Ring<f32>,
    scratch: &mut [f32; CHUNK * STRIDE],
    samples: &[f32],
) -> bool {
    debug_assert_eq!(samples.len() % 2, 0);
    let n = samples.len() / 2 * STRIDE;
    for (frame, bundle) in samples
        .chunks_exact(2)
        .zip(scratch[..n].chunks_exact_mut(STRIDE))
    {
        bundle[..2].copy_from_slice(frame);
    }
    // SAFETY: the caller is the producer (this fn's contract).
    unsafe { audio.push_slice(&scratch[..n]) }
}

/// A stem-identity update the worker still has to publish. `try_lock` only:
/// this thread is `SCHED_FIFO` and must not wait on a mutex a control thread holds.
struct StemPublish {
    /// `Some(None)` clears the set; `Some(Some(set))` publishes it.
    action: Option<Option<Arc<patch_core::stems::StemSet>>>,
    /// A load or eject still has to clear `stem_error`. Kept apart from
    /// `action` so a following attach replaces the set without forgetting
    /// the error clear that did not get the lock.
    clear_error: bool,
    arm_ready: bool,
}

impl StemPublish {
    fn idle() -> Self {
        Self {
            action: None,
            clear_error: false,
            arm_ready: false,
        }
    }

    fn pending(&self) -> bool {
        self.action.is_some() || self.clear_error
    }

    fn flush(&mut self, sh: &Shared) {
        if self.clear_error {
            let Some(mut error) = try_lock(&sh.stem_error) else {
                return;
            };
            *error = None;
            drop(error);
            self.clear_error = false;
        }
        let Some(action) = self.action.take() else {
            return;
        };
        match action {
            None => {
                let Some(mut set) = try_lock(&sh.stem_set) else {
                    self.action = Some(None);
                    return;
                };
                *set = None;
            }
            Some(set) => {
                let Some(mut slot) = try_lock(&sh.stem_set) else {
                    self.action = Some(Some(set));
                    return;
                };
                *slot = Some(set);
                drop(slot);
                if self.arm_ready {
                    sh.stems.ready.store(true, Ordering::Release);
                    self.arm_ready = false;
                }
            }
        }
    }
}

/// `None` when another thread holds the mutex. A poisoned mutex is fatal,
/// the same as `lock().unwrap()` on the control threads.
fn try_lock<T>(mutex: &Mutex<T>) -> Option<std::sync::MutexGuard<'_, T>> {
    match mutex.try_lock() {
        Ok(guard) => Some(guard),
        Err(std::sync::TryLockError::WouldBlock) => None,
        Err(std::sync::TryLockError::Poisoned(_)) => panic!("stem lock poisoned"),
    }
}

fn run(sh: Arc<Shared>) {
    let mut stereo_bundle = [0.0f32; CHUNK * STRIDE];
    let mut t = Transport::new(sh.rate.load(Ordering::Relaxed));
    let mut restart_pos = 0.0;
    let mut restart_out = 0u64;
    // Frames rendered in the current generation. After a jump the ring
    // still holds the old generation's audio (dropped by the consumer), so
    // a full buffer of the new one is rendered regardless of the fill.
    let mut fresh = 0usize;
    let mut status_at: Option<std::time::Instant> = None;
    let mut status_playing = false;
    let mut status_loaded = false;
    // A snapshot not yet published (a reader held the lock).
    let mut unpublished: Option<Status> = None;
    // Reused across turns. A command burst allocates once; an empty turn does not.
    let mut cmds: Vec<Cmd> = Vec::new();
    let mut stem_publish = StemPublish::idle();
    let mut cached = Status::default();
    // The audition's next frame, and how far its fade-in has come.
    let mut audition: Option<usize> = None;
    let mut audition_faded = 0usize;
    let mut audition_buf = vec![0.0f32; CHUNK * 2];
    loop {
        // This thread is real-time (FIFO): it never waits on a lock other
        // threads hold. Commands a sender is still pushing are taken next
        // turn (the sender unparks this thread after pushing). Stem identity
        // uses the same rule: a held `stem_set` / `stem_error` is retried.
        stem_publish.flush(&sh);
        cmds.clear();
        if let Ok(mut queued) = sh.cmds.try_lock() {
            cmds.extend(queued.drain(..));
        }
        let stop_at = sh.scheduled_stop.load(Ordering::Acquire);
        if stop_at != u64::MAX
            && sh.graph_sample.load(Ordering::Acquire) >= stop_at
            && sh
                .scheduled_stop
                .compare_exchange(stop_at, u64::MAX, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            cmds.push(Cmd::Pause);
        }
        let changed = !cmds.is_empty();
        for c in cmds.drain(..) {
            if let Cmd::Audition(at) = &c {
                sh.audition_flush.store(true, Ordering::Release);
                audition = at
                    .filter(|s| s.is_finite() && *s >= 0.0 && t.pcm.is_some())
                    .map(|s| (s * t.rate as f64) as usize);
                audition_faded = 0;
                continue;
            }
            let (c, scheduled_graph_sample) = match c {
                Cmd::PlayAt(at) => {
                    sh.scheduled_start.store(at, Ordering::Release);
                    (Cmd::Play, Some(at))
                }
                Cmd::PauseAt(at) => {
                    sh.scheduled_stop.store(at, Ordering::Release);
                    continue;
                }
                other => {
                    if matches!(
                        &other,
                        Cmd::Play
                            | Cmd::Pause
                            | Cmd::Toggle
                            | Cmd::Load(_)
                            | Cmd::Eject
                            | Cmd::Seek(_)
                    ) {
                        sh.scheduled_start.store(u64::MAX, Ordering::Release);
                    }
                    if matches!(
                        &other,
                        Cmd::Play
                            | Cmd::Pause
                            | Cmd::Toggle
                            | Cmd::Load(_)
                            | Cmd::Eject
                            | Cmd::Seek(_)
                    ) {
                        sh.scheduled_stop.store(u64::MAX, Ordering::Release);
                    }
                    (other, None)
                }
            };
            if let Cmd::Load(p) = &c {
                t.rate = p.rate;
            }
            match &c {
                Cmd::Load(_) | Cmd::Eject => {
                    if matches!(c, Cmd::Load(_)) {
                        sh.line.store(false, Ordering::Release);
                    }
                    if audition.take().is_some() {
                        sh.audition_flush.store(true, Ordering::Release);
                    }
                    sh.loads.fetch_add(1, Ordering::Release);
                    sh.stems.reset();
                    sh.stems.ready.store(false, Ordering::Release);
                    stem_publish.arm_ready = false;
                    stem_publish.clear_error = true;
                    stem_publish.action = Some(None);
                    stem_publish.flush(&sh);
                }
                Cmd::AttachStems(audio) => {
                    if t.pcm.as_ref().is_none_or(|p| p.path != audio.source_path)
                        || sh.stems.set_layout(&audio.ids).is_err()
                    {
                        continue;
                    }
                    stem_publish.action = Some(Some(Arc::new(audio.set.clone())));
                    stem_publish.arm_ready = true;
                    stem_publish.flush(&sh);
                }
                _ => {}
            }
            // "Now" is what the engine plays next — unless a restart is
            // still in flight, then it's where that restart begins.
            let (now, now_out) = if sh.rt_generation.load(Ordering::Acquire) == t.generation {
                (sh.playhead().0, sh.next_out.load(Ordering::Acquire))
            } else {
                (restart_pos, restart_out)
            };
            let before = t.generation;
            let was_stopped = !(t.playing || t.previewing);
            t.apply(c, Some(now));
            let rendering = t.playing || t.previewing;
            sh.rendering.store(rendering, Ordering::Release);
            if !rendering {
                // Pause leaves the consumer's last transport step published
                // until its next cycle. Clear it and its counter: an idle
                // ring must never create thousands of phantom underruns.
                sh.step.store(0.0f64.to_bits(), Ordering::Release);
                sh.underruns.store(0, Ordering::Release);
            }
            if t.generation != before {
                restart_pos = t.pos;
                // A stopped deck has nothing scheduled, so it is never late:
                // it starts from the output frame the engine is on now, not
                // from a restart that no cycle has taken up yet. After an
                // eject that one stays open while the graph runs on, and the
                // next start skipped all of it (8 s of a new track).
                let out = if was_stopped {
                    sh.next_out.load(Ordering::Acquire)
                } else {
                    now_out
                };
                // `next_out` is output frame number `cursor_sample` on the
                // graph clock: a start at graph sample `at` is that far on.
                restart_out = scheduled_graph_sample.map_or(out, |at| {
                    out.saturating_add(at.saturating_sub(sh.cursor_sample.load(Ordering::Acquire)))
                });
                // Stopped, the place is the new one at once. With the graph
                // asleep no cycle publishes it, and a track just loaded read
                // as the old track's position until it played.
                if !t.playing && !t.previewing {
                    sh.next_src.store(t.pos.to_bits(), Ordering::Release);
                }
                fresh = 0;
                // A stopped deck renders nothing: nothing to wait for, the
                // consumer switches at once (a pause is immediate).
                sh.gen_frames
                    .store(if t.playing { 0 } else { u64::MAX >> 1 }, Ordering::Release);
                sh.gen_out.store(restart_out, Ordering::Release);
                sh.generation.store(t.generation, Ordering::Release);
            }
        }
        sh.active.store(t.pcm.is_some(), Ordering::Release);
        // Render ahead while playing. Stopped, one silent chunk after a
        // change (it carries the new position to the engine), then nothing:
        // a paused deck rendered silence and rebuilt its status 250 times a
        // second (6 % of a core).
        let playing = t.playing;
        let target = match sh.target_frames.load(Ordering::Relaxed) {
            0 => target_frames(t.rate, sh.quantum.load(Ordering::Relaxed)),
            set => set,
        };
        while (playing && (sh.audio.len() / STRIDE < target || fresh < target)
            || !playing && fresh < CHUNK)
            && sh.audio.free() >= CHUNK * STRIDE + 64
            && sh.tags.free() >= 8
        {
            // SAFETY (the pushes below): this render worker is the only
            // producer of its deck's audio and tag rings.
            t.render(&mut |tag, a| {
                if tag.bundle {
                    unsafe { sh.audio.push_slice(a) };
                } else {
                    unsafe { push_stereo(&sh.audio, &mut stereo_bundle, a) };
                }
                unsafe { sh.tags.push(tag) };
                sh.gen_frames
                    .fetch_add(tag.frames as u64, Ordering::Release);
            });
            fresh = fresh.saturating_add(CHUNK);
        }
        // The audition, once the callback has dropped the previous one.
        if let (Some(mut pos), Some(pcm)) = (audition, t.pcm.as_ref())
            && !sh.audition_flush.load(Ordering::Acquire)
        {
            let end = pcm.length();
            while pos < end && sh.audition.len() / 2 < target && sh.audition.free() >= CHUNK * 2 {
                pcm.read(pos, &mut audition_buf);
                if audition_faded < AUDITION_FADE {
                    for (f, frame) in audition_buf.chunks_exact_mut(2).enumerate() {
                        let g = ((audition_faded + f) as f32 / AUDITION_FADE as f32).min(1.0);
                        frame[0] *= g;
                        frame[1] *= g;
                    }
                    audition_faded += CHUNK;
                }
                // SAFETY: this worker is the audition ring's only producer.
                unsafe { sh.audition.push_slice(&audition_buf) };
                pos += CHUNK;
            }
            audition = (pos < end).then_some(pos);
        }
        sh.auditioning.store(audition.is_some(), Ordering::Release);
        let rendering = t.playing || t.previewing;
        sh.rendering.store(rendering, Ordering::Release);
        if !rendering {
            sh.step.store(0.0f64.to_bits(), Ordering::Release);
            sh.underruns.store(0, Ordering::Release);
        }
        // Status after a command, a start or stop (the track's end stops it
        // inside render) or a track coming or going (a load finishing or
        // failing), 20 times a second while playing, once a second else:
        // never stale.
        let loaded = t.pcm.is_some();
        let scheduled_start = sh.scheduled_start.load(Ordering::Acquire);
        let audible_playing = t.playing
            && (scheduled_start == u64::MAX
                || sh.graph_sample.load(Ordering::Acquire) >= scheduled_start);
        let due = status_at.is_none_or(|at| {
            at.elapsed() >= Duration::from_millis(if t.playing { 50 } else { 1000 })
        });
        if changed || audible_playing != status_playing || loaded != status_loaded || due {
            refresh_status(&mut cached, &t, &sh);
            status_playing = cached.playing;
            unpublished = Some(cached.clone());
            status_at = Some(std::time::Instant::now());
            status_loaded = loaded;
        }
        // Published without waiting: a reader holding the status lock (it
        // may be building a reply) defers it to the next turn.
        if unpublished.is_some()
            && let Ok(mut published) = sh.status.try_lock()
        {
            *published = unpublished.take().unwrap();
        }
        // Retry a stem publish that lost the try-lock during render, before sleeping.
        stem_publish.flush(&sh);
        // A stopped deck sleeps until a command wakes it (send() unparks).
        // A stem identity still waiting on a control-thread lock polls, so
        // the publish is not stuck behind the one-second idle sleep.
        std::thread::park_timeout(
            if playing || audition.is_some() || unpublished.is_some() || stem_publish.pending() {
                POLL
            } else {
                Duration::from_secs(1)
            },
        );
    }
}

fn refresh_status(dst: &mut Status, t: &Transport, sh: &Shared) {
    let r = t.rate as f64;
    let p: Option<&Arc<Pcm>> = t.pcm.as_ref();
    let scheduled = sh.scheduled_start.load(Ordering::Acquire);
    let current_sample = sh.graph_sample.load(Ordering::Acquire);
    let scheduled_start_sample = if scheduled != u64::MAX && scheduled > current_sample {
        Some(scheduled)
    } else {
        if scheduled != u64::MAX {
            let _ = sh.scheduled_start.compare_exchange(
                scheduled,
                u64::MAX,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
        None
    };
    dst.loaded = p.is_some();
    match p {
        Some(p) => {
            // Clones of arcs the loader built. Same track: refcount only.
            dst.path = Arc::clone(&p.labels.path);
            dst.title = Arc::clone(&p.labels.title);
            dst.artist = Arc::clone(&p.labels.artist);
            dst.codec = Arc::clone(&p.labels.codec);
            dst.native_rate = p.info.rate;
            dst.bits = p.info.bits;
            dst.duration = if p.done.load(Ordering::Acquire) {
                p.frames() as f64 / r
            } else {
                p.info.duration
            };
            dst.decoded = if p.done.load(Ordering::Acquire) {
                1.0
            } else {
                p.frames() as f64 / p.capacity_frames().max(1) as f64
            };
            dst.decode_done = p.done.load(Ordering::Acquire);
            dst.decoded_audio = p.frames() > 0;
            dst.decode_error = match p.error.try_lock() {
                Ok(error) => error.clone(),
                Err(std::sync::TryLockError::WouldBlock) => dst.decode_error.clone(),
                Err(std::sync::TryLockError::Poisoned(_)) => panic!("pcm error lock poisoned"),
            };
        }
        None => {
            dst.path = blank_label();
            dst.title = blank_label();
            dst.artist = blank_label();
            dst.codec = blank_label();
            dst.native_rate = 0;
            dst.bits = 0;
            dst.duration = 0.0;
            dst.decoded = 0.0;
            dst.decode_done = true;
            dst.decoded_audio = false;
            dst.decode_error = None;
        }
    }
    dst.playing = t.playing && scheduled_start_sample.is_none();
    dst.scheduled_start_sample = scheduled_start_sample;
    dst.end = t.end;
    dst.tempo = t.tempo;
    dst.bend = t.bend;
    dst.keylock = t.keylock;
    dst.key = t.key;
    dst.loop_in = t.looping.map(|(a, _)| a / r);
    dst.loop_out = t.looping.map(|(_, b)| b / r);
    dst.loop_on = t.loop_on;
    dst.slip = t.slip;
    dst.reverse = t.reverse;
    dst.censor = t.censor_slip.is_some();
    dst.quantize = t.quantize;
    dst.cue_point = t.cue_point.map(|c| c / r);
    dst.previewing = t.previewing;
    dst.grid = t.grid;
}

/// RT side of one deck: the tag being played and how far into it.
pub struct Consumer {
    cur: Option<Tag>,
    used: u32,
    scratch: Vec<f32>,
    /// Output frames played so far (the clock restarts are timed on).
    played: u64,
    /// The generation whose start has been timed (late frames skipped).
    started: u32,
    /// Switching to this generation: old audio is being dropped.
    switching: Option<u32>,
    /// Late frames of the new generation still to skip, and the fade-in
    /// frames left at a switch (click-free).
    skip: u64,
    fade: u32,
    /// The generation actually playing (published for the worker).
    playing: u32,
    mix: Mix,
    pub graph_sample: u64,
    /// Final per-deck safety guard after the stem summer. Stems can sum to a
    /// higher peak than the original mix, so this guard also covers the
    /// multichannel stretcher path while stem controls are bypassed.
    peak_gain: f32,
    peak_release: f32,
    peak_rate: u32,
    /// At a restart, the old audio's next frames (mixed, before the guard):
    /// they fade out while the new audio fades in, so a jump is a short
    /// crossfade rather than a cut to silence.
    tail: [[f32; 4]; SWITCH_FADE as usize],
    tail_len: usize,
    tail_at: usize,
}

/// Crossfade at a restart: 256 frames (5.3 ms at 48 kHz), equal power.
const SWITCH_FADE: u32 = 256;
const DECK_CEILING: f32 = 0.94406086; // −0.5 dBFS

fn guard_frame(gain: &mut f32, release: f32, mut frame: [f32; 4]) -> [f32; 4] {
    let peak = frame.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
    let required = if peak > DECK_CEILING {
        DECK_CEILING / peak
    } else {
        1.0
    };
    let recovered = (*gain + (1.0 - *gain) * release).min(1.0);
    let recovered = if 1.0 - recovered < 2e-4 {
        1.0
    } else {
        recovered
    };
    *gain = required.min(recovered);
    for x in &mut frame {
        *x *= *gain;
    }
    frame
}

impl Default for Consumer {
    fn default() -> Self {
        Self::new()
    }
}

impl Consumer {
    pub fn new() -> Consumer {
        Consumer {
            cur: None,
            used: 0,
            played: 0,
            started: 0,
            switching: None,
            skip: 0,
            fade: 0,
            playing: 0,
            scratch: vec![0.0; 8192 * STRIDE],
            mix: Mix::default(),
            graph_sample: 0,
            peak_gain: 1.0,
            peak_release: 0.0,
            peak_rate: 0,
            tail: [[0.0; 4]; SWITCH_FADE as usize],
            tail_len: 0,
            tail_at: 0,
        }
    }

    /// Add the deck's headphone audition to `l`/`r` (the cue sum; `heard`
    /// false: no headphones, it is only dropped). A flush fades out what
    /// this cycle had and drops the rest. RT-safe: no allocation, no locks.
    pub fn audition(&mut self, sh: &Shared, l: &mut [f32], r: &mut [f32], heard: bool) {
        let flush = sh.audition_flush.load(Ordering::Acquire);
        let n = l.len().min(r.len()).min(sh.audition.len() / 2).min(self.scratch.len() / 2);
        if n > 0 {
            // SAFETY: the RT callback is the audition ring's only consumer.
            let got = unsafe { sh.audition.pop_into(&mut self.scratch[..n * 2]) } / 2;
            if heard {
                for i in 0..got {
                    let g = if flush { 1.0 - i as f32 / got as f32 } else { 1.0 };
                    l[i] += self.scratch[i * 2] * g;
                    r[i] += self.scratch[i * 2 + 1] * g;
                }
            }
        }
        if flush {
            // SAFETY: as above.
            unsafe { sh.audition.skip(usize::MAX) };
            sh.audition_flush.store(false, Ordering::Release);
        }
    }

    /// Fill `l`/`r` with the deck's next frames. Returns the source position
    /// of the first frame (the deck's position for this cycle's stamp) and
    /// its step. RT-safe: no allocation, no locks.
    pub fn pull(&mut self, sh: &Shared, l: &mut [f32], r: &mut [f32]) -> (f64, f64) {
        self.pull_with_stem(sh, l, r, None, None, None)
    }

    /// Fill the deck and (optionally) one isolated, already gain-controlled
    /// stem send. Both outputs use this exact transport sample mapping.
    /// A cycle with no track on the deck: nothing plays, but its clock
    /// moves on with the graph's, so a track loaded now and scheduled for
    /// a later graph sample starts exactly then.
    pub fn idle(&mut self, sh: &Shared, graph_sample: u64, n: usize) {
        self.graph_sample = graph_sample;
        self.played += n as u64;
        sh.publish_graph_sample(graph_sample);
        let cursor_seq = sh.cursor_seq.load(Ordering::Relaxed);
        sh.cursor_seq
            .store(cursor_seq.wrapping_add(1), Ordering::Release);
        sh.cursor_sample
            .store(graph_sample.saturating_add(n as u64), Ordering::Relaxed);
        sh.cursor_seq
            .store(cursor_seq.wrapping_add(2), Ordering::Release);
        sh.next_out.store(self.played, Ordering::Release);
    }

    /// The old audio's next frame at a restart, faded out (equal power over
    /// what was kept); silence once it is spent.
    fn next_tail(&mut self) -> [f32; 4] {
        if self.tail_at >= self.tail_len {
            return [0.0; 4];
        }
        let x = (self.tail_at + 1) as f32 / self.tail_len as f32;
        let g = (x * std::f32::consts::FRAC_PI_2).cos();
        let t = self.tail[self.tail_at].map(|v| v * g);
        self.tail_at += 1;
        t
    }

    pub fn pull_with_stem(
        &mut self,
        sh: &Shared,
        l: &mut [f32],
        r: &mut [f32],
        sl: Option<&mut [f32]>,
        sr: Option<&mut [f32]>,
        send: Option<usize>,
    ) -> (f64, f64) {
        let n = l.len().min(r.len()).min(self.scratch.len() / STRIDE);
        sh.quantum.store(n, Ordering::Relaxed);
        if sh.rendering.load(Ordering::Acquire) {
            sh.fill_min
                .fetch_min(sh.audio.len() / STRIDE, Ordering::Relaxed);
        }
        let rate = sh.rate.load(Ordering::Acquire).max(1);
        if rate != self.peak_rate {
            self.peak_rate = rate;
            self.peak_release = (1.0 - (-1.0 / (0.080 * rate as f32)).exp()).clamp(0.0, 1.0);
        }
        let mut sl = sl;
        let mut sr = sr;
        if let Some(x) = sl.as_deref_mut() {
            let m = n.min(x.len());
            x[..m].fill(0.0);
        }
        if let Some(x) = sr.as_deref_mut() {
            let m = n.min(x.len());
            x[..m].fill(0.0);
        }
        let want = sh.generation.load(Ordering::Acquire);
        let ready = sh.gen_frames.load(Ordering::Acquire);
        let gen_out = sh.gen_out.load(Ordering::Acquire);
        let mut done = 0usize;
        let mut first: Option<(f64, f64)> = None;
        let mut next = sh.playhead();
        // Audio taken from the worker this cycle (it refills: wake it).
        let mut took = false;
        while done < n {
            if self.cur.is_none_or(|t| self.used >= t.frames) {
                // SAFETY (pops and skips in this fn): the engine callback
                // is the only consumer of the deck's audio and tag rings.
                match unsafe { sh.tags.pop() } {
                    Some(t) => {
                        took = true;
                        self.cur = Some(t);
                        self.used = 0;
                    }
                    None => {
                        // Worker behind: silence, counted while playing
                        // (a paused deck renders nothing, by design).
                        l[done..n].fill(0.0);
                        r[done..n].fill(0.0);
                        if let Some(x) = sl.as_deref_mut() {
                            x[done..n].fill(0.0);
                        }
                        if let Some(x) = sr.as_deref_mut() {
                            x[done..n].fill(0.0);
                        }
                        if sh.rendering.load(Ordering::Acquire) && f64_load(&sh.step) > 0.0 {
                            sh.underruns.fetch_add(1, Ordering::Relaxed);
                            sh.underruns_total.fetch_add(1, Ordering::Relaxed);
                        }
                        break;
                    }
                }
            }
            let tag = self.cur.unwrap();
            if tag.generation != want {
                // A restart (jump, cue, loop, play) is pending. Keep playing
                // the old audio until the new one covers the rest of this
                // cycle plus what it is late by; then drop the old for good.
                let late = (self.played + done as u64).saturating_sub(gen_out);
                if self.switching != Some(want) && ready < late + (n - done) as u64 {
                    // fall through: play the old audio
                } else {
                    let left = (tag.frames - self.used) as usize;
                    let mut kept = 0;
                    if self.switching != Some(want) {
                        // Keep the old audio's next frames to fade out.
                        kept = left.min(SWITCH_FADE as usize);
                        kept = unsafe { sh.audio.pop_into(&mut self.scratch[..kept * STRIDE]) }
                            / STRIDE;
                        for i in 0..kept {
                            self.tail[i] = self.mix.frame_with_send(
                                &sh.stems,
                                &self.scratch[i * STRIDE..(i + 1) * STRIDE],
                                tag.bundle,
                                self.graph_sample + (done + i) as u64,
                                send,
                            );
                        }
                        (self.tail_len, self.tail_at) = (kept, 0);
                    }
                    self.switching = Some(want);
                    unsafe { sh.audio.skip((left - kept) * STRIDE) };
                    self.cur = None;
                    continue;
                }
            } else if self.started != want {
                // The new generation's first frame: it was meant for output
                // frame `gen_out`; skip what we're late by, fade it in.
                let before = gen_out.saturating_sub(self.played + (done as u64));
                if before > 0 {
                    // A start later on: the old audio's tail fades out first.
                    let silence = (before as usize).min(n - done);
                    for i in done..done + silence {
                        let t = self.next_tail();
                        l[i] = t[0];
                        r[i] = t[1];
                        if let Some(x) = sl.as_deref_mut()
                            && i < x.len()
                        {
                            x[i] = t[2];
                        }
                        if let Some(x) = sr.as_deref_mut()
                            && i < x.len()
                        {
                            x[i] = t[3];
                        }
                    }
                    done += silence;
                    if done == n {
                        break;
                    }
                    continue;
                }
                self.started = want;
                self.switching = None;
                self.skip = (self.played + done as u64).saturating_sub(gen_out);
                self.fade = SWITCH_FADE;
            }
            if tag.generation == want && self.skip > 0 {
                let k = ((tag.frames - self.used) as u64).min(self.skip) as u32;
                unsafe { sh.audio.skip(k as usize * STRIDE) };
                self.used += k;
                self.skip -= k as u64;
                // Where the skipped frames lead is the deck's place now: a
                // stopped deck's seek is one 256-frame chunk, and a skip of a
                // cycle swallowed it whole, so the old place stayed published
                // and play started from there (every other seek was lost).
                next = (tag.src + self.used as f64 * tag.step, tag.step);
                continue;
            }
            self.playing = tag.generation;
            let k = ((tag.frames - self.used) as usize).min(n - done);
            let got = unsafe { sh.audio.pop_into(&mut self.scratch[..k * STRIDE]) } / STRIDE;
            for i in 0..got {
                let g = if self.fade > 0 {
                    let x = (SWITCH_FADE - self.fade + 1) as f32 / SWITCH_FADE as f32;
                    self.fade -= 1;
                    (x * std::f32::consts::FRAC_PI_2).sin()
                } else {
                    1.0
                };
                let mut frame = self.mix.frame_with_send(
                    &sh.stems,
                    &self.scratch[i * STRIDE..(i + 1) * STRIDE],
                    tag.bundle,
                    self.graph_sample + (done + i) as u64,
                    send,
                );
                if g < 1.0 {
                    let t = self.next_tail();
                    for (x, o) in frame.iter_mut().zip(t) {
                        *x = *x * g + o;
                    }
                }
                if self.graph_sample + (done + i) as u64
                    >= sh.scheduled_stop.load(Ordering::Acquire)
                {
                    frame = [0.0; 4];
                }
                let [ol, or, bl, br] = guard_frame(&mut self.peak_gain, self.peak_release, frame);
                l[done + i] = ol;
                r[done + i] = or;
                if let Some(x) = sl.as_deref_mut()
                    && done + i < x.len()
                {
                    x[done + i] = bl;
                }
                if let Some(x) = sr.as_deref_mut()
                    && done + i < x.len()
                {
                    x[done + i] = br;
                }
            }
            let at = tag.src + self.used as f64 * tag.step;
            if first.is_none() {
                first = Some((at, tag.step));
            }
            self.used += k as u32;
            done += k;
            next = (tag.src + self.used as f64 * tag.step, tag.step);
        }
        self.played += n as u64;
        let cursor_seq = sh.cursor_seq.load(Ordering::Relaxed);
        sh.cursor_seq
            .store(cursor_seq.wrapping_add(1), Ordering::Release);
        sh.next_src.store(next.0.to_bits(), Ordering::Relaxed);
        sh.step.store(next.1.to_bits(), Ordering::Relaxed);
        sh.cursor_sample.store(
            self.graph_sample.saturating_add(n as u64),
            Ordering::Relaxed,
        );
        sh.cursor_seq
            .store(cursor_seq.wrapping_add(2), Ordering::Release);
        sh.next_out.store(self.played, Ordering::Release);
        // The generation really playing: the worker times the next command
        // from it (a restart still in flight uses its own start instead).
        let current = self.started == want || self.switching == Some(want);
        sh.rt_generation
            .store(if current { want } else { self.playing }, Ordering::Release);
        // Only when it has something to refill: a paused deck's worker was
        // woken every cycle for nothing.
        if took {
            sh.wake();
        }
        first.unwrap_or(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batched_stereo_preserves_lanes_segments_and_ring_wrap() {
        let ring = Ring::new(CHUNK * STRIDE, 0.0);
        let mut scratch = [0.0; CHUNK * STRIDE];
        for frames in [CHUNK, 17, 1, CHUNK - 17, CHUNK] {
            let samples: Vec<f32> = (0..frames * 2).map(|i| i as f32 - 12.0).collect();
            assert!(unsafe { push_stereo(&ring, &mut scratch, &samples) });
            let mut got = vec![0.0; frames * STRIDE];
            assert_eq!(unsafe { ring.pop_into(&mut got) }, got.len());
            for (i, bundle) in got.chunks_exact(STRIDE).enumerate() {
                assert_eq!(&bundle[..2], &samples[i * 2..i * 2 + 2]);
                assert!(bundle[2..].iter().all(|x| *x == 0.0));
            }
        }
        assert_eq!(ring.dropped.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn the_render_target_is_43_ms_or_four_cycles_never_more_than_before() {
        assert_eq!(target_frames(48_000, 256), 2064);
        assert_eq!(target_frames(44_100, 128), 1897);
        assert_eq!(target_frames(48_000, 1024), 4096);
        assert_eq!(target_frames(96_000, 2048), TARGET_FRAMES);
    }

    /// S2 (audit 2026-10-13): how close a playing deck's render buffer comes
    /// to empty under load, i.e. how much of the 4,096-frame target is used.
    /// `PATCH_TEST_TRACK=<file> PATCH_HEADROOM_SECONDS=60
    /// PATCH_HEADROOM_LOAD=<busy threads> cargo test --release -p patch-engine
    /// render_headroom -- --ignored --nocapture`
    #[test]
    #[ignore = "manual performance measurement"]
    fn render_headroom_under_load() {
        let env = |k: &str, d: usize| {
            std::env::var(k)
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(d)
        };
        let path = std::env::var("PATCH_TEST_TRACK").expect("PATCH_TEST_TRACK");
        let seconds = env("PATCH_HEADROOM_SECONDS", 30) as u64;
        let busy = env("PATCH_HEADROOM_LOAD", 0);
        let quantum = env("PATCH_HEADROOM_QUANTUM", 256);
        let target = env("PATCH_HEADROOM_TARGET", target_frames(48_000, quantum));
        let decks = env("PATCH_HEADROOM_DECKS", 1).max(1);
        let rate = 48_000u32;
        let pcm =
            super::super::pcm::Pcm::load_full(std::path::Path::new(&path), rate, 1.0).unwrap();
        let shared: Vec<Arc<Shared>> = (0..decks)
            .map(|d| {
                let sh = Shared::new(rate);
                sh.target_frames.store(target, Ordering::Relaxed);
                sh.spawn(&format!("headroom-{d}"));
                sh.send(Cmd::Load(pcm.clone()));
                sh.send(Cmd::Keylock(true));
                sh.send(Cmd::Tempo(0.03));
                sh.send(Cmd::Seek(30.0 + 40.0 * d as f64));
                sh.send(Cmd::Play);
                sh
            })
            .collect();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let load: Vec<_> = (0..busy)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    let mut x = 1u64;
                    while !stop.load(Ordering::Relaxed) {
                        x = std::hint::black_box(
                            x.wrapping_mul(6364136223846793005).wrapping_add(1),
                        );
                    }
                })
            })
            .collect();
        // The consumer at real time, as the callback: one quantum per cycle.
        let mut consumers: Vec<Consumer> = (0..decks).map(|_| Consumer::new()).collect();
        let (mut l, mut r) = (vec![0.0f32; quantum], vec![0.0f32; quantum]);
        let period = Duration::from_nanos(quantum as u64 * 1_000_000_000 / rate as u64);
        std::thread::sleep(Duration::from_millis(500));
        let start = std::time::Instant::now();
        let mut fills = Vec::new();
        // Time from a pull until the deck is back at its target (µs); the
        // slowest refills are what a smaller target must still cover.
        let mut refills: Vec<u64> = Vec::new();
        let mut cycle = 0u64;
        let cycles_per_2s = (2 * rate as u64) / quantum as u64;
        while start.elapsed() < Duration::from_secs(seconds) {
            // A seek every 2 s (a fresh target's worth to render) and a
            // tempo move every other second, on alternating decks.
            if cycle > 0 && cycle.is_multiple_of(cycles_per_2s) {
                let d = (cycle / cycles_per_2s) as usize % decks;
                shared[d].send(Cmd::Seek(30.0 + (cycle % 97) as f64));
            } else if cycle % cycles_per_2s == cycles_per_2s / 2 {
                let d = (cycle / cycles_per_2s) as usize % decks;
                shared[d].send(Cmd::Tempo(if cycle.is_multiple_of(3) {
                    0.02
                } else {
                    0.04
                }));
            }
            let pulled = std::time::Instant::now();
            for (sh, c) in shared.iter().zip(&mut consumers) {
                c.graph_sample = cycle * quantum as u64;
                sh.publish_graph_sample(c.graph_sample);
                fills.push(sh.audio.len() / STRIDE);
                c.pull(sh, &mut l, &mut r);
            }
            cycle += 1;
            let next = start + period * cycle as u32;
            // Wait for the refill (or the next cycle) in short steps.
            loop {
                let full = shared.iter().all(|sh| sh.audio.len() / STRIDE >= target);
                let now = std::time::Instant::now();
                if full || now >= next {
                    refills.push(now.duration_since(pulled).as_micros() as u64);
                    break;
                }
                std::thread::sleep(Duration::from_micros(100));
            }
            if let Some(wait) = next.checked_duration_since(std::time::Instant::now()) {
                std::thread::sleep(wait);
            }
        }
        stop.store(true, Ordering::Relaxed);
        for t in load {
            t.join().unwrap();
        }
        fills.sort_unstable();
        let at = |q: f64| fills[((fills.len() - 1) as f64 * q) as usize];
        let min = fills[0];
        let underruns: u64 = shared
            .iter()
            .map(|sh| sh.underruns_total.load(Ordering::Relaxed))
            .sum();
        refills.sort_unstable();
        let refill = |q: f64| refills[((refills.len() - 1) as f64 * q) as usize] as f64 / 1000.0;
        println!(
            "refill ms: p50 {:.2} · p99 {:.2} · p99.9 {:.2} · max {:.2} (cycle {:.2} ms)",
            refill(0.5),
            refill(0.99),
            refill(0.999),
            refill(1.0),
            period.as_secs_f64() * 1000.0
        );
        println!(
            "headroom target={target} quantum={quantum} decks={decks} busy={busy}: min {min} ({:.1} quanta) · p0.1 {} · p1 {} · p50 {} · dip {:.1} ms · underruns {underruns}",
            min as f64 / quantum as f64,
            at(0.001),
            at(0.01),
            at(0.5),
            target.saturating_sub(min) as f64 * 1000.0 / rate as f64,
        );
    }

    #[test]
    #[ignore = "manual performance measurement"]
    fn stereo_batch_cost() {
        let samples = [0.25; CHUNK * 2];
        let mut scratch = [0.0; CHUNK * STRIDE];
        for batched in [false, true] {
            let ring = Ring::new(CHUNK * STRIDE, 0.0);
            let start = std::time::Instant::now();
            for _ in 0..100_000 {
                if batched {
                    std::hint::black_box(unsafe {
                        push_stereo(&ring, &mut scratch, std::hint::black_box(&samples))
                    });
                } else {
                    for frame in std::hint::black_box(&samples).chunks_exact(2) {
                        let mut bundle = [0.0; STRIDE];
                        bundle[..2].copy_from_slice(frame);
                        std::hint::black_box(unsafe { ring.push_slice(&bundle) });
                    }
                }
                unsafe { ring.skip(CHUNK * STRIDE) };
            }
            eprintln!(
                "batched={batched}: {:.1} ns/chunk",
                start.elapsed().as_nanos() as f64 / 100_000.0
            );
        }
    }

    #[test]
    fn only_audio_handed_to_a_deck_is_locked_in_ram() {
        let offline = Pcm::from_samples(48_000, vec![0.25; 48_000 * 2]);
        let live = Pcm::from_samples(48_000, vec![0.25; 48_000 * 2]);
        let probe = Pcm::from_samples(48_000, vec![0.25; 2]);
        probe.keep_resident();
        if !probe.resident() {
            return; // no memlock allowance here: nothing is ever locked
        }
        Shared::new(48_000).send(Cmd::Load(Arc::clone(&live)));
        assert!(live.resident());
        assert!(!offline.resident());
    }

    #[test]
    fn a_paused_deck_with_a_stale_step_does_not_accumulate_underruns() {
        let sh = Shared::new(48_000);
        sh.step.store(1.0f64.to_bits(), Ordering::Relaxed);
        let mut consumer = Consumer::new();
        let (mut left, mut right) = (vec![0.0; 256], vec![0.0; 256]);
        for _ in 0..1_000 {
            consumer.pull(&sh, &mut left, &mut right);
        }
        assert_eq!(sh.underruns.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn deck_guard_caps_multichannel_stem_peaks_and_recovers_smoothly() {
        let mut gain = 1.0;
        let release = 1.0 - (-1.0f32 / (0.080 * 48_000.0)).exp();
        let limited = guard_frame(&mut gain, release, [2.0, -1.5, 0.25, -0.2]);
        assert!(limited.iter().map(|x| x.abs()).fold(0.0f32, f32::max) <= DECK_CEILING + 1e-6);
        let first = guard_frame(&mut gain, release, [0.1, 0.1, 0.0, 0.0]);
        assert!(
            first[0] < 0.1 && gain > 0.0,
            "release should be smooth: {first:?}"
        );
        let mut last = first[0];
        for _ in 0..48_000 {
            last = guard_frame(&mut gain, release, [0.1, 0.1, 0.0, 0.0])[0];
        }
        assert!(
            (last - 0.1).abs() < 1e-5,
            "last={last}, gain={gain}, release={release}"
        );
    }

    /// An audition plays the track from its own playhead into the cue sum
    /// only, leaves the paused transport where it was, and a stop drops
    /// what was queued (faded) so a new start never hears the old one.
    #[test]
    fn audition_is_a_second_playhead_for_the_headphones() {
        let rate = 48_000;
        let sh = Shared::new(rate);
        sh.spawn("test");
        // Frame i holds i / rate on the left: the audio says where it is.
        let samples: Vec<f32> = (0..rate as usize * 4).flat_map(|i| [i as f32 / rate as f32, -1.0]).collect();
        sh.send(Cmd::Load(Pcm::from_samples(rate, samples)));
        sh.send(Cmd::Seek(0.5));
        let mut consumer = Consumer::new();
        let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        // The callback's cycles: they acknowledge a flush (an empty ring
        // then) before the worker queues the new audition.
        let fill = |sh: &Shared, consumer: &mut Consumer| {
            for _ in 0..200 {
                if sh.audition.len() / 2 >= 1024 && !sh.audition_flush.load(Ordering::Acquire) {
                    return;
                }
                if sh.audition_flush.load(Ordering::Acquire) && sh.audition.is_empty() {
                    consumer.audition(sh, &mut [0.0; 256], &mut [0.0; 256], true);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("no audition audio");
        };
        sh.send(Cmd::Audition(Some(2.0)));
        fill(&sh, &mut consumer);
        consumer.audition(&sh, &mut l, &mut r, true);
        assert!(l[0].abs() < 1e-3, "fades in");
        l.fill(0.0);
        r.fill(0.0);
        consumer.audition(&sh, &mut l, &mut r, true);
        assert!((l[255] - (2.0 + 511.0 / rate as f32)).abs() < 1e-4, "{}", l[255]);
        assert_eq!(r[255], -1.0);
        assert!(sh.auditioning.load(Ordering::Acquire));

        // A new spot: nothing of the old one is heard after the flush cycle.
        sh.send(Cmd::Audition(Some(3.0)));
        for _ in 0..200 {
            if sh.audition_flush.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        l.fill(0.0);
        consumer.audition(&sh, &mut l, &mut r, true);
        assert!(l[255].abs() < 0.01, "the flush cycle fades out: {}", l[255]);
        fill(&sh, &mut consumer);
        l.fill(0.0);
        consumer.audition(&sh, &mut l, &mut r, true);
        l.fill(0.0);
        consumer.audition(&sh, &mut l, &mut r, true);
        assert!((l[0] - (3.0 + 256.0 / rate as f32)).abs() < 1e-4, "{}", l[0]);

        // Off: no more audio, and the transport never moved.
        sh.send(Cmd::Audition(None));
        std::thread::sleep(Duration::from_millis(30));
        consumer.audition(&sh, &mut l, &mut r, false);
        std::thread::sleep(Duration::from_millis(30));
        assert!(sh.audition.is_empty());
        assert!(!sh.auditioning.load(Ordering::Acquire));
        let st = sh.status.lock().unwrap().clone();
        assert!(!st.playing && !st.previewing, "the program transport stayed paused");
    }

    /// The waveform tells a new track's playhead from the old one's by
    /// this count: a load or an eject moves it, a jump doesn't.
    #[test]
    fn loads_count_tracks_not_jumps() {
        let rate = 48_000;
        let sh = Shared::new(rate);
        sh.spawn("test");
        let wait = |n: u32| {
            for _ in 0..200 {
                if sh.loads.load(Ordering::Acquire) == n {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("loads {} != {n}", sh.loads.load(Ordering::Acquire));
        };
        sh.send(Cmd::Load(Pcm::from_samples(
            rate,
            vec![0.0; rate as usize * 2],
        )));
        wait(1);
        sh.send(Cmd::Seek(0.5));
        sh.send(Cmd::Play);
        std::thread::sleep(Duration::from_millis(30));
        wait(1);
        sh.send(Cmd::Load(Pcm::from_samples(
            rate,
            vec![0.0; rate as usize * 2],
        )));
        wait(2);
        sh.send(Cmd::Eject);
        wait(3);
    }

    #[test]
    fn engine_positions_follow_the_cycles() {
        let rate = 48_000;
        let sh = Shared::new(rate);
        sh.spawn("test");
        let ramp: Vec<f32> = (0..rate as usize * 4)
            .flat_map(|i| [i as f32 * 1e-6, 0.0])
            .collect();
        sh.send(Cmd::Load(Pcm::from_samples(rate, ramp)));
        sh.send(Cmd::Play);
        let mut c = Consumer::new();
        let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        // Let the worker fill, then play cycles like the engine does.
        std::thread::sleep(Duration::from_millis(50));
        let mut positions = vec![];
        for cycle in 0..50 {
            c.graph_sample = cycle as u64 * l.len() as u64;
            sh.publish_graph_sample(c.graph_sample);
            let (p, _) = c.pull(&sh, &mut l, &mut r);
            let (source_frame, step, output_sample) = sh.timed_playhead();
            assert_eq!(step, 1.0);
            assert_eq!(source_frame, p + l.len() as f64 * step);
            assert_eq!(output_sample, (cycle as u64 + 1) * l.len() as u64);
            positions.push((p, l[0]));
            std::thread::sleep(Duration::from_millis(2));
        }
        // After the first cycle, every cycle advances exactly 512 frames and
        // the audio is the source frame the position names.
        for w in positions.windows(2).skip(1) {
            assert_eq!(w[1].0 - w[0].0, 512.0, "{:?}", w);
        }
        for (p, v) in &positions[1..] {
            assert!(
                (*v - *p as f32 * 1e-6).abs() < 1e-9,
                "sample {v} at position {p}"
            );
        }
        assert_eq!(sh.underruns.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn scheduled_play_stays_silent_until_its_shared_output_sample() {
        let rate = 48_000;
        let sh = Shared::new(rate);
        sh.spawn("scheduled-test");
        sh.send(Cmd::Load(Pcm::from_samples(
            rate,
            [0.25f32, -0.25].repeat(rate as usize),
        )));
        std::thread::sleep(Duration::from_millis(30));
        let at = 8200;
        sh.send(Cmd::PlayAt(at));
        std::thread::sleep(Duration::from_millis(20));
        {
            let status = sh.status.lock().unwrap();
            assert!(!status.playing, "future material is prepared, not playing");
            assert_eq!(status.scheduled_start_sample, Some(at));
        }
        let mut consumer = Consumer::new();
        let (mut left, mut right) = (vec![0.0; 512], vec![0.0; 512]);
        let mut before = Vec::new();
        for _ in 0..16 {
            std::thread::sleep(Duration::from_millis(10));
            consumer.pull(&sh, &mut left, &mut right);
            before.extend_from_slice(&left);
        }
        assert!(before.iter().all(|sample| *sample == 0.0));
        consumer.pull(&sh, &mut left, &mut right);
        assert!(left[..8].iter().all(|sample| *sample == 0.0));
        assert!(left[8..].iter().any(|sample| sample.abs() > 0.1));
        assert!(left.iter().any(|sample| sample.abs() > 0.1));
        assert!(right.iter().any(|sample| sample.abs() > 0.1));
        sh.publish_graph_sample(at);
        std::thread::sleep(Duration::from_millis(60));
        let status = sh.status.lock().unwrap();
        assert!(status.playing);
        assert_eq!(status.scheduled_start_sample, None);
    }

    #[test]
    fn scheduled_pause_silences_on_its_exact_graph_sample() {
        let rate = 48_000;
        let sh = Shared::new(rate);
        sh.spawn("scheduled-pause-test");
        sh.send(Cmd::Load(Pcm::from_samples(
            rate,
            [0.25f32, -0.25].repeat(rate as usize),
        )));
        sh.send(Cmd::Play);
        std::thread::sleep(Duration::from_millis(30));
        let stop_at = 8_192u64;
        sh.send(Cmd::PauseAt(stop_at));
        let mut consumer = Consumer::new();
        let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        let mut before = 0.0;
        let mut at = 1.0;
        for cycle in 0..(stop_at as usize / left.len() + 3) {
            consumer.graph_sample = cycle as u64 * left.len() as u64;
            sh.publish_graph_sample(consumer.graph_sample);
            consumer.pull(&sh, &mut left, &mut right);
            for (offset, sample) in left.iter().copied().enumerate() {
                let stamp = consumer.graph_sample + offset as u64;
                if stamp + 1 == stop_at {
                    before = sample;
                }
                if stamp == stop_at {
                    at = sample;
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(before.abs() > 0.1, "last frame before pause was {before}");
        assert_eq!(at, 0.0, "pause must silence exactly at its sample");
        std::thread::sleep(Duration::from_millis(20));
        assert!(!sh.status.lock().unwrap().playing);
    }

    #[test]
    fn a_stopped_deck_takes_every_seek() {
        let rate = 48_000;
        let sh = Shared::new(rate);
        sh.spawn("seek");
        let ramp: Vec<f32> = (0..rate as usize * 20)
            .flat_map(|i| [i as f32 * 1e-7, 0.0])
            .collect();
        sh.send(Cmd::Load(Pcm::from_samples(rate, ramp)));
        let mut c = Consumer::new();
        let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        let settle = |c: &mut Consumer, l: &mut [f32], r: &mut [f32]| {
            for _ in 0..8 {
                std::thread::sleep(Duration::from_millis(10));
                c.pull(&sh, l, r);
            }
            sh.playhead().0 / rate as f64
        };
        settle(&mut c, &mut l, &mut r);
        // As a deck the DJ played and then paused: audio rendered ahead.
        sh.send(Cmd::Play);
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(10));
            c.pull(&sh, &mut l, &mut r);
        }
        sh.send(Cmd::Pause);
        settle(&mut c, &mut l, &mut r);
        for at in [10.0, 3.0, 15.0, 3.0] {
            sh.send(Cmd::Seek(at));
            let here = settle(&mut c, &mut l, &mut r);
            assert!(
                (here - at).abs() < 0.05,
                "stopped at {here} s after a seek to {at} s"
            );
        }
        // And it plays from there.
        sh.send(Cmd::Play);
        let here = settle(&mut c, &mut l, &mut r);
        assert!((3.0..3.5).contains(&here), "played from {here} s, not 3 s");
    }

    /// After an eject the engine only idles the deck, so the eject's restart
    /// stays "in flight" while the graph runs on until it suspends. Loading
    /// and playing with the graph asleep then started "late" by all of that
    /// (measured on a Mac: 8 s of the new track skipped, 213 underruns).
    /// A stopped deck has nothing scheduled: it is never late.
    #[test]
    fn a_start_after_an_eject_does_not_catch_up_the_idle_graph() {
        let rate = 48_000;
        let sh = Shared::new(rate);
        sh.spawn("eject");
        let track = || Pcm::from_samples(rate, vec![0.1; rate as usize * 40]);
        let mut c = Consumer::new();
        let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        sh.send(Cmd::Load(track()));
        for _ in 0..8 {
            std::thread::sleep(Duration::from_millis(10));
            c.pull(&sh, &mut l, &mut r);
        }
        sh.send(Cmd::Eject);
        std::thread::sleep(Duration::from_millis(30));
        // The graph runs on for 8 s before it suspends.
        for i in 0..(rate as u64 * 8 / 256) {
            c.idle(&sh, i * 256, 256);
        }
        // Asleep now: the load and the start see no cycle.
        sh.send(Cmd::Load(track()));
        std::thread::sleep(Duration::from_millis(30));
        sh.send(Cmd::Play);
        std::thread::sleep(Duration::from_millis(30));
        let mut here = 0.0;
        for _ in 0..40 {
            c.pull(&sh, &mut l, &mut r);
            here = sh.playhead().0 / rate as f64;
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(here < 1.0, "the new track started {here:.2} s in");
    }

    /// With the graph asleep no cycle publishes the place: a track just
    /// loaded must not read as the old track's position.
    #[test]
    fn a_load_with_the_graph_asleep_reads_its_own_position() {
        let rate = 48_000;
        let sh = Shared::new(rate);
        sh.spawn("asleep");
        let track = || Pcm::from_samples(rate, vec![0.1; rate as usize * 40]);
        let mut c = Consumer::new();
        let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        sh.send(Cmd::Load(track()));
        sh.send(Cmd::Seek(6.0));
        for _ in 0..8 {
            std::thread::sleep(Duration::from_millis(10));
            c.pull(&sh, &mut l, &mut r);
        }
        assert!(sh.playhead().0 / rate as f64 > 5.0);
        sh.send(Cmd::Load(track()));
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(sh.playhead().0, 0.0);
    }

    /// A stopped deck's seek is one 256-frame chunk; when the engine adopts
    /// it a cycle late, its whole chunk is skipped. The place must still be
    /// the new one, and play must start there (live, every other seek on a
    /// stopped deck was lost this way).
    #[test]
    fn a_seek_whose_chunk_is_skipped_still_moves_the_deck() {
        let rate = 48_000;
        let sh = Shared::new(rate);
        sh.spawn("seek-skip");
        let ramp: Vec<f32> = (0..rate as usize * 20)
            .flat_map(|i| [i as f32 * 1e-7, 0.0])
            .collect();
        sh.send(Cmd::Load(Pcm::from_samples(rate, ramp)));
        let mut c = Consumer::new();
        let (mut l, mut r) = (vec![0.0f32; 1024], vec![0.0f32; 1024]);
        for _ in 0..8 {
            std::thread::sleep(Duration::from_millis(10));
            c.pull(&sh, &mut l, &mut r);
        }
        for at in [10.0, 3.0] {
            sh.send(Cmd::Seek(at));
            std::thread::sleep(Duration::from_millis(30));
            // Adopted a whole cycle late: the skip covers the chunk.
            c.played += 4096;
            for _ in 0..4 {
                c.pull(&sh, &mut l, &mut r);
                std::thread::sleep(Duration::from_millis(10));
            }
            let here = sh.playhead().0 / rate as f64;
            assert!(
                (here - at).abs() < 0.05,
                "stopped at {here} s after a seek to {at} s"
            );
        }
        sh.send(Cmd::Play);
        for _ in 0..6 {
            std::thread::sleep(Duration::from_millis(10));
            c.pull(&sh, &mut l, &mut r);
        }
        let here = sh.playhead().0 / rate as f64;
        assert!((3.0..3.5).contains(&here), "played from {here} s, not 3 s");
    }

    /// As the engine drives it: the callback pulls every cycle on its own
    /// thread while seeks arrive seconds apart (the idle worker parked).
    #[test]
    fn a_stopped_deck_takes_seeks_seconds_apart_while_the_engine_runs() {
        use std::sync::atomic::AtomicBool;
        let rate = 48_000;
        let sh = std::sync::Arc::new(Shared::new(rate));
        sh.spawn("seek-live");
        let ramp: Vec<f32> = (0..rate as usize * 20)
            .flat_map(|i| [i as f32 * 1e-7, 0.0])
            .collect();
        sh.send(Cmd::Load(Pcm::from_samples(rate, ramp)));
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let engine = {
            let (sh, stop) = (sh.clone(), stop.clone());
            std::thread::spawn(move || {
                // A long engine cycle, as a large PipeWire quantum makes it.
                let mut c = Consumer::new();
                let (mut l, mut r) = (vec![0.0f32; 4096], vec![0.0f32; 4096]);
                while !stop.load(Ordering::Relaxed) {
                    c.pull(&sh, &mut l, &mut r);
                    std::thread::sleep(Duration::from_micros(85_333));
                }
            })
        };
        std::thread::sleep(Duration::from_millis(1500));
        let mut lost = vec![];
        for at in [10.0, 3.0, 15.0, 7.0, 12.0, 4.0] {
            sh.send(Cmd::Seek(at));
            std::thread::sleep(Duration::from_millis(1500));
            let here = sh.playhead().0 / rate as f64;
            if (here - at).abs() > 0.05 {
                lost.push((at, here));
            }
        }
        stop.store(true, Ordering::Relaxed);
        engine.join().unwrap();
        assert!(lost.is_empty(), "seeks lost (wanted, stayed at): {lost:?}");
    }

    #[test]
    fn a_jump_takes_effect_on_the_next_cycle() {
        let rate = 48_000;
        let sh = Shared::new(rate);
        sh.spawn("jump");
        let ramp: Vec<f32> = (0..rate as usize * 20)
            .flat_map(|i| [i as f32 * 1e-7, 0.0])
            .collect();
        sh.send(Cmd::Load(Pcm::from_samples(rate, ramp)));
        sh.send(Cmd::Grid(Some(Grid {
            bpm: 120.0,
            offset: 0.0,
            downbeat: None,
        })));
        sh.send(Cmd::Play);
        let mut c = Consumer::new();
        let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        std::thread::sleep(Duration::from_millis(50));
        for _ in 0..4 {
            c.pull(&sh, &mut l, &mut r);
        }
        let (before, _) = sh.playhead();
        sh.send(Cmd::Jump(8.0)); // 8 beats = 192 000 frames
        std::thread::sleep(Duration::from_millis(20));
        let (p, _) = c.pull(&sh, &mut l, &mut r);
        assert_eq!(
            p,
            before + 192_000.0,
            "jumped exactly 8 beats from the playhead"
        );
    }

    #[test]
    fn a_jump_never_gaps_and_lands_where_an_instant_jump_would() {
        let rate = 48_000;
        let sh = Shared::new(rate);
        sh.spawn("gapless");
        let ramp: Vec<f32> = (0..rate as usize * 20)
            .flat_map(|i| [i as f32 * 1e-7 + 1e-3, 0.0])
            .collect();
        sh.send(Cmd::Load(Pcm::from_samples(rate, ramp)));
        sh.send(Cmd::Grid(Some(Grid {
            bpm: 120.0,
            offset: 0.0,
            downbeat: None,
        })));
        sh.send(Cmd::Play);
        let mut c = Consumer::new();
        let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        std::thread::sleep(Duration::from_millis(50));
        for _ in 0..4 {
            c.pull(&sh, &mut l, &mut r);
        }
        let (before, _) = sh.playhead();
        // Jump and pull at once, before the worker can have rendered it:
        // the old audio keeps playing (no silence), then the new one takes
        // over where it would be had the jump been instant.
        sh.send(Cmd::Jump(8.0));
        let mut switched = false;
        for k in 0..12u64 {
            let (p, _) = c.pull(&sh, &mut l, &mut r);
            assert!(l.iter().all(|v| *v != 0.0), "no gap at pull {k}");
            if p >= before + 192_000.0 {
                switched = true;
                assert_eq!(
                    p,
                    before + 192_000.0 + (k * 512) as f64,
                    "phase-exact at pull {k}"
                );
                // The audio is the source frame the position names (after
                // the crossfade).
                let i = SWITCH_FADE as usize + 44;
                assert!((l[i] - ((p + i as f64) as f32 * 1e-7 + 1e-3)).abs() < 1e-6);
            } else {
                assert_eq!(
                    p,
                    before + (k * 512) as f64,
                    "old audio continues at pull {k}"
                );
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(switched);
        assert_eq!(sh.underruns.load(Ordering::Relaxed), 0);
    }

    /// A jump crossfades: no sample-to-sample step bigger than the music's
    /// own across the switch (a cut to silence was a click every time).
    #[test]
    fn a_jump_is_a_crossfade_not_a_cut() {
        let rate = 48_000;
        let sh = Shared::new(rate);
        sh.spawn("crossfade");
        // A loud 220 Hz tone: any cut shows as a step far above its slope.
        let tone: Vec<f32> = (0..rate as usize * 20)
            .flat_map(|i| {
                let v = 0.8 * (i as f32 * 220.0 * std::f32::consts::TAU / rate as f32).sin();
                [v, v]
            })
            .collect();
        sh.send(Cmd::Load(Pcm::from_samples(rate, tone)));
        sh.send(Cmd::Play);
        let mut c = Consumer::new();
        let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        std::thread::sleep(Duration::from_millis(50));
        let mut out = vec![];
        for _ in 0..4 {
            c.pull(&sh, &mut l, &mut r);
            out.extend_from_slice(&l);
        }
        sh.send(Cmd::Seek(7.3));
        std::thread::sleep(Duration::from_millis(20));
        for _ in 0..4 {
            c.pull(&sh, &mut l, &mut r);
            out.extend_from_slice(&l);
        }
        let slope = 0.8 * 220.0 * std::f32::consts::TAU / rate as f32;
        let worst = out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(
            worst < slope * 1.6,
            "a step of {worst} against the tone's {slope}"
        );
    }

    #[test]
    fn pause_is_immediate_and_play_resumes_on_time() {
        let rate = 48_000;
        let sh = Shared::new(rate);
        sh.spawn("pause");
        let ramp: Vec<f32> = (0..rate as usize * 20)
            .flat_map(|i| [i as f32 * 1e-7 + 1e-3, 0.0])
            .collect();
        sh.send(Cmd::Load(Pcm::from_samples(rate, ramp)));
        sh.send(Cmd::Play);
        let mut c = Consumer::new();
        let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        std::thread::sleep(Duration::from_millis(50));
        for _ in 0..4 {
            c.pull(&sh, &mut l, &mut r);
        }
        sh.send(Cmd::Pause);
        std::thread::sleep(Duration::from_millis(10));
        c.pull(&sh, &mut l, &mut r);
        // The music fades out over the switch (no click), then is silent.
        let fade = SWITCH_FADE as usize;
        assert!(l[0] != 0.0, "the old audio fades out, not cut");
        assert!(
            l.windows(2)
                .take(fade)
                .all(|w| w[1].abs() <= w[0].abs() + 1e-6),
            "a monotone fade-out"
        );
        assert!(
            l[fade..].iter().all(|v| *v == 0.0),
            "silent once faded, in the cycle after the pause"
        );
        let (at, _) = sh.playhead();
        // Paused for a long while (many cycles), then play: it resumes
        // where it stopped, not skipping the paused time.
        for _ in 0..200 {
            c.pull(&sh, &mut l, &mut r);
        }
        sh.send(Cmd::Play);
        std::thread::sleep(Duration::from_millis(20));
        let (p, _) = c.pull(&sh, &mut l, &mut r);
        assert!(
            p >= at && p <= at + 1024.0,
            "resumed at {p}, paused at {at}"
        );
    }

    fn marker_set(id: &str) -> Arc<patch_core::stems::StemSet> {
        Arc::new(patch_core::stems::StemSet {
            version: 1,
            id: id.into(),
            source_hash: String::new(),
            track_id: String::new(),
            model_id: String::new(),
            model_version: String::new(),
            weights_digest: String::new(),
            profile: String::new(),
            pipeline_version: 1,
            backend: String::new(),
            sample_rate: 48_000,
            frames: 0,
            duration: 0.0,
            layout: vec![],
            stems: Default::default(),
            created: 0,
        })
    }

    /// R1: a control thread holding `stem_set` and `stem_error` must not stall
    /// the FIFO worker. The clear is published once the locks are released.
    #[test]
    fn the_worker_renders_while_stem_locks_are_held() {
        let rate = 48_000;
        let sh = Shared::new(rate);
        *sh.stem_set.lock().unwrap() = Some(marker_set("held"));
        *sh.stem_error.lock().unwrap() = Some(Arc::from("old"));
        sh.spawn("stem-lock");
        let hold_set = sh.stem_set.lock().unwrap();
        let hold_error = sh.stem_error.lock().unwrap();
        sh.send(Cmd::Load(Pcm::from_samples(
            rate,
            [0.25f32, -0.25].repeat(rate as usize),
        )));
        sh.send(Cmd::Play);
        let mut consumer = Consumer::new();
        let (mut left, mut right) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        let start = std::time::Instant::now();
        let mut heard = false;
        while start.elapsed() < Duration::from_millis(500) {
            consumer.pull(&sh, &mut left, &mut right);
            if left.iter().any(|sample| sample.abs() > 0.1) {
                heard = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(heard, "deck worker blocked on stem_set or stem_error");
        drop(hold_error);
        drop(hold_set);
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_millis(500) {
            let cleared =
                sh.stem_set.lock().unwrap().is_none() && sh.stem_error.lock().unwrap().is_none();
            if cleared {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("stem identity was not cleared after the locks were released");
    }

    /// R3: status text is the loader's `Arc`, not a new string every snapshot.
    #[test]
    fn status_text_is_shared_across_snapshots() {
        let rate = 48_000;
        let sh = Shared::new(rate);
        sh.spawn("status-arc");
        let mut pcm = Pcm::from_samples(rate, vec![0.1; rate as usize * 2]);
        {
            let pcm = Arc::get_mut(&mut pcm).unwrap();
            pcm.path = "deck-song.wav".into();
            pcm.info.title = "Hello".into();
            pcm.info.artist = "DJ".into();
            pcm.info.codec = "flac".into();
            pcm.labels = crate::deck::pcm::track_labels(&pcm.path, &pcm.info);
        }
        sh.send(Cmd::Load(pcm));
        sh.send(Cmd::Play);
        std::thread::sleep(Duration::from_millis(80));
        let first = sh.status.lock().unwrap().clone();
        assert_eq!(first.path.as_ref(), "deck-song.wav");
        assert_eq!(first.title.as_ref(), "Hello");
        assert_eq!(first.artist.as_ref(), "DJ");
        assert_eq!(first.codec.as_ref(), "flac");
        std::thread::sleep(Duration::from_millis(80));
        let second = sh.status.lock().unwrap().clone();
        assert!(Arc::ptr_eq(&first.path, &second.path));
        assert!(Arc::ptr_eq(&first.title, &second.title));
        assert!(Arc::ptr_eq(&first.artist, &second.artist));
        assert!(Arc::ptr_eq(&first.codec, &second.codec));
    }
}
