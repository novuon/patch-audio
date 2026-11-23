//! Deck transport: what a deck plays, where, how fast, in which key, and
//! how it loops. Pure logic, driven by the deck worker (see `worker.rs`) and
//! by tests; it never runs on the RT thread.
//!
//! The transport renders ahead in chunks. Every chunk is described by a
//! [`Tag`]: the source position of its first frame and the source frames
//! per output frame. The RT side reads tags as it plays, so the engine knows
//! the exact source sample leaving at every graph cycle. A generation
//! number lets a jump discard audio rendered before it, so cues, jumps and
//! seeks take effect on the next cycle.
//!
//! Three render paths, chosen per chunk:
//! * direct: speed 1, no key shift → the decoded samples, bit-exact;
//! * varispeed (keylock off, no key shift) → band-limited sinc resampling;
//! * stretch (keylock, or any key shift) → Rubber Band R3, primed with its
//!   preferred start pad so output starts exactly at the position.
//!
//! Loops wrap inside all three: reads map through the loop, so the sample
//! after the loop end is the loop start sample.

use super::pcm::Pcm;
use super::sinc::Sinc;
use super::stretch::Stretcher;
use std::sync::Arc;

/// Frames per render chunk.
pub const CHUNK: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tag {
    pub bundle: bool,
    pub generation: u32,
    pub frames: u32,
    /// Source position (frames, at the graph rate) of the first frame.
    pub src: f64,
    /// Source frames per output frame (0 while stopped).
    pub step: f64,
}

/// Beat grid of the loaded track (from analysis or a manual correction).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub bpm: f64,
    /// First beat, seconds.
    pub offset: f64,
    /// Which grid beat (0…3) starts a bar, when known (4/4).
    pub downbeat: Option<u8>,
}

/// Within this many frames of the cue point, a paused deck "is on it".
const CUE_SNAP: f64 = 64.0;

#[derive(Debug)]
pub enum Cmd {
    Load(Arc<Pcm>),
    AttachStems(Box<super::stems::Audio>),
    Reverse(bool),
    Scratch {
        seconds: f64,
        velocity: f64,
    },
    ScratchEnd,
    Eject,
    Play,
    /// Begin playback at this deck worker's output-clock sample.
    PlayAt(u64),
    /// Stop transport at this engine graph-clock sample.
    PauseAt(u64),
    Pause,
    Toggle,
    /// Seconds.
    Seek(f64),
    /// CDJ cue (pressed): playing → back to the cue point, paused; paused
    /// on the cue point → preview from it while held; paused elsewhere →
    /// set it here.
    Cue,
    /// Cue released: a preview returns to the cue point and pauses.
    CueRelease,
    /// Headphone audition from here (seconds), or off: a second playhead
    /// the worker reads for the headphones only; the transport ignores it.
    Audition(Option<f64>),
    /// A hot cue pressed (seconds). Paused: play from it while held, like
    /// a CDJ (CueRelease returns there, paused). Playing: jump to it.
    GateCue(f64),
    /// Tempo fader, −0.5 … +0.5 (0.02 = +2 %).
    Tempo(f64),
    /// Temporary bend (nudge, sync correction), ±0.1.
    Bend(f64),
    Keylock(bool),
    /// Semitones.
    Key(i32),
    Grid(Option<Grid>),
    /// Loop `beats` from the current beat (quantized when a grid exists).
    AutoLoop(f64),
    LoopIn,
    LoopOut,
    LoopExit,
    /// Exit, or replay the last loop from its in-point. No history: no-op.
    Reloop,
    LoopSet {
        from: f64,
        to: f64,
        play: bool,
    },
    LoopAdjust {
        out: bool,
        seconds: f64,
    },
    Censor(bool),
    RecallCue(f64),
    /// Halve (0.5) or double (2.0) the loop.
    LoopScale(f64),
    /// Beats (negative = back); keeps the phase.
    Jump(f64),
    Slip(bool),
    Quantize(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Path {
    Silence,
    Direct,
    Vari,
    Stretch,
}

pub struct Transport {
    pub rate: u32,
    pub stems: Option<Box<super::stems::Audio>>,
    pub reverse: bool,
    pub censor_slip: Option<bool>,
    pub scratching: Option<f64>,
    /// While scrubbing by hand: the source frame the deck glides to (the
    /// platter following the hand; see `Cmd::Scratch`).
    scratch_target: Option<f64>,
    scratch_playing: bool,
    pub pcm: Option<Arc<Pcm>>,
    /// Source position of the next frame to render.
    pub pos: f64,
    pub playing: bool,
    pub end: bool,
    pub tempo: f64,
    pub bend: f64,
    pub keylock: bool,
    pub key: i32,
    pub grid: Option<Grid>,
    pub quantize: bool,
    /// Loop (in, out) in source frames, and whether it's engaged.
    pub looping: Option<(f64, f64)>,
    pub loop_on: bool,
    pending_in: Option<f64>,
    pub slip: bool,
    /// Where the track would be without loops/jumps (slip mode).
    pub shadow: f64,
    /// Where the cue point is, once one has been set at all. `None` means no
    /// cue: a bare `f64` cannot tell that apart from a cue at the very start,
    /// because a cue near zero snaps to zero — so the cue light stayed dark for
    /// a deck parked at the beginning of its track.
    pub cue_point: Option<f64>,
    /// Playing only while Cue is held (released: back to the cue point).
    pub previewing: bool,
    /// Where a held preview returns to when it is not the cue point (a gated
    /// hot cue).
    preview_from: Option<f64>,
    pub generation: u32,
    path: Path,
    stretch: Option<Stretcher>,
    primed: bool,
    rb_in: f64,
    discard: usize,
    sinc: Sinc,
    l: Vec<f32>,
    r: Vec<f32>,
    inter: Vec<f32>,
    /// A 16-bit track's frames for one resampler chunk, as float.
    window: Vec<f32>,
    /// Interleaved stem bundle, reused every chunk.
    pub(crate) bundle: Vec<f32>,
    /// Planar scratch for the stem stretcher, one buffer per channel.
    pub(crate) planes: Vec<Vec<f32>>,
    /// Previous-path audio for a stretch crossfade.
    fade: Vec<f32>,
    /// Crossfaded chunk. Kept so the mix is not a fresh `Vec`.
    fade_mix: Vec<f32>,
}

impl Transport {
    pub fn new(rate: u32) -> Transport {
        Transport {
            rate,
            stems: None,
            reverse: false,
            censor_slip: None,
            scratching: None,
            scratch_target: None,
            scratch_playing: false,
            pcm: None,
            pos: 0.0,
            playing: false,
            end: false,
            tempo: 0.0,
            bend: 0.0,
            keylock: true,
            key: 0,
            grid: None,
            quantize: true,
            looping: None,
            loop_on: false,
            pending_in: None,
            slip: false,
            shadow: 0.0,
            cue_point: None,
            previewing: false,
            preview_from: None,
            generation: 0,
            path: Path::Silence,
            stretch: None,
            primed: false,
            rb_in: 0.0,
            discard: 0,
            sinc: Sinc::new(),
            l: vec![0.0; 8192],
            r: vec![0.0; 8192],
            inter: vec![0.0; CHUNK * 2],
            window: Vec::new(),
            bundle: vec![0.0; CHUNK * super::stems::STRIDE],
            planes: (0..super::stems::STRIDE).map(|_| vec![0.0; 8192]).collect(),
            fade: Vec::with_capacity(CHUNK * 2),
            fade_mix: vec![0.0; CHUNK * 2],
        }
    }

    pub fn speed(&self) -> f64 {
        self.scratching.unwrap_or(
            (1.0 + self.tempo) * (1.0 + self.bend) * if self.reverse { -1.0 } else { 1.0 },
        )
    }

    /// Frames per beat at the graph rate (source time).
    pub fn beat_frames(&self) -> Option<f64> {
        self.grid.map(|g| 60.0 / g.bpm * self.rate as f64)
    }

    fn grid_origin(&self) -> f64 {
        self.grid
            .map(|g| g.offset * self.rate as f64)
            .unwrap_or(0.0)
    }

    /// Snap to the beat at or before `p` (or the `div`-beat grid).
    fn floor_to_beat(&self, p: f64, div: f64) -> f64 {
        match (self.beat_frames(), self.quantize) {
            (Some(per), true) => {
                let o = self.grid_origin();
                let step = per * div;
                let mut a = o + ((p - o) / step).floor() * step;
                // Never before the track: the first grid point at or after 0.
                while a < 0.0 {
                    a += step;
                }
                a
            }
            _ => p,
        }
    }

    fn snap_to_beat(&self, p: f64) -> f64 {
        match (self.beat_frames(), self.quantize) {
            (Some(per), true) => {
                let o = self.grid_origin();
                (o + ((p - o) / per).round() * per).max(0.0)
            }
            _ => p,
        }
    }

    pub(super) fn len(&self) -> f64 {
        self.pcm.as_ref().map(|p| p.length() as f64).unwrap_or(0.0)
    }

    /// Map a source index through the active loop.
    #[inline]
    pub(super) fn map(&self, i: i64) -> i64 {
        if let (true, Some((a, b))) = (self.loop_on, self.looping) {
            let (a, b) = (a.round() as i64, b.round() as i64);
            if b > a && (i >= b || (self.reverse || self.scratching.is_some()) && i < a) {
                return a + (i - a).rem_euclid(b - a);
            }
        }
        i
    }

    fn sample(&self, i: i64, c: usize) -> f32 {
        let Some(p) = &self.pcm else { return 0.0 };
        let j = self.map(i);
        if j < 0 { 0.0 } else { p.get(j as usize, c) }
    }

    /// Wrap a position that ran past the loop end back into the loop.
    pub(super) fn wrap(&self, p: f64) -> f64 {
        if let (true, Some((a, b))) = (self.loop_on, self.looping) {
            let (ai, bi) = (a.round(), b.round());
            if bi > ai && (p >= bi || (self.reverse || self.scratching.is_some()) && p < ai) {
                return ai + (p - ai).rem_euclid(bi - ai);
            }
        }
        p
    }

    /// Apply a command. `now` is the source position the RT side will play
    /// next (the published playhead), used when a command must take effect
    /// at once: it discards rendered-ahead audio (new generation) and
    /// continues from there.
    pub fn apply(&mut self, cmd: Cmd, now: Option<f64>) {
        let here = now.unwrap_or(self.pos);
        let restart = |t: &mut Transport, p: f64| {
            t.pos = p.max(0.0);
            t.generation = t.generation.wrapping_add(1);
            t.primed = false;
            // Re-primed at the new place on its next chunk, not rebuilt: a
            // new stretcher is ~3 MB of buffers and a sinc table designed on
            // the render thread, at every cue, seek and jump.
            if let Some(render) = t.stems.as_mut().and_then(|a| a.render.as_mut()) {
                render.restart();
            }
            t.end = false;
        };
        match cmd {
            Cmd::Reverse(on) => {
                if let Some(slip) = self.censor_slip.take() {
                    self.slip = slip;
                }
                self.reverse = on;
                restart(self, here);
            }
            // The first touch jumps there and plays at `velocity`; while the
            // hand keeps moving, each further position is a target the deck
            // glides to (its speed re-aimed every chunk), so a drag sounds
            // like a platter, not a string of jumps.
            Cmd::Scratch { seconds, velocity } => {
                if self.scratching.is_none() {
                    self.scratch_playing = self.playing;
                    self.scratching = Some(velocity.clamp(-4.0, 4.0));
                    self.scratch_target = None;
                    self.playing = true;
                    restart(self, seconds * self.rate as f64);
                } else {
                    self.scratch_target = Some(seconds * self.rate as f64);
                }
            }
            Cmd::Audition(_) => {}
            Cmd::ScratchEnd => {
                self.scratching = None;
                self.scratch_target = None;
                self.playing = self.scratch_playing;
                let position = if self.slip { self.shadow } else { here };
                restart(self, position);
            }
            Cmd::AttachStems(audio) => {
                if self
                    .pcm
                    .as_ref()
                    .is_some_and(|p| p.path.to_string_lossy() == audio.source_path)
                    && audio.set.playback_ready()
                    && audio.ids == audio.set.layout
                {
                    self.stems = Some(audio);
                    restart(self, here);
                }
            }
            Cmd::Load(pcm) => {
                if let Some(slip) = self.censor_slip.take() {
                    self.slip = slip;
                }
                self.stems = None;
                self.reverse = false;
                self.scratching = None;
                self.scratch_target = None;
                if let Some(previous) = self.pcm.replace(pcm)
                    && !self
                        .pcm
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(&previous, current))
                {
                    previous.cancel_decode();
                }
                self.playing = false;
                self.looping = None;
                self.loop_on = false;
                self.pending_in = None;
                self.cue_point = None;
                self.grid = None;
                self.shadow = 0.0;
                restart(self, 0.0);
            }
            Cmd::Eject => {
                if let Some(slip) = self.censor_slip.take() {
                    self.slip = slip;
                }
                self.reverse = false;
                self.loop_on = false;
                self.looping = None;
                self.pending_in = None;
                self.cue_point = None;
                self.previewing = false;
                self.stems = None;
                if let Some(previous) = self.pcm.take() {
                    previous.cancel_decode();
                }
                self.playing = false;
                restart(self, 0.0);
            }
            Cmd::Play | Cmd::PlayAt(_) => {
                // Play during a preview: it keeps playing after release.
                if self.previewing {
                    self.previewing = false;
                } else if self.pcm.is_some() && !self.end && !self.playing {
                    // A timed restart: the consumer starts it on the output
                    // frame it was meant for (not a cycle late).
                    restart(self, here);
                    self.playing = true;
                    self.shadow = here;
                }
            }
            Cmd::CueRelease => {
                if self.previewing {
                    self.previewing = false;
                    self.playing = false;
                    let back = self.preview_from.take().or(self.cue_point);
                    restart(self, back.unwrap_or(0.0));
                }
            }
            Cmd::GateCue(secs) => {
                if self.pcm.is_some() && secs.is_finite() && secs >= 0.0 {
                    let p = (secs * self.rate as f64).min(self.len());
                    if !self.playing {
                        self.previewing = true;
                        self.playing = true;
                        self.preview_from = Some(p);
                    }
                    self.shadow = p;
                    restart(self, p);
                }
            }
            Cmd::Pause => {
                self.previewing = false;
                if self.playing {
                    self.playing = false;
                    restart(self, here);
                }
            }
            // Scheduled pauses are consumed by the deck worker because they
            // must also gate audio at the engine sample clock.
            Cmd::PauseAt(_) => {}
            Cmd::Toggle => {
                let c = if self.playing { Cmd::Pause } else { Cmd::Play };
                self.apply(c, now);
            }
            Cmd::Seek(secs) => {
                let p = secs * self.rate as f64;
                self.shadow = p;
                restart(self, p);
            }
            Cmd::Cue => {
                // Played to the end: back to the cue point (a cue point at
                // the very end would strand the deck there).
                if self.previewing {
                    // Key repeat while held: keep previewing.
                } else if !self.playing
                    && !self.end
                    && self.pcm.is_some()
                    && self.cue_point.is_some_and(|c| (here - c).abs() < CUE_SNAP)
                {
                    self.previewing = true;
                    self.playing = true;
                    self.preview_from = None;
                    self.shadow = here;
                } else if self.playing || self.end {
                    // Back to the cue point; with no cue set, the start of the
                    // track is the only place left to go.
                    self.playing = false;
                    restart(self, self.cue_point.unwrap_or(0.0));
                } else {
                    let c = self.snap_to_beat(here);
                    self.cue_point = Some(c);
                    restart(self, c);
                }
            }
            Cmd::Tempo(t) => self.tempo = t.clamp(-0.5, 0.5),
            Cmd::Bend(b) => self.bend = b.clamp(-0.1, 0.1),
            Cmd::Keylock(on) => {
                if self.keylock != on {
                    self.keylock = on;
                    self.primed = false;
                }
            }
            Cmd::Key(k) => self.key = k.clamp(-12, 12),
            Cmd::Grid(g) => self.grid = g,
            Cmd::Quantize(q) => self.quantize = q,
            Cmd::AutoLoop(beats) => {
                if let Some(per) = self.beat_frames() {
                    let len = per * beats;
                    let a = self.floor_to_beat(here, beats.min(1.0));
                    self.looping = Some((a, a + len));
                    self.loop_on = true;
                    // Rendered-ahead audio past the new end must go.
                    if self.pos >= a + len {
                        let p = self.wrap(here);
                        restart(self, p);
                    }
                }
            }
            Cmd::RecallCue(seconds) => {
                if seconds.is_finite() && seconds >= 0.0 && self.pcm.is_some() {
                    let position = (seconds * self.rate as f64).min(self.len());
                    self.cue_point = Some(position);
                    self.playing = false;
                    self.previewing = false;
                    self.loop_on = false;
                    self.shadow = position;
                    restart(self, position);
                }
            }
            Cmd::Censor(on) => {
                if on && self.censor_slip.is_none() && self.pcm.is_some() {
                    self.censor_slip = Some(self.slip);
                    if !self.slip {
                        self.shadow = here;
                    }
                    self.slip = true;
                    self.reverse = true;
                    restart(self, here);
                } else if !on && let Some(slip) = self.censor_slip.take() {
                    self.reverse = false;
                    self.slip = slip;
                    restart(self, self.shadow);
                }
            }
            Cmd::LoopSet { from, to, play } => {
                if from.is_finite() && to.is_finite() && from >= 0.0 && to > from {
                    let a = from * self.rate as f64;
                    let b = (to * self.rate as f64).min(self.len());
                    if b - a >= 64.0 {
                        self.looping = Some((a, b));
                        self.loop_on = true;
                        self.previewing = false;
                        self.playing = play;
                        self.shadow = a;
                        restart(self, a);
                    }
                }
            }
            Cmd::LoopAdjust { out, seconds } => {
                if let Some((mut a, mut b)) = self.looping
                    && seconds.is_finite()
                    && self.len() >= 64.0
                {
                    let delta = seconds.clamp(-10.0, 10.0) * self.rate as f64;
                    b = b.clamp(64.0, self.len());
                    a = a.clamp(0.0, b - 64.0);
                    if out {
                        b = (b + delta).clamp(a + 64.0, self.len());
                    } else {
                        a = (a + delta).clamp(0.0, b - 64.0);
                    }
                    self.looping = Some((a, b));
                    if self.loop_on {
                        let position = if here < a { a } else { self.wrap(here) };
                        restart(self, position);
                    }
                }
            }
            Cmd::LoopIn => {
                let point = self.snap_to_beat(here);
                self.pending_in = Some(point);
                self.cue_point = Some(point);
            }
            Cmd::LoopOut => {
                if let Some(a) = self.pending_in.take() {
                    let b = self.snap_to_beat(here);
                    if b > a + 64.0 {
                        self.looping = Some((a, b));
                        self.loop_on = true;
                        let p = self.wrap(here);
                        restart(self, p);
                    }
                }
            }
            Cmd::LoopExit | Cmd::Reloop if self.loop_on => {
                self.loop_on = false;
                if self.slip {
                    let s = self.shadow;
                    restart(self, s);
                }
            }
            Cmd::LoopExit => {}
            Cmd::Reloop => {
                if let Some((a, b)) = self.looping
                    && a >= 0.0
                    && b > a
                    && a < self.len()
                {
                    self.loop_on = true;
                    if !self.playing {
                        self.shadow = here;
                    }
                    self.playing = true;
                    self.previewing = false;
                    restart(self, a);
                }
            }
            Cmd::LoopScale(f) => {
                if let Some((a, b)) = self.looping {
                    let nb = a + (b - a) * f;
                    if nb - a >= 64.0 {
                        self.looping = Some((a, nb));
                        if self.loop_on && self.pos >= nb {
                            let p = self.wrap(here);
                            restart(self, p);
                        }
                    }
                }
            }
            Cmd::Jump(beats) => {
                if let Some(per) = self.beat_frames() {
                    let d = beats * per;
                    if let (true, Some((a, b))) = (self.loop_on, self.looping) {
                        // Loop move: the loop travels with the jump.
                        if a + d >= 0.0 {
                            self.looping = Some((a + d, b + d));
                            restart(self, here + d);
                        }
                    } else {
                        restart(self, (here + d).max(0.0));
                    }
                }
            }
            Cmd::Slip(on) => {
                self.slip = on;
                self.shadow = here;
            }
        }
    }

    fn wanted_path(&self) -> Path {
        if !self.playing || self.pcm.is_none() {
            Path::Silence
        } else if self.speed() == 1.0 && self.key == 0 && self.pos.fract() == 0.0 {
            Path::Direct
        } else if self.key == 0 && (!self.keylock || !self.pitch_would_show()) {
            Path::Vari
        } else {
            Path::Stretch
        }
    }

    /// Keylock only when the pitch would move audibly: above 0.3 % (about
    /// 5 cents, where the smallest heard difference lies), back below
    /// 0.15 % (no flipping around the edge). The AI's fine tempo matches
    /// (a few hundredths of a percent) play through the sinc resampler,
    /// not the time-stretcher: less CPU, and no stretching artefacts.
    fn pitch_would_show(&self) -> bool {
        self.pitch_would_show_while(self.path == Path::Stretch)
    }

    /// The same rule for a path that tracks its own stretcher (the stems
    /// bundle): `stretching` is whether that stretcher runs now.
    pub(super) fn pitch_would_show_while(&self, stretching: bool) -> bool {
        let off = (self.speed() - 1.0).abs();
        off > if stretching { 0.0015 } else { 0.003 }
    }

    pub(super) fn pitch(&self) -> f64 {
        let k = 2f64.powf(self.key as f64 / 12.0);
        if self.keylock { k } else { k * self.speed() }
    }

    /// Render one chunk; `emit(tag, interleaved)` receives it (split into
    /// segments at loop wraps, so every tag's positions are continuous).
    pub fn render(&mut self, emit: &mut impl FnMut(Tag, &[f32])) {
        // Steer a scratch toward the hand before either path renders: with
        // stems attached the stems path plays at this speed, and steering
        // only in `render_scratched` left a stems deck frozen under the hand.
        self.steer_scratch();
        if self.stems.is_some() {
            super::stems::render(self, emit);
            return;
        }
        if self.reverse || self.scratching.is_some() {
            self.render_scratched(emit);
            return;
        }
        let path = self.wanted_path();
        // Into or out of the time-stretcher (keylock engaging or releasing as
        // the tempo moves): the two paths' waveforms differ, so a hard switch
        // clicks. Render this chunk through the old path too and crossfade.
        let old = self.path;
        let cross = old != path
            && (old == Path::Stretch || path == Path::Stretch)
            && old != Path::Silence
            && path != Path::Silence;
        let mut fade = std::mem::take(&mut self.fade);
        if cross {
            let (pos, playing, previewing, end) =
                (self.pos, self.playing, self.previewing, self.end);
            fade.clear();
            let mut keep = |_: Tag, a: &[f32]| fade.extend_from_slice(a);
            match old {
                Path::Stretch => self.render_stretch(&mut keep),
                Path::Vari => self.render_vari(&mut keep),
                _ => self.render_direct(&mut keep),
            }
            (self.pos, self.playing, self.previewing, self.end) = (pos, playing, previewing, end);
        }
        if path != Path::Stretch {
            self.primed = false;
        }
        self.path = path;
        let start_shadow_step = if self.playing { self.speed() } else { 0.0 };
        let mut fade_mix = std::mem::take(&mut self.fade_mix);
        if cross {
            if fade_mix.len() < CHUNK * 2 {
                fade_mix.resize(CHUNK * 2, 0.0);
            }
            let mut at = 0usize;
            let mut faded = |tag: Tag, a: &[f32]| {
                let n = a.len();
                fade_mix[..n].copy_from_slice(a);
                let frames = fade.len() / 2;
                for (i, frame) in fade_mix[..n].chunks_exact_mut(2).enumerate() {
                    let k = at + i;
                    if k < frames {
                        // Equal-power: the paths are correlated, not identical.
                        let g = (k as f32 + 0.5) / frames as f32 * std::f32::consts::FRAC_PI_2;
                        frame[0] = fade[k * 2] * g.cos() + frame[0] * g.sin();
                        frame[1] = fade[k * 2 + 1] * g.cos() + frame[1] * g.sin();
                    }
                }
                at += n / 2;
                emit(tag, &fade_mix[..n]);
            };
            match path {
                Path::Silence => self.emit_silence(&mut faded),
                Path::Direct => self.render_direct(&mut faded),
                Path::Vari => self.render_vari(&mut faded),
                Path::Stretch => self.render_stretch(&mut faded),
            }
        } else {
            match path {
                Path::Silence => self.emit_silence(emit),
                Path::Direct => self.render_direct(emit),
                Path::Vari => self.render_vari(emit),
                Path::Stretch => self.render_stretch(emit),
            }
        }
        self.fade = fade;
        self.fade_mix = fade_mix;
        self.shadow += start_shadow_step * CHUNK as f64;
        // End of track (outside a loop).
        if self.playing
            && !self.loop_on
            && self
                .pcm
                .as_ref()
                .is_some_and(|p| p.done.load(std::sync::atomic::Ordering::Acquire))
            && self.pos >= self.len()
        {
            self.playing = false;
            self.previewing = false;
            self.end = true;
        }
    }

    fn emit_silence(&mut self, emit: &mut (impl FnMut(Tag, &[f32]) + ?Sized)) {
        self.inter.fill(0.0);
        emit(
            Tag {
                bundle: false,
                generation: self.generation,
                frames: CHUNK as u32,
                src: self.pos,
                step: 0.0,
            },
            &self.inter,
        );
    }

    /// Source frames until the next wrap point (loop end or track end).
    pub(super) fn limit(&self) -> f64 {
        match (self.loop_on, self.looping) {
            (true, Some((_, b))) => b.round(),
            _ => self.len().max(self.pos),
        }
    }

    /// Reach the hand within ~40 ms; ease the speed so it never zips.
    fn steer_scratch(&mut self) {
        if let (Some(target), Some(speed)) = (self.scratch_target, self.scratching) {
            let wanted = ((target - self.pos) / (self.rate as f64 * 0.04)).clamp(-4.0, 4.0);
            self.scratching = Some(speed + (wanted - speed) * 0.35);
        }
    }

    fn render_scratched(&mut self, emit: &mut impl FnMut(Tag, &[f32])) {
        let mut sinc = std::mem::take(&mut self.sinc);
        let step = if self.playing { self.speed() } else { 0.0 };
        let mut buf = std::mem::take(&mut self.inter);
        if buf.len() < CHUNK * 2 {
            buf.resize(CHUNK * 2, 0.0);
        }
        let start = self.pos;
        for i in 0..CHUNK {
            let position = self.wrap(self.pos);
            for c in 0..2 {
                buf[i * 2 + c] = if self.playing {
                    sinc.at(position, step, |index| self.sample(index, c))
                } else {
                    0.0
                };
            }
            self.pos = self.wrap(self.pos + step);
        }
        self.shadow += if self.playing {
            (1.0 + self.tempo) * (1.0 + self.bend) * CHUNK as f64
        } else {
            0.0
        };
        emit(
            Tag {
                bundle: false,
                generation: self.generation,
                frames: CHUNK as u32,
                src: start,
                step,
            },
            &buf[..CHUNK * 2],
        );
        self.sinc = sinc;
        self.inter = buf;
        if !self.loop_on && self.pos < 0.0 {
            self.pos = 0.0;
            self.playing = false;
            self.end = true;
        }
    }

    fn render_direct(&mut self, emit: &mut (impl FnMut(Tag, &[f32]) + ?Sized)) {
        let mut done = 0usize;
        let mut buf = std::mem::take(&mut self.inter);
        while done < CHUNK {
            let limit = self.limit();
            let k = ((limit - self.pos).max(0.0) as usize).min(CHUNK - done);
            if k == 0 {
                if self.loop_on && self.looping.is_some() {
                    self.pos = self.wrap(self.pos);
                    continue;
                }
                // Past the end: silence for the rest of the chunk.
                buf[..(CHUNK - done) * 2].fill(0.0);
                emit(
                    Tag {
                        bundle: false,
                        generation: self.generation,
                        frames: (CHUNK - done) as u32,
                        src: self.pos,
                        step: 0.0,
                    },
                    &buf[..(CHUNK - done) * 2],
                );
                break;
            }
            if let Some(p) = &self.pcm {
                p.read(self.pos as usize, &mut buf[..k * 2]);
            }
            emit(
                Tag {
                    bundle: false,
                    generation: self.generation,
                    frames: k as u32,
                    src: self.pos,
                    step: 1.0,
                },
                &buf[..k * 2],
            );
            self.pos += k as f64;
            done += k;
        }
        self.inter = buf;
    }

    fn render_vari(&mut self, emit: &mut (impl FnMut(Tag, &[f32]) + ?Sized)) {
        let step = self.speed();
        let mut done = 0usize;
        let mut buf = std::mem::take(&mut self.inter);
        let mut sinc = std::mem::take(&mut self.sinc);
        while done < CHUNK {
            let limit = self.limit();
            // Frames until the position crosses the wrap point.
            let until = ((limit - self.pos) / step).ceil().max(0.0) as usize;
            let k = until.min(CHUNK - done);
            if k == 0 {
                if self.loop_on && self.looping.is_some() {
                    self.pos = self.wrap(self.pos);
                    continue;
                }
                buf[..(CHUNK - done) * 2].fill(0.0);
                emit(
                    Tag {
                        bundle: false,
                        generation: self.generation,
                        frames: (CHUNK - done) as u32,
                        src: self.pos,
                        step: 0.0,
                    },
                    &buf[..(CHUNK - done) * 2],
                );
                break;
            }
            let start = self.pos;
            let me = &*self;
            // Read directly up to the loop end (the mapping is the identity
            // below it) or the decoded end; the edges go through `sample`.
            let until = match (me.loop_on, me.looping) {
                (true, Some((_, b))) => b.round() as i64,
                _ => i64::MAX,
            };
            let end = match me.pcm.as_ref().and_then(|p| p.published()) {
                Some(x) => sinc.render_from(&mut buf[..k * 2], start, step, x, until, |i, c| {
                    me.sample(i, c)
                }),
                None => {
                    // A 16-bit store: the frames this chunk reads, as float,
                    // once each; positions relative to the window's start.
                    let half = crate::deck::sinc::TAPS as f64 / 2.0;
                    let first = (start - half).floor().max(0.0) as usize;
                    let last = (start + k as f64 * step + half + 2.0).ceil() as usize;
                    let n = last.saturating_sub(first);
                    let mut win = std::mem::take(&mut self.window);
                    win.resize(n * 2, 0.0);
                    let real = self.pcm.as_ref().map_or(0, |p| p.read(first, &mut win));
                    let me = &*self;
                    let off = first as f64;
                    let wuntil = until.saturating_sub(first as i64).min(real as i64);
                    let e = sinc.render_from(
                        &mut buf[..k * 2],
                        start - off,
                        step,
                        &win[..real * 2],
                        wuntil,
                        |i, c| me.sample(i + first as i64, c),
                    ) + off;
                    self.window = win;
                    e
                }
            };
            emit(
                Tag {
                    bundle: false,
                    generation: self.generation,
                    frames: k as u32,
                    src: start,
                    step,
                },
                &buf[..k * 2],
            );
            self.pos = self.wrap(end);
            done += k;
        }
        self.sinc = sinc;
        self.inter = buf;
    }

    /// Read `n` mapped source frames from `from` into the planar scratch.
    fn feed(&mut self, from: f64, n: usize) {
        let base = from.floor() as i64;
        for i in 0..n {
            self.l[i] = self.sample(base + i as i64, 0);
            self.r[i] = self.sample(base + i as i64, 1);
        }
    }

    fn render_stretch(&mut self, emit: &mut (impl FnMut(Tag, &[f32]) + ?Sized)) {
        let (speed, pitch) = (self.speed(), self.pitch());
        if self.stretch.is_none() {
            self.stretch = Some(Stretcher::new(self.rate));
        }
        // Taken out so `process`/`retrieve` can borrow `l`/`r` without copying
        // them into a fresh buffer.
        let mut st = self.stretch.take().unwrap();
        if !self.primed {
            // Restart the stretcher exactly at `pos`: feed its preferred
            // pad before the position and drop its start delay.
            st.reset();
            st.set(speed, pitch);
            let pad = st.start_pad().min(self.l.len());
            self.discard = st.start_delay();
            let from = self.pos - pad as f64;
            self.feed(from, pad);
            st.process(&self.l[..pad], &self.r[..pad]);
            self.rb_in = self.pos;
            self.primed = true;
        }
        st.set(speed, pitch);
        // Fill until a whole chunk (after the start delay) is ready.
        let mut guard = 0;
        while st.available() < CHUNK + self.discard && guard < 64 {
            let n = st.required().clamp(64, 2048).min(self.l.len());
            let from = self.rb_in;
            self.feed(from, n);
            st.process(&self.l[..n], &self.r[..n]);
            self.rb_in = self.wrap(self.rb_in + n as f64);
            guard += 1;
        }
        // Drop the start delay, then take one chunk, into the planar scratch.
        while self.discard > 0 {
            let n = self.discard.min(self.l.len());
            let got = st.retrieve(&mut self.l[..n], &mut self.r[..n]);
            if got == 0 {
                break;
            }
            self.discard -= got;
        }
        let got = st.retrieve(&mut self.l[..CHUNK], &mut self.r[..CHUNK]);
        self.stretch = Some(st);
        let mut buf = std::mem::take(&mut self.inter);
        for i in 0..CHUNK {
            let (a, b) = if i < got {
                (self.l[i], self.r[i])
            } else {
                (0.0, 0.0)
            };
            buf[i * 2] = a;
            buf[i * 2 + 1] = b;
        }
        // Tags: split where the playhead wraps at the loop end.
        let mut done = 0usize;
        while done < CHUNK {
            let limit = self.limit();
            let until = ((limit - self.pos) / speed).ceil().max(0.0) as usize;
            let k = until
                .min(CHUNK - done)
                .max(if self.loop_on { 0 } else { CHUNK - done });
            if k == 0 {
                self.pos = self.wrap(self.pos);
                continue;
            }
            emit(
                Tag {
                    bundle: false,
                    generation: self.generation,
                    frames: k as u32,
                    src: self.pos,
                    step: speed,
                },
                &buf[done * 2..(done + k) * 2],
            );
            self.pos = self.wrap(self.pos + k as f64 * speed);
            done += k;
        }
        self.inter = buf;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// Dragging the waveform: the deck follows the hand (here a steady
    /// drag forward, then a stop) without jumps, and rests where the hand
    /// stopped, so a cue can be set there.
    #[test]
    fn a_hand_drag_is_followed_smoothly_and_stops_on_the_spot() {
        let p = ramp(RATE as usize * 6);
        let mut t = Transport::new(RATE);
        t.apply(Cmd::Load(p), None);
        t.apply(Cmd::Seek(1.0), None);
        // Touch: hold at 1 s.
        t.apply(
            Cmd::Scratch {
                seconds: 1.0,
                velocity: 0.0,
            },
            None,
        );
        let mut out = vec![];
        let mut hand = 1.0f64;
        for chunk in 0..400 {
            // The mouse reports every ~3 chunks (≈16 ms), moving 0.5 s per
            // second of real time for 200 chunks, then stays put.
            if chunk % 3 == 0 {
                if chunk < 200 {
                    hand = 1.0 + chunk as f64 * CHUNK as f64 / RATE as f64 * 0.5;
                }
                t.apply(
                    Cmd::Scratch {
                        seconds: hand,
                        velocity: 0.0,
                    },
                    None,
                );
            }
            t.render(&mut |_, a| out.extend(a.chunks_exact(2).map(|f| f[0])));
        }
        // The ramp source: each output sample names the frame it played.
        let jumps = out
            .windows(2)
            .map(|w| ((w[1] - w[0]) * 1e6).abs())
            .fold(0.0f32, f32::max);
        assert!(
            jumps < 4.0,
            "the playhead jumped {jumps} frames in one sample"
        );
        let rest = *out.last().unwrap() as f64 * 1e6 / RATE as f64;
        assert!(
            (rest - hand).abs() < 0.005,
            "stopped at {rest:.3} s, hand at {hand:.3} s"
        );
    }

    /// Diagnostic: a real track with keylock while the tempo is dragged
    /// (+0.05 % per chunk to +3 %, back to 0), against steady playback:
    /// clipping, sample jumps and high-frequency bursts per 20 ms.
    /// PATCH_TEST_TRACK=… cargo test --release -p patch-engine tempo_drag -- --ignored --nocapture
    #[test]
    #[ignore]
    fn tempo_drag_quality() {
        let path = std::env::var("PATCH_TEST_TRACK").unwrap();
        let out = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-ss", "60", "-t", "30", "-i", &path])
            .args(["-ac", "2", "-ar", "48000", "-f", "f32le", "-"])
            .output()
            .unwrap();
        let x: Vec<f32> = out
            .stdout
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        let pcm = Pcm::from_samples(RATE, x);
        let run = |drag: bool| -> Vec<f32> {
            let mut t = Transport::new(RATE);
            t.apply(Cmd::Load(pcm.clone()), None);
            t.apply(Cmd::Keylock(true), None);
            t.apply(Cmd::Play, None);
            let mut out = vec![];
            let mut tempo = 0.0f64;
            let mut chunk = 0usize;
            while out.len() < RATE as usize * 2 * 20 {
                if drag && chunk >= 200 {
                    let phase = chunk - 200;
                    tempo = if phase < 60 {
                        phase as f64 * 0.0005
                    } else if phase < 120 {
                        (120 - phase) as f64 * 0.0005
                    } else {
                        0.0
                    };
                    t.apply(Cmd::Tempo(tempo), None);
                    // Key shifts too: +1, +2, back to 0 semitones.
                    match phase {
                        150 => t.apply(Cmd::Key(1), None),
                        190 => t.apply(Cmd::Key(2), None),
                        230 => t.apply(Cmd::Key(0), None),
                        _ => {}
                    }
                }
                t.render(&mut |_, a| out.extend_from_slice(a));
                chunk += 1;
            }
            let _ = tempo;
            out
        };
        for (name, y) in [("steady", run(false)), ("drag", run(true))] {
            let mono: Vec<f32> = y.chunks_exact(2).map(|f| f[0]).collect();
            let clip = y.iter().filter(|v| v.abs() >= 0.999).count();
            let jump = mono
                .windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0f32, f32::max);
            // High band (one-pole difference) energy per 20 ms window.
            let hf: Vec<f64> = mono
                .windows(2)
                .map(|w| f64::from(w[1] - w[0]).powi(2))
                .collect::<Vec<_>>()
                .chunks(960)
                .map(|c| c.iter().sum::<f64>() / c.len() as f64)
                .collect();
            let mut sorted = hf.clone();
            sorted.sort_by(f64::total_cmp);
            let med = sorted[sorted.len() / 2].max(1e-12);
            let worst = hf.iter().copied().fold(0.0f64, f64::max);
            // In the drag region (chunks 200..340 → 4.3..7.3 s).
            let region = &hf[(4.2 * 50.0) as usize..(7.5 * 50.0) as usize];
            let worst_region = region.iter().copied().fold(0.0f64, f64::max);
            // The biggest jumps, with when they happen (chunk index).
            let mut jumps: Vec<(f32, usize)> = mono
                .windows(2)
                .enumerate()
                .map(|(i, w)| ((w[1] - w[0]).abs(), i / CHUNK))
                .collect();
            jumps.sort_by(|a, b| b.0.total_cmp(&a.0));
            println!("{name:6}: top jumps (size, chunk) {:?}", &jumps[..6]);
            println!(
                "{name:6}: clipped {clip}, max jump {jump:.3}, HF worst/median {:.1}x (drag window {:.1}x)",
                worst / med,
                worst_region / med
            );
        }
    }

    /// A ramp track: frame i holds i (as f32 / 1e6) on both channels, so
    /// output samples reveal exactly which source frame played.
    fn ramp(frames: usize) -> Arc<Pcm> {
        Pcm::from_samples(
            RATE,
            (0..frames)
                .flat_map(|i| [i as f32 * 1e-6, i as f32 * 1e-6])
                .collect(),
        )
    }

    fn collect(t: &mut Transport, chunks: usize) -> (Vec<Tag>, Vec<f32>) {
        let (mut tags, mut audio) = (vec![], vec![]);
        for _ in 0..chunks {
            t.render(&mut |tag, a| {
                tags.push(tag);
                audio.extend_from_slice(a);
            });
        }
        (tags, audio)
    }

    fn deck(frames: usize) -> Transport {
        let mut t = Transport::new(RATE);
        t.apply(Cmd::Load(ramp(frames)), None);
        t.apply(
            Cmd::Grid(Some(Grid {
                bpm: 120.0,
                offset: 0.0,
                downbeat: None,
            })),
            None,
        );
        t
    }

    #[test]
    fn an_unchanged_keylock_does_not_restart_a_running_stretcher() {
        let mut t=deck(RATE as usize*8);
        t.apply(Cmd::Keylock(true),None);
        t.apply(Cmd::Tempo(0.05),None);
        t.apply(Cmd::Play,None);
        collect(&mut t,40);
        assert!(t.primed);
        let generation=t.generation;
        t.apply(Cmd::Keylock(true),None);
        assert!(t.primed,"a repeated pitch update must not re-prime R3");
        t.apply(Cmd::Bend(0.04),None);
        assert!(t.primed);
        assert_eq!(t.generation,generation);
        t.apply(Cmd::Keylock(false),None);
        assert!(!t.primed,"an actual path change still re-primes");
    }

    #[test]
    fn a_gated_hot_cue_plays_while_held_and_returns_paused() {
        let mut t = deck(RATE as usize * 8);
        t.apply(Cmd::Cue, Some(0.5 * RATE as f64)); // main cue at 0.5 s
        let cue = t.cue_point;
        t.apply(Cmd::GateCue(3.0), None);
        assert!(t.playing && t.previewing);
        assert_eq!(t.pos, 3.0 * RATE as f64);
        t.apply(Cmd::CueRelease, None);
        assert!(!t.playing && !t.previewing);
        assert_eq!(t.pos, 3.0 * RATE as f64, "back to the hot cue, not the main cue");
        assert_eq!(t.cue_point, cue, "the main cue is untouched");

        // Play while held keeps playing after release.
        t.apply(Cmd::GateCue(2.0), None);
        t.apply(Cmd::Play, None);
        t.apply(Cmd::CueRelease, None);
        assert!(t.playing && !t.previewing);

        // Playing: a hot cue jumps and plays on.
        t.apply(Cmd::GateCue(1.0), None);
        assert!(t.playing && !t.previewing);
        assert_eq!(t.pos, RATE as f64);
        t.apply(Cmd::CueRelease, None);
        assert!(t.playing);

        // The main cue's own preview still returns to the main cue.
        t.apply(Cmd::Pause, None);
        t.apply(Cmd::GateCue(4.0), None);
        t.apply(Cmd::CueRelease, None);
        t.apply(Cmd::Seek(0.5), None);
        t.apply(Cmd::Cue, Some(0.5 * RATE as f64));
        assert!(t.previewing);
        t.apply(Cmd::CueRelease, None);
        assert_eq!(t.pos, 0.5 * RATE as f64);
    }

    #[test]
    fn memory_recall_is_exact_paused_and_does_not_quantize() {
        let mut t = deck(RATE as usize * 8);
        t.apply(Cmd::Play, None);
        t.apply(Cmd::AutoLoop(1.0), Some(0.0));
        t.apply(Cmd::RecallCue(1.2345), None);
        assert!(!t.playing && !t.loop_on && !t.previewing);
        assert_eq!(t.pos, 1.2345 * RATE as f64);
        assert_eq!(t.cue_point, Some(t.pos));
        t.apply(Cmd::Cue, None);
        assert!(t.previewing && t.playing);
        t.apply(Cmd::CueRelease, None);
        assert_eq!(t.pos, 1.2345 * RATE as f64);
        assert!(!t.playing);
    }

    #[test]
    fn saved_loop_recall_and_boundary_edits_keep_valid_source_bounds() {
        let mut t = deck(RATE as usize * 8);
        t.apply(Cmd::LoopSet { from: 1.2345, to: 2.3456, play: false }, None);
        assert!(t.loop_on && !t.playing);
        assert_eq!(t.looping, Some((1.2345 * RATE as f64, 2.3456 * RATE as f64)));
        t.apply(Cmd::LoopAdjust { out: true, seconds: 0.01 }, None);
        let (a,b)=t.looping.unwrap();
        assert!((b-2.3556 * RATE as f64).abs()<1e-8);
        t.apply(Cmd::LoopAdjust { out: false, seconds: -10.0 }, None);
        assert_eq!(t.looping.unwrap().0, 0.0);
        t.apply(Cmd::LoopAdjust { out: true, seconds: -10.0 }, None);
        assert_eq!(t.looping, Some((0.0,64.0)));
        t.apply(Cmd::LoopAdjust { out: false, seconds: 10.0 }, None);
        assert_eq!(t.looping, Some((0.0,64.0)));
        assert!(a>0.0);
        t.apply(Cmd::LoopSet { from: 99.0, to: 100.0, play: true }, None);
        assert_eq!(t.looping, Some((0.0,64.0)), "outside the track is ignored");
        t.looping=Some((1e9,1e10));
        t.apply(Cmd::LoopAdjust {out:true,seconds:0.1},None);
        let (a,b)=t.looping.unwrap();
        assert!(a>=0.0 && b<=t.len() && b-a>=64.0);
    }

    #[test]
    fn held_slip_reverse_returns_to_forward_timeline_and_restores_slip() {
        for slip in [false,true] {
            let mut t=deck(RATE as usize * 20);
            t.apply(Cmd::Seek(4.0),None);
            t.apply(Cmd::Slip(slip),Some(4.0 * RATE as f64));
            t.apply(Cmd::Play,None);
            t.apply(Cmd::Censor(true),Some(4.0 * RATE as f64));
            assert!(t.reverse && t.slip && t.censor_slip==Some(slip));
            collect(&mut t,20);
            let shadow=t.shadow;
            t.apply(Cmd::Censor(false),None);
            assert!(!t.reverse && t.censor_slip.is_none());
            assert_eq!(t.slip,slip);
            assert_eq!(t.pos,shadow);
            assert!(shadow>4.0 * RATE as f64);
            t.apply(Cmd::Censor(false),None);
            assert_eq!(t.pos,shadow,"a duplicate release has no effect");
        }
    }

    #[test]
    fn eject_clears_reverse_loop_and_cue_before_another_source() {
        let mut t=deck(RATE as usize * 8);
        t.apply(Cmd::Seek(2.0),None);
        t.apply(Cmd::LoopIn,Some(2.0 * RATE as f64));
        assert_eq!(t.cue_point,Some(2.0 * RATE as f64));
        t.apply(Cmd::Censor(true),None);
        t.apply(Cmd::Eject,None);
        assert!(!t.reverse && !t.loop_on && !t.previewing && !t.slip);
        assert!(t.censor_slip.is_none() && t.pending_in.is_none() && t.cue_point.is_none());
        t.apply(Cmd::Load(Pcm::from_samples(RATE,vec![0.0;RATE as usize*2])),None);
        assert!(!t.reverse && t.looping.is_none() && t.cue_point.is_none());
    }

    #[test]
    fn direct_play_is_bit_exact_and_positions_advance_by_frames() {
        let mut t = deck(RATE as usize * 4);
        t.apply(Cmd::Play, None);
        let (tags, audio) = collect(&mut t, 8);
        let mut expect = 0.0;
        for tag in &tags {
            assert_eq!(tag.src, expect, "positions advance exactly by frames");
            assert_eq!(tag.step, 1.0);
            expect += tag.frames as f64;
        }
        for (i, f) in audio.as_chunks::<2>().0.iter().enumerate() {
            assert_eq!(f[0], i as f32 * 1e-6, "frame {i} bit-exact");
        }
    }

    #[test]
    fn stretched_transient_matches_its_tagged_source_position() {
        let mut samples = vec![0.0f32; RATE as usize * 3 * 2];
        for i in RATE as usize..RATE as usize + 128 {
            samples[i * 2] = 0.8;
            samples[i * 2 + 1] = 0.8;
        }
        let mut t = Transport::new(RATE);
        t.apply(Cmd::Load(Pcm::from_samples(RATE, samples)), None);
        t.apply(Cmd::Tempo(-0.02), None);
        t.apply(Cmd::Play, None);
        let (tags, audio) = collect(&mut t, 240);
        let (i, _) = audio
            .as_chunks::<2>()
            .0
            .iter()
            .enumerate()
            .max_by(|a, b| a.1[0].total_cmp(&b.1[0]))
            .unwrap();
        let tag = &tags[i / CHUNK];
        let source = tag.src + (i % CHUNK) as f64 * tag.step;
        let error_ms = (source - RATE as f64) / RATE as f64 * 1000.0;
        assert!(
            error_ms.abs() < 10.0,
            "stretched transient tag error {error_ms:.1} ms"
        );
    }

    #[test]
    fn loop_wraps_sample_accurately() {
        let mut t = deck(RATE as usize * 4);
        t.apply(Cmd::Seek(0.25), None);
        t.apply(Cmd::Play, None);
        // 1-beat loop at 120 BPM from the beat at 0.0 s? From 0.25 s the
        // beat below is 0.0 s: loop [0, 24 000).
        t.apply(Cmd::AutoLoop(1.0), Some(0.25 * RATE as f64));
        let (_, audio) = collect(&mut t, 200);
        let played: Vec<f32> = audio.as_chunks::<2>().0.iter().map(|f| f[0]).collect();
        let at = played
            .iter()
            .position(|v| (*v - 23_999e-6).abs() < 1e-9)
            .expect("reaches the loop end");
        assert!(
            (played[at + 1] - 0.0).abs() < 1e-9,
            "the frame after the loop end is the loop start: {}",
            played[at + 1]
        );
    }

    #[test]
    fn reloop_recalls_manual_bounds_and_wraps_bit_exactly_without_a_grid() {
        let mut t = deck(RATE as usize * 2);
        t.apply(Cmd::Grid(None), None);
        t.apply(Cmd::LoopIn, Some(12_345.0));
        t.apply(Cmd::LoopOut, Some(34_567.0));
        let bounds = t.looping;
        t.apply(Cmd::LoopExit, None);
        t.apply(Cmd::Seek(1.5), None);
        let old_generation = t.generation;
        t.apply(Cmd::Reloop, Some(72_000.0));
        assert!(t.loop_on && t.playing);
        assert_eq!(t.looping, bounds, "no replacement four-beat loop");
        assert_eq!(t.pos, 12_345.0);
        assert!(
            t.generation > old_generation,
            "discard rendered-ahead audio"
        );
        let (tags, audio) = collect(&mut t, 200);
        assert_eq!(tags[0].src, 12_345.0);
        for (i, frame) in audio.as_chunks::<2>().0.iter().enumerate() {
            let source = 12_345 + i % (34_567 - 12_345);
            assert_eq!(frame[0], source as f32 * 1e-6, "source frame {source}");
            assert_eq!(frame[1], frame[0]);
        }
    }

    #[test]
    fn reloop_preserves_the_last_auto_loop_after_scaling_and_leaving_it() {
        let mut t = deck(RATE as usize * 20);
        t.apply(Cmd::Play, None);
        t.apply(Cmd::AutoLoop(8.0), Some(108_000.0));
        t.apply(Cmd::LoopScale(2.0), None);
        let bounds = t.looping;
        assert_eq!(bounds, Some((96_000.0, 480_000.0)));
        t.apply(Cmd::Reloop, None);
        assert!(!t.loop_on && t.playing, "EXIT continues normal playback");
        t.apply(Cmd::Seek(12.0), None);
        t.apply(Cmd::Reloop, None);
        assert!(t.loop_on && t.playing);
        assert_eq!(t.looping, bounds);
        assert_eq!(t.pos, 96_000.0, "RELOOP starts at the original in-point");
    }

    #[test]
    fn reloop_exit_keeps_the_slip_timeline_even_when_started_paused() {
        let mut t = deck(RATE as usize * 20);
        t.apply(Cmd::Slip(true), Some(0.0));
        t.apply(Cmd::Play, None);
        t.apply(Cmd::AutoLoop(1.0), Some(0.0));
        collect(&mut t, 400);
        t.apply(Cmd::Reloop, None);
        assert!(!t.loop_on);
        assert_eq!(t.pos, 400.0 * CHUNK as f64);
        t.apply(Cmd::Pause, None);
        t.apply(Cmd::Seek(3.0), None);
        t.apply(Cmd::Reloop, None);
        assert!(t.playing && t.loop_on);
        assert_eq!(t.pos, 0.0);
        collect(&mut t, 10);
        t.apply(Cmd::Reloop, None);
        assert_eq!(t.pos, 3.0 * RATE as f64 + 10.0 * CHUNK as f64);
    }

    #[test]
    fn reloop_never_invents_a_loop_or_reuses_another_tracks_bounds() {
        let mut t = deck(RATE as usize * 4);
        t.apply(Cmd::Reloop, None);
        assert!(!t.playing && !t.loop_on && t.looping.is_none());
        t.apply(Cmd::AutoLoop(4.0), Some(0.0));
        t.apply(Cmd::LoopExit, None);
        t.apply(Cmd::Eject, None);
        t.apply(Cmd::Reloop, None);
        assert!(!t.playing && !t.loop_on);
        t.apply(Cmd::Load(ramp(RATE as usize * 4)), None);
        t.apply(Cmd::Reloop, None);
        assert!(!t.playing && !t.loop_on && t.looping.is_none());
        assert_eq!(t.pos, 0.0);
    }

    #[test]
    fn loop_wraps_in_varispeed_too() {
        let mut t = deck(RATE as usize * 4);
        t.apply(Cmd::Keylock(false), None);
        t.apply(Cmd::Tempo(0.08), None);
        t.apply(Cmd::Play, None);
        t.apply(Cmd::AutoLoop(1.0), Some(0.0));
        let (tags, _) = collect(&mut t, 400);
        for tag in tags.iter().filter(|t| t.step > 0.0) {
            let end = tag.src + tag.step * (tag.frames as f64 - 1.0);
            assert!(
                tag.src >= 0.0 && end < 24_000.0 + 1e-6,
                "every tagged position stays in the loop: {} … {}",
                tag.src,
                end
            );
        }
        let wraps = tags.windows(2).filter(|w| w[1].src < w[0].src).count();
        assert!(wraps >= 2, "it looped ({wraps} wraps)");
    }

    #[test]
    fn a_compact_track_plays_like_the_float_one() {
        // Music-like: a few partials, about −6 dBFS.
        let frames = RATE as usize * 6;
        let x: Vec<f32> = (0..frames)
            .flat_map(|i| {
                let t = i as f32 / RATE as f32;
                let v = 0.25 * (t * 110.0 * std::f32::consts::TAU).sin()
                    + 0.15 * (t * 1_730.0 * std::f32::consts::TAU).sin()
                    + 0.05 * (t * 9_100.0 * std::f32::consts::TAU).sin();
                [v, -v * 0.8]
            })
            .collect();
        let grid = || {
            Some(Grid {
                bpm: 120.0,
                offset: 0.0,
                downbeat: None,
            })
        };
        for (tempo, looped, keylock) in [
            (0.0, false, false),
            (0.03, false, false),
            (0.03, true, false),
            (0.03, false, true),
        ] {
            let play = |pcm: Arc<Pcm>| {
                let mut t = Transport::new(RATE);
                t.apply(Cmd::Load(pcm), None);
                t.apply(Cmd::Grid(grid()), None);
                t.apply(Cmd::Keylock(keylock), None);
                t.apply(Cmd::Tempo(tempo), None);
                t.apply(Cmd::Play, None);
                if looped {
                    t.apply(Cmd::AutoLoop(1.0), Some(1000.0));
                }
                collect(&mut t, 600).1
            };
            let (a, b) = (
                play(Pcm::from_samples(RATE, x.clone())),
                play(Pcm::from_samples_i16(RATE, x.clone())),
            );
            if keylock {
                // Rubber Band decides adaptively: the same tiny change in a
                // float track (one 16-bit step on every sample) moves its
                // output as much, so compare against that, not zero.
                let nudged: Vec<f32> = x
                    .iter()
                    .enumerate()
                    .map(|(i, v)| v + if i % 3 == 0 { 1.0 / 23_197.0 } else { 0.0 })
                    .collect();
                let c = play(Pcm::from_samples(RATE, nudged));
                let base = a
                    .iter()
                    .zip(&c)
                    .map(|(p, q)| (p - q).abs())
                    .fold(0.0f32, f32::max);
                let worst = a
                    .iter()
                    .zip(&b)
                    .map(|(p, q)| (p - q).abs())
                    .fold(0.0f32, f32::max);
                assert!(
                    worst <= base * 2.0 + 1e-4,
                    "keylock: 16-bit differs {worst}, a float nudge {base}"
                );
                continue;
            }
            assert_eq!(a.len(), b.len());
            let worst = a
                .iter()
                .zip(&b)
                .map(|(p, q)| (p - q).abs())
                .fold(0.0f32, f32::max);
            // 16-bit rounding through the resampler or the stretcher:
            // under −80 dBFS.
            assert!(
                worst < 1e-4,
                "tempo {tempo} loop {looped} keylock {keylock}: worst difference {worst}"
            );
            assert!(a.iter().any(|v| v.abs() > 0.1), "it played");
        }
    }

    #[test]
    fn beat_jump_keeps_phase() {
        let mut t = deck(RATE as usize * 20);
        t.apply(Cmd::Play, None);
        let now = 12_345.0;
        t.apply(Cmd::Jump(4.0), Some(now));
        let generation = t.generation;
        let (tags, _) = collect(&mut t, 1);
        assert_eq!(tags[0].generation, generation);
        assert_eq!(
            tags[0].src,
            now + 4.0 * 24_000.0,
            "4 beats at 120 BPM later, same phase"
        );
    }

    #[test]
    fn jump_in_a_loop_moves_the_loop() {
        let mut t = deck(RATE as usize * 20);
        t.apply(Cmd::Play, None);
        t.apply(Cmd::AutoLoop(4.0), Some(1000.0)); // [0, 96 000)
        t.apply(Cmd::Jump(4.0), Some(5000.0));
        assert_eq!(t.looping, Some((96_000.0, 192_000.0)));
        assert_eq!(t.pos, 101_000.0);
    }

    #[test]
    fn loops_never_start_before_the_track() {
        let mut t = deck(RATE as usize * 20);
        t.apply(
            Cmd::Grid(Some(Grid {
                bpm: 120.0,
                offset: 0.3,
                downbeat: None,
            })),
            None,
        );
        t.apply(Cmd::AutoLoop(4.0), Some(0.0));
        // The beat before 0 s is at −0.2 s; the loop starts at the first
        // beat of the track instead, 0.3 s = frame 14 400.
        assert_eq!(t.looping.unwrap().0, 14_400.0);
    }

    #[test]
    fn cue_sets_on_the_beat_and_returns() {
        let mut t = deck(RATE as usize * 20);
        t.apply(Cmd::Cue, Some(24_100.0)); // paused: set (snaps to 24 000)
        assert_eq!(t.cue_point, Some(24_000.0));
        t.apply(Cmd::Play, None);
        collect(&mut t, 10);
        t.apply(Cmd::Cue, Some(30_000.0)); // playing: back and pause
        assert!(!t.playing);
        assert_eq!(t.pos, 24_000.0);
    }

    #[test]
    fn held_cue_previews_and_returns() {
        let mut t = deck(RATE as usize * 20);
        t.apply(Cmd::Cue, Some(24_000.0)); // paused: set the cue at 0.5 s
        assert_eq!((t.cue_point, t.playing), (Some(24_000.0), false));
        t.apply(Cmd::Cue, Some(24_000.0)); // on the cue point: preview
        assert!(t.playing && t.previewing);
        collect(&mut t, 10);
        t.apply(Cmd::Cue, None); // key repeat while held: nothing changes
        assert!(t.playing && t.previewing);
        t.apply(Cmd::CueRelease, None);
        assert!(!t.playing && !t.previewing);
        assert_eq!(t.pos, 24_000.0, "back on the cue point");
        // Held, then Play: it keeps playing after release.
        t.apply(Cmd::Cue, Some(24_000.0));
        t.apply(Cmd::Play, None);
        t.apply(Cmd::CueRelease, None);
        assert!(t.playing && !t.previewing);
    }

    #[test]
    fn cue_after_the_end_returns_to_the_cue_point() {
        let mut t = deck(RATE as usize);
        t.apply(Cmd::Play, None);
        collect(&mut t, RATE as usize / CHUNK + 4);
        assert!(t.end && !t.playing, "played to the end");
        t.apply(Cmd::Cue, None);
        assert_eq!(t.pos, 0.0);
        assert_eq!(
            t.cue_point, None,
            "no cue was ever set, and that stays true"
        );
        t.apply(Cmd::Play, None);
        assert!(t.playing, "and it plays again");
    }

    #[test]
    fn keylock_stretches_only_an_audible_pitch_change() {
        let mut t = deck(RATE as usize * 10);
        t.apply(Cmd::Play, None);
        // A fine tempo match (0.02 %): the resampler, not the stretcher.
        t.apply(Cmd::Tempo(0.0002), None);
        let _ = collect(&mut t, 2);
        assert!(t.path == Path::Vari);
        // 1 %: the pitch would show, keylock stretches.
        t.apply(Cmd::Tempo(0.01), None);
        let _ = collect(&mut t, 2);
        assert!(t.path == Path::Stretch);
        // Back to 0.2 %: still stretching (hysteresis), at 0.1 % it lets go.
        t.apply(Cmd::Tempo(0.002), None);
        let _ = collect(&mut t, 2);
        assert!(t.path == Path::Stretch);
        t.apply(Cmd::Tempo(0.001), None);
        let _ = collect(&mut t, 2);
        assert!(t.path == Path::Vari);
    }

    #[test]
    fn keylock_stretch_advances_at_speed() {
        let mut t = deck(RATE as usize * 10);
        t.apply(Cmd::Tempo(0.1), None);
        t.apply(Cmd::Play, None);
        let (tags, audio) = collect(&mut t, 40);
        let played: usize = tags.iter().map(|t| t.frames as usize).sum();
        assert_eq!(played, 40 * CHUNK);
        assert_eq!(audio.len(), played * 2);
        let last = tags.last().unwrap();
        let expect = 40.0 * CHUNK as f64 * 1.1;
        assert!((last.src + last.step * last.frames as f64 - expect).abs() < 1e-6);
        assert!((last.step - 1.1).abs() < 1e-12);
    }

    #[test]
    fn slip_returns_where_the_track_would_be() {
        let mut t = deck(RATE as usize * 20);
        t.apply(Cmd::Slip(true), Some(0.0));
        t.apply(Cmd::Play, Some(0.0));
        t.apply(Cmd::AutoLoop(1.0), Some(0.0));
        collect(&mut t, 400); // 102 400 frames of looping a 24 000-frame loop
        t.apply(Cmd::LoopExit, None);
        assert_eq!(
            t.pos,
            400.0 * CHUNK as f64,
            "continues as if it never looped"
        );
    }

    /// Clicks at known source frames, played at `tempo` (keylock on or
    /// off): where each click comes out, against the position the deck
    /// reports for that output sample. Returns the errors, ms (+: the audio
    /// is later than reported).
    fn click_errors(tempo: f64, keylock: bool) -> Vec<f64> {
        let period = RATE as usize / 2; // a click every 0.5 s
        let frames = RATE as usize * 30;
        let mut x = vec![0.0f32; frames * 2];
        let clicks: Vec<usize> = (1..frames / period).map(|k| k * period).collect();
        for &c in &clicks {
            // A sharp, short burst (a kick's attack).
            for i in 0..48 {
                let v = (1.0 - i as f32 / 48.0) * if i % 2 == 0 { 0.9 } else { -0.9 };
                x[(c + i) * 2] = v;
                x[(c + i) * 2 + 1] = v;
            }
        }
        let mut t = Transport::new(RATE);
        t.apply(Cmd::Load(Pcm::from_samples(RATE, x)), None);
        t.apply(
            Cmd::Grid(Some(Grid {
                bpm: 120.0,
                offset: 0.0,
                downbeat: None,
            })),
            None,
        );
        t.apply(Cmd::Keylock(keylock), None);
        t.apply(Cmd::Tempo(tempo), None);
        t.apply(Cmd::Play, None);
        let (tags, audio) = collect(&mut t, (RATE as usize * 25) / CHUNK);
        // Reported source position of every output frame.
        let mut reported = Vec::with_capacity(audio.len() / 2);
        for tag in &tags {
            for i in 0..tag.frames as usize {
                reported.push(tag.src + tag.step * i as f64);
            }
        }
        let mono: Vec<f32> = audio
            .as_chunks::<2>()
            .0
            .iter()
            .map(|f| f[0].abs())
            .collect();
        let speed = 1.0 + tempo;
        let mut errs = vec![];
        // Each click's attack: the first output frame above half its peak,
        // within the window where the report says it should be.
        for &c in clicks.iter().skip(2) {
            let Some(at) = reported.iter().position(|r| *r >= c as f64) else {
                break;
            };
            let (lo, hi) = (
                at.saturating_sub(RATE as usize / 8),
                (at + RATE as usize / 8).min(mono.len()),
            );
            if hi <= lo {
                break;
            }
            let peak = mono[lo..hi].iter().cloned().fold(0.0f32, f32::max);
            if peak < 0.1 {
                continue;
            }
            let Some(heard) = (lo..hi).find(|&i| mono[i] >= peak * 0.5) else {
                continue;
            };
            let _ = speed;
            errs.push((heard as f64 - at as f64) / RATE as f64 * 1e3);
        }
        errs
    }

    #[test]
    fn what_a_deck_plays_is_where_it_says_it_is() {
        for (tempo, keylock) in [
            (0.0, false),
            (-0.0305, false),
            (-0.0305, true),
            (0.02, true),
            (0.06, true),
        ] {
            let e = click_errors(tempo, keylock);
            assert!(e.len() > 20, "{tempo} {keylock}: {} clicks", e.len());
            let mean = e.iter().sum::<f64>() / e.len() as f64;
            let worst = e.iter().cloned().fold(0.0f64, |a, b| a.max(b.abs()));
            assert!(
                worst < 1.0,
                "tempo {tempo} keylock {keylock}: audio {mean:+.2} ms from the reported position (worst {worst:.2})"
            );
        }
    }

    /// R3: scratch, stretch and the stretch crossfade keep the buffers
    /// A held scratch follows the hand to each new target (a paused deck
    /// too). The steering runs before the stems path as well: on a deck
    /// with stems it never ran, and the deck stood still under the hand.
    #[test]
    fn a_scratch_follows_the_hand() {
        let mut t = deck(RATE as usize * 8);
        t.apply(Cmd::Scratch { seconds: 1.0, velocity: 0.0 }, None);
        t.render(&mut |_, _| {});
        let start = t.pos;
        t.apply(Cmd::Scratch { seconds: 2.0, velocity: 0.0 }, None);
        for _ in 0..200 {
            t.render(&mut |_, _| {});
        }
        let target = 2.0 * RATE as f64;
        assert!(t.pos > start + 0.5 * RATE as f64, "stood still: {} -> {}", start, t.pos);
        assert!((t.pos - target).abs() < 0.05 * RATE as f64, "missed the hand: {} vs {target}", t.pos);
        t.apply(Cmd::ScratchEnd, None);
        assert!(!t.playing, "a paused deck rests where released");
    }

    /// `Transport` already owns. Capacity must not climb across chunks.
    #[test]
    fn scratch_stretch_and_crossfade_reuse_transport_buffers() {
        let mut t = deck(RATE as usize * 8);
        t.apply(Cmd::Play, None);
        t.render(&mut |_, _| {});
        let inter = t.inter.capacity();
        let left = t.l.capacity();
        let right = t.r.capacity();
        let fade = t.fade.capacity();
        let fade_mix = t.fade_mix.capacity();
        t.apply(
            Cmd::Scratch {
                seconds: 1.2,
                velocity: 0.4,
            },
            None,
        );
        for _ in 0..40 {
            t.render(&mut |_, a| assert_eq!(a.len() % 2, 0));
        }
        t.apply(Cmd::ScratchEnd, None);
        t.apply(Cmd::Keylock(true), None);
        t.apply(Cmd::Tempo(0.04), None);
        for _ in 0..20 {
            t.render(&mut |_, _| {});
        }
        t.apply(Cmd::Tempo(0.0), None);
        for _ in 0..8 {
            t.render(&mut |_, _| {});
        }
        assert_eq!(t.inter.capacity(), inter);
        assert_eq!(t.l.capacity(), left);
        assert_eq!(t.r.capacity(), right);
        assert_eq!(t.fade.capacity(), fade);
        assert_eq!(t.fade_mix.capacity(), fade_mix);
        assert!(t.fade.capacity() <= CHUNK * 4);
    }
}
