//! Sample-clock drum sequencer. The audio callback only reads atomics and
//! plays a precomputed, original synthesis bank: no allocation, locks or I/O.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
pub const NAMES: [&str; 8] = [
    "Sub kick",
    "Snare",
    "Closed hat",
    "Open hat",
    "Clap",
    "Low tom",
    "Rim",
    "Shaker",
];
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct Step {
    pub velocity: u32,
    pub probability: u32,
    pub repeats: u32,
}
impl Default for Step {
    fn default() -> Self {
        Self {
            velocity: 0,
            probability: 100,
            repeats: 1,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Track {
    pub steps: [Step; 32],
    pub length: u32,
    pub muted: bool,
    pub level: u32,
    pub pan: i32,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Pattern {
    pub bpm: u32,
    pub swing: u32,
    pub level: u32,
    pub tracks: [Track; 8],
}
impl Default for Pattern {
    fn default() -> Self {
        Self::preset(0)
    }
}
impl Pattern {
    pub fn preset(which: usize) -> Self {
        let mut p = Self {
            bpm: [122, 94, 132][which.min(2)],
            swing: if which == 1 { 24 } else { 0 },
            level: 75,
            tracks: std::array::from_fn(|i| Track {
                steps: [Step::default(); 32],
                length: 16,
                muted: false,
                level: 80,
                pan: [0, 0, -18, 22, 8, -12, 15, 30][i],
            }),
        };
        let hits: [&[usize]; 8] = match which {
            1 => [
                &[0, 7, 10],
                &[4, 12],
                &[0, 2, 4, 6, 8, 10, 12, 14],
                &[15],
                &[12],
                &[11],
                &[3, 14],
                &[1, 5, 9, 13],
            ],
            2 => [
                &[0, 6, 8, 14],
                &[4, 12],
                &[0, 2, 3, 6, 8, 10, 11, 14],
                &[7, 15],
                &[4, 12],
                &[5, 13],
                &[2, 10],
                &[1, 3, 5, 7, 9, 11, 13, 15],
            ],
            _ => [
                &[0, 4, 8, 12],
                &[4, 12],
                &[0, 2, 4, 6, 8, 10, 12, 14],
                &[6, 14],
                &[12],
                &[10],
                &[3, 11],
                &[1, 3, 5, 7, 9, 11, 13, 15],
            ],
        };
        for (t, hits) in hits.iter().enumerate() {
            for &s in *hits {
                p.tracks[t].steps[s] = Step {
                    velocity: if s % 4 == 0 { 100 } else { 75 },
                    ..Step::default()
                };
            }
        }
        p
    }
    pub fn validate(&self) -> Result<(), String> {
        if !(40..=240).contains(&self.bpm)
            || self.swing > 60
            || self.level > 100
            || self.tracks.iter().any(|t| {
                !(1..=32).contains(&t.length)
                    || t.level > 100
                    || !(-100..=100).contains(&t.pan)
                    || t.steps.iter().any(|s| {
                        s.velocity > 127 || s.probability > 100 || !(1..=4).contains(&s.repeats)
                    })
            })
        {
            return Err("Invalid sequencer pattern".into());
        }
        Ok(())
    }
}
pub struct Shared {
    control: std::sync::Mutex<()>,
    pub playing: AtomicBool,
    pub position: AtomicU32,
    reset: AtomicU32,
    bpm: AtomicU32,
    swing: AtomicU32,
    level: AtomicU32,
    sync_on: AtomicBool,
    sync_source: AtomicU32,
    steps: [[AtomicU32; 32]; 8],
    lengths: [AtomicU32; 8],
    levels: [AtomicU32; 8],
    pans: [AtomicU32; 8],
    muted: [AtomicBool; 8],
    /// Voices a running pattern (the Pattern Unit) asks for, each at its
    /// graph sample: filled by the pattern scheduler, drained here.
    pub pattern: patch_pattern::rt::Queue,
}
impl Default for Shared {
    fn default() -> Self {
        let s = Self {
            control: std::sync::Mutex::new(()),
            playing: AtomicBool::new(false),
            position: AtomicU32::new(0),
            reset: AtomicU32::new(0),
            bpm: AtomicU32::new(122),
            swing: AtomicU32::new(0),
            level: AtomicU32::new(75),
            sync_on: AtomicBool::new(false),
            sync_source: AtomicU32::new(0),
            steps: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU32::new(0))),
            lengths: std::array::from_fn(|_| AtomicU32::new(16)),
            levels: std::array::from_fn(|_| AtomicU32::new(80)),
            pans: std::array::from_fn(|_| AtomicU32::new(100)),
            muted: std::array::from_fn(|_| AtomicBool::new(false)),
            pattern: patch_pattern::rt::Queue::new(1024),
        };
        s.set(&Pattern::default());
        s
    }
}
impl Shared {
    pub fn set(&self, p: &Pattern) {
        self.bpm.store(p.bpm, Relaxed);
        self.swing.store(p.swing, Relaxed);
        self.level.store(p.level, Relaxed);
        for (i, t) in p.tracks.iter().enumerate() {
            self.lengths[i].store(t.length, Relaxed);
            self.levels[i].store(t.level, Relaxed);
            self.pans[i].store((t.pan + 100) as u32, Relaxed);
            self.muted[i].store(t.muted, Relaxed);
            for (j, s) in t.steps.iter().enumerate() {
                self.steps[i][j].store(s.velocity | s.probability << 8 | s.repeats << 16, Relaxed);
            }
        }
    }
    pub fn pattern(&self) -> Pattern {
        Pattern {
            bpm: self.bpm.load(Relaxed),
            swing: self.swing.load(Relaxed),
            level: self.level.load(Relaxed),
            tracks: std::array::from_fn(|i| Track {
                length: self.lengths[i].load(Relaxed),
                level: self.levels[i].load(Relaxed),
                pan: self.pans[i].load(Relaxed) as i32 - 100,
                muted: self.muted[i].load(Relaxed),
                steps: std::array::from_fn(|j| {
                    let v = self.steps[i][j].load(Relaxed);
                    Step {
                        velocity: v & 255,
                        probability: (v >> 8) & 255,
                        repeats: (v >> 16) & 255,
                    }
                }),
            }),
        }
    }
    /// Sync state: toggle on/off and set source (0=deck-a, 1=deck-b).
    /// When on, the BPM follows the master deck's grid BPM.
    pub fn set_sync(&self, on: bool, source: u32) {
        self.sync_on.store(on, Relaxed);
        self.sync_source.store(source, Relaxed);
    }
    pub fn sync_on(&self) -> bool {
        self.sync_on.load(Relaxed)
    }
    pub fn sync_source(&self) -> u32 {
        self.sync_source.load(Relaxed)
    }
    /// Set BPM from an external source (used by sync).
    pub fn set_bpm(&self, bpm: u32) {
        self.bpm.store(bpm, Relaxed);
    }
    /// Current BPM (read by the daemon's syncer thread).
    pub fn bpm(&self) -> u32 {
        self.bpm.load(Relaxed)
    }
    pub fn request(&self, req: &Value) -> Result<Value, String> {
        let path =
            std::path::PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap_or_else(|| {
                std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                    .join(".config")
                    .into_os_string()
            }))
            .join("patch/sequencer.json");
        self.request_at(req, &path)
    }
    fn request_at(&self, req: &Value, path: &std::path::Path) -> Result<Value, String> {
        let _control = self
            .control
            .lock()
            .map_err(|_| "Sequencer control unavailable")?;
        if let Some(p) = req.get("pattern") {
            let p: Pattern = serde_json::from_value(p.clone()).map_err(|e| e.to_string())?;
            p.validate()?;
            self.set(&p);
        }
        if let Some(play) = req["playing"].as_bool() {
            self.playing.store(play, Relaxed);
        }
        if req["stop"] == true {
            self.playing.store(false, Relaxed);
            self.reset.fetch_add(1, Relaxed);
            self.position.store(0, Relaxed);
        }
        if req["save"] == true {
            std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
            let temp = path.with_extension("tmp");
            std::fs::write(&temp, serde_json::to_vec_pretty(&self.pattern()).unwrap())
                .map_err(|e| e.to_string())?;
            std::fs::rename(temp, path).map_err(|e| e.to_string())?;
        }
        if req["load"] == true {
            let p: Pattern =
                serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            p.validate()?;
            self.set(&p);
        }
        let mut reply = json!({"playing":self.playing.load(Relaxed), "position":self.position.load(Relaxed), "saved":req["save"]==true});
        if req["status_only"] != true {
            reply["pattern"] = json!(self.pattern());
        }
        Ok(reply)
    }
}
#[derive(Clone, Copy, Default)]
struct Voice {
    track: usize,
    pos: f64,
    gain: f32,
    choked: bool,
    active: bool,
    /// Triggered by a running pattern (the Pattern Unit) rather than by a
    /// step: it renders on the pattern's lane, not the step Sequencer's.
    from_pattern: bool,
}
/// Where the deck the Sequencer is synced to is, at the start of a block:
/// sixteenths since its first downbeat (beats since its grid's first beat
/// when the downbeat is unknown), and output frames per sixteenth (the
/// deck's tempo fader and bend included).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Follow {
    pub sixteenths: f64,
    pub frames_per_sixteenth: f64,
}

pub struct Sequencer {
    bank: [Vec<f32>; 8],
    /// Synced: the steps follow this deck position (set every block);
    /// None: the Sequencer keeps its own clock.
    follow: Option<Follow>,
    /// The last step started on the followed position.
    follow_step: Option<i64>,
    voices: [Voice; 48],
    tick: u64,
    remaining: f64,
    duration: f64,
    repeats: [u32; 8],
    velocity: [f32; 8],
    phase: f64,
    reset: u32,
    rng: u32,
    was_playing: bool,
    master: [f32; 2],
    track_gain: [f32; 8],
    track_pan: [f32; 8],
    rate: f64,
    /// Pattern voices due in this block (time order), and whether pattern
    /// voices are sounding (they play while the step sequencer is stopped).
    due: [patch_pattern::rt::RtEvent; 64],
    due_n: usize,
    pattern_live: bool,
    /// Derived-material events due in this block, for the sampler (the
    /// queue is shared, and this is its one consumer).
    samples: [patch_pattern::rt::RtEvent; 32],
    samples_n: usize,
}
impl Default for Sequencer {
    fn default() -> Self {
        Self {
            bank: std::array::from_fn(sound),
            follow: None,
            follow_step: None,
            voices: [Voice::default(); 48],
            tick: 0,
            remaining: 0.0,
            duration: 1.0,
            repeats: [0; 8],
            velocity: [0.0; 8],
            phase: 0.0,
            reset: 0,
            rng: 1234567,
            was_playing: false,
            master: [0.0; 2],
            track_gain: [0.0; 8],
            track_pan: [0.0; 8],
            rate: 48_000.0,
            due: [patch_pattern::rt::RtEvent::default(); 64],
            due_n: 0,
            pattern_live: false,
            samples: [patch_pattern::rt::RtEvent::default(); 32],
            samples_n: 0,
        }
    }
}
impl Sequencer {
    fn random(&mut self) -> u32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        self.rng
    }
    fn trigger(&mut self, t: usize, from_pattern: bool) {
        if t == 2 {
            for v in &mut self.voices {
                if v.track == 3 {
                    v.choked = true;
                }
            }
        }
        let slot = self
            .voices
            .iter()
            .position(|v| !v.active)
            .unwrap_or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .max_by(|(_, a), (_, b)| a.pos.total_cmp(&b.pos))
                    .unwrap()
                    .0
            });
        self.voices[slot] = Voice {
            track: t,
            pos: 0.0,
            gain: self.velocity[t],
            choked: false,
            active: true,
            from_pattern,
        };
    }
    /// The synced deck's position for the next block (None: own clock).
    pub fn follow(&mut self, follow: Option<Follow>) {
        self.follow = follow;
    }
    /// Derived-material events the last block took from the pattern queue
    /// (in time order), for the sampler.
    pub fn samples_due(&self) -> &[patch_pattern::rt::RtEvent] {
        &self.samples[..self.samples_n]
    }
    /// A block without a graph position: the step sequencer only.
    ///
    /// `seq` is the step Sequencer's lane, `pat` the Pattern Unit's (the
    /// mixer channels named `sequencer` and `pattern`); each holds L, then R.
    pub fn render(&mut self, p: &Shared, rate: u32, seq: [&mut [f32]; 2], pat: [&mut [f32]; 2]) {
        self.render_at(p, rate, None, seq, pat);
    }
    /// A block starting at graph sample `at`: pattern voices due in it are
    /// triggered on their own frame (late ones on the first).
    pub fn render_at(
        &mut self,
        p: &Shared,
        rate: u32,
        at: Option<i64>,
        seq: [&mut [f32]; 2],
        pat: [&mut [f32]; 2],
    ) {
        let frames = seq[0]
            .len()
            .min(seq[1].len())
            .min(pat[0].len())
            .min(pat[1].len());
        let [seq_l, seq_r] = seq;
        let [pat_l, pat_r] = pat;
        let (seq_l, seq_r) = (&mut seq_l[..frames], &mut seq_r[..frames]);
        let (pat_l, pat_r) = (&mut pat_l[..frames], &mut pat_r[..frames]);
        self.due_n = 0;
        self.samples_n = 0;
        if let Some(start) = at {
            let end = start + frames as i64;
            let (due, n) = (&mut self.due, &mut self.due_n);
            let (samples, sn) = (&mut self.samples, &mut self.samples_n);
            // SAFETY: the engine callback is the pattern queue's only consumer.
            unsafe { p.pattern.drain_before(end, |e| {
                if e.kind == patch_pattern::rt::RtKind::Sample {
                    if *sn < samples.len() {
                        samples[*sn] = e;
                        *sn += 1;
                    }
                } else if *n < due.len() {
                    due[*n] = e;
                    *n += 1;
                }
            }) };
        }
        let block_start = at.unwrap_or(0);
        let mut next_due = 0usize;
        let reset = p.reset.load(Relaxed);
        if reset != self.reset {
            self.reset = reset;
            self.tick = 0;
            self.remaining = 0.0;
        }
        let play = p.playing.load(Relaxed);
        let rate = rate.max(1) as f64;
        if rate != self.rate {
            let ratio = rate / self.rate;
            self.remaining *= ratio;
            self.duration *= ratio;
            self.phase *= ratio;
            self.rate = rate;
        }
        let smoothing = (1.0 - (-1.0 / (rate * 0.005)).exp()) as f32;
        let choke = (-1.0 / (rate * 0.002)).exp() as f32;
        let levels: [f32; 8] = std::array::from_fn(|t| {
            if p.muted[t].load(Relaxed) {
                0.0
            } else {
                p.levels[t].load(Relaxed) as f32 / 100.0
            }
        });
        let pans: [f32; 8] =
            std::array::from_fn(|t| (p.pans[t].load(Relaxed) as f32 - 100.0) / 100.0);
        if !play && !self.was_playing && self.due_n == 0 && self.voices.iter().all(|v| !v.active) {
            self.pattern_live = false;
            return;
        }
        if self.due_n > 0 {
            self.pattern_live = true;
        }
        for frame in 0..frames {
            while next_due < self.due_n && self.due[next_due].sample - block_start <= frame as i64 {
                let e = self.due[next_due];
                next_due += 1;
                let t = e.index as usize;
                if e.kind == patch_pattern::rt::RtKind::Voice && t < 8 && !p.muted[t].load(Relaxed)
                {
                    self.velocity[t] = e.value.clamp(0.0, 1.5);
                    self.trigger(t, true);
                }
            }
            if play {
                let swing = p.swing.load(Relaxed) as f64 / 100.0;
                let started = match self.follow {
                    // Synced: the step is where the deck is, so tempo
                    // changes, bends, jumps and loops carry the pattern.
                    // Odd sixteenths start `swing` of a step late.
                    Some(f) if f.frames_per_sixteenth > 0.0 => {
                        let unit = f.frames_per_sixteenth;
                        let s = swing.min(0.9);
                        let pos = f.sixteenths + frame as f64 / unit;
                        let pair = (pos / 2.0).floor();
                        let within = pos - pair * 2.0;
                        let odd = within >= 1.0 + s;
                        let k = pair as i64 * 2 + odd as i64;
                        let (start, len) = if odd { (1.0 + s, 1.0 - s) } else { (0.0, 1.0 + s) };
                        self.duration = len * unit;
                        self.phase = (within - start) * unit;
                        self.remaining = self.duration - self.phase;
                        if self.follow_step != Some(k) {
                            self.follow_step = Some(k);
                            // Entered mid-step (sync engaged, a jump):
                            // no late hit, the next step plays.
                            (self.phase < rate * 0.005).then_some(k)
                        } else {
                            None
                        }
                    }
                    _ => {
                        self.follow_step = None;
                        (self.remaining <= 0.0).then(|| {
                            let base =
                                rate * 60.0 / (p.bpm.load(Relaxed).clamp(40, 240) as f64 * 4.0);
                            self.duration = base
                                * (1.0
                                    + if self.tick.is_multiple_of(2) { 1.0 } else { -1.0 }
                                        * swing);
                            self.remaining += self.duration;
                            self.phase = 0.0;
                            self.tick as i64
                        })
                    }
                };
                if let Some(k) = started {
                    // Free running goes on from the next step.
                    self.tick = k.wrapping_add(1) as u64;
                    p.position.store(k as u32, Relaxed);
                    for t in 0..8 {
                        let step = k.rem_euclid(p.lengths[t].load(Relaxed).clamp(1, 32) as i64)
                            as usize;
                        let v = p.steps[t][step].load(Relaxed);
                        self.repeats[t] = 0;
                        if !p.muted[t].load(Relaxed)
                            && v & 255 > 0
                            && self.random() % 100 < ((v >> 8) & 255)
                        {
                            self.velocity[t] = (v & 255) as f32 / 127.0;
                            self.repeats[t] = ((v >> 16) & 255).clamp(1, 4);
                            self.trigger(t, false);
                        }
                    }
                }
                for t in 0..8 {
                    let n = self.repeats[t];
                    if n > 1
                        && !p.muted[t].load(Relaxed)
                        && (self.phase * n as f64 / self.duration).floor()
                            < ((self.phase + 1.0) * n as f64 / self.duration).floor()
                        && self.phase + 1.0 < self.duration
                    {
                        self.trigger(t, false);
                    }
                }
                if self.follow_step.is_none() {
                    self.phase += 1.0;
                    self.remaining -= 1.0;
                }
            }
            // The instrument's own gain and per-voice level/pan; the mixer
            // channels named `sequencer` and `pattern` carry the faders.
            let gain = if play || self.pattern_live {
                p.level.load(Relaxed) as f32 / 100.0 * 0.55
            } else {
                0.0
            };
            for side in 0..2 {
                smooth(&mut self.master[side], gain, smoothing);
            }
            for t in 0..8 {
                smooth(&mut self.track_gain[t], levels[t], smoothing);
                smooth(&mut self.track_pan[t], pans[t], smoothing);
            }
            for v in &mut self.voices {
                if !v.active {
                    continue;
                }
                let b = &self.bank[v.track];
                let i = v.pos as usize;
                if i + 1 >= b.len() {
                    v.active = false;
                    continue;
                }
                if v.choked {
                    v.gain *= choke;
                    if v.gain < 0.00001 {
                        v.active = false;
                    }
                }
                let x = (b[i] + (b[i + 1] - b[i]) * (v.pos - i as f64) as f32)
                    * v.gain
                    * self.track_gain[v.track];
                let pan = self.track_pan[v.track];
                let (l, r) = if v.from_pattern {
                    (&mut pat_l[frame], &mut pat_r[frame])
                } else {
                    (&mut seq_l[frame], &mut seq_r[frame])
                };
                *l += x * self.master[0] * (1.0 - pan.max(0.0));
                *r += x * self.master[1] * (1.0 + pan.min(0.0));
                v.pos += 48000.0 / rate;
            }
        }
        self.was_playing = play;
        if self.due_n == 0 && self.voices.iter().all(|v| !v.active) {
            self.pattern_live = false;
        }
    }
}
/// Snap the inaudible remainder so muted voices cannot run on subnormals.
#[inline]
fn smooth(current: &mut f32, target: f32, coefficient: f32) {
    *current += (target - *current) * coefficient;
    if (*current - target).abs() < 1e-7 {
        *current = target;
    }
}
/// Original drum designs: swept sine kick/tom, resonant snare, filtered
/// metallic hats, three-burst clap and a bright rim. Generated at startup.
/// The pattern instrument plays the same bank.
pub(crate) fn sound(kind: usize) -> Vec<f32> {
    let duration = [1.2, 0.65, 0.18, 0.9, 0.5, 0.8, 0.18, 0.25][kind];
    let mut phase = 0.0f64;
    let mut low = 0.0;
    let mut rng = 12345u32 + kind as u32;
    let mut b = Vec::with_capacity((duration * 48000.0) as usize);
    for i in 0..b.capacity() {
        let t = i as f64 / 48000.0;
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;
        let noise = rng as f64 / u32::MAX as f64 * 2.0 - 1.0;
        low += 0.18 * (noise - low);
        let high = noise - low;
        let x = match kind {
            0 | 5 => {
                let f = if kind == 0 {
                    47.0 + 125.0 * (-t * 42.0).exp()
                } else {
                    105.0 + 85.0 * (-t * 26.0).exp()
                };
                phase += std::f64::consts::TAU * f / 48000.0;
                (phase.sin() * (-t * if kind == 0 { 6.5 } else { 10.0 }).exp()
                    + high * 0.18 * (-t * 150.0).exp())
                    * 1.1
            }
            1 => {
                (std::f64::consts::TAU * 185.0 * t).sin() * (-t * 24.0).exp() * 0.5
                    + high * (-t * 15.0).exp() * 0.9
            }
            2 | 3 => {
                let metal = [317.0, 569.0, 811.0, 1237.0, 1741.0, 2399.0]
                    .iter()
                    .map(|f| {
                        (std::f64::consts::TAU * f * t).sin()
                            * (std::f64::consts::TAU * f * 1.43 * t).sin()
                    })
                    .sum::<f64>()
                    / 3.0;
                (high * 0.55 + metal * 0.45) * (-t * if kind == 2 { 42.0 } else { 8.0 }).exp()
            }
            4 => {
                let bursts = [0.0, 0.011, 0.023]
                    .iter()
                    .map(|a| {
                        if t >= *a {
                            (-(t - a) * 110.0).exp()
                        } else {
                            0.0
                        }
                    })
                    .sum::<f64>();
                high * (bursts * 0.45 + (-t * 16.0).exp() * 0.35)
            }
            6 => {
                ((std::f64::consts::TAU * 810.0 * t).sin() * 0.6
                    + (std::f64::consts::TAU * 1730.0 * t).sin() * 0.3)
                    * (-t * 65.0).exp()
            }
            _ => high * (-t * 28.0).exp() * 0.6,
        };
        b.push((x * (t / 0.0007).min(1.0) * ((duration - t) / 0.01).min(1.0)).tanh() as f32);
    }
    b
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_pattern_is_rejected() {
        let mut p = Pattern::default();
        p.tracks[0].length = 0;
        assert!(p.validate().is_err());
    }
    #[test]
    fn synced_steps_stay_on_a_fractional_tempo_deck_and_never_hit_late() {
        // A deck at 123.9999 BPM played at +2 %: the old integer BPM (123)
        // drifted a beat a minute. Followed, step k starts on the deck's
        // k-th sixteenth for minutes, whatever the block size.
        let p = Shared::default();
        p.playing.store(true, Relaxed);
        let mut s = Sequencer::default();
        let unit = 48_000.0 * 60.0 / (123.9999 * 1.02 * 4.0);
        // Engaged half way into sixteenth 10: no late hit on it.
        let mut at = 10.5;
        let block = 64usize;
        let (mut l, mut r) = (vec![0.0; block], vec![0.0; block]);
        let (mut pl, mut pr) = (vec![0.0; block], vec![0.0; block]);
        s.follow(Some(Follow { sixteenths: at, frames_per_sixteenth: unit }));
        s.render(&p, 48000, [&mut l, &mut r], [&mut pl, &mut pr]);
        assert_eq!(p.position.load(Relaxed), 0, "joined mid-step: nothing fired");
        for _ in 0..(48_000 * 180 / block) {
            at += block as f64 / unit;
            s.follow(Some(Follow { sixteenths: at, frames_per_sixteenth: unit }));
            s.render(&p, 48000, [&mut l, &mut r], [&mut pl, &mut pr]);
            // The last step started is the one the deck is in.
            let deck_step = (at + (block - 1) as f64 / unit).floor() as u32;
            if deck_step > 10 {
                assert_eq!(p.position.load(Relaxed), deck_step, "at sixteenth {at}");
            }
        }
        // The deck stops: the Sequencer runs on from the next step.
        s.follow(None);
        let before = p.position.load(Relaxed);
        for _ in 0..(48_000 / block) {
            s.render(&p, 48000, [&mut l, &mut r], [&mut pl, &mut pr]);
        }
        assert!(p.position.load(Relaxed) > before);
    }
    #[test]
    fn timing_is_independent_of_block_size() {
        fn run(block: usize) -> (Vec<f32>, u32) {
            let p = Shared::default();
            p.playing.store(true, Relaxed);
            let mut s = Sequencer::default();
            let mut out = vec![];
            for _ in 0..48000 / block {
                let mut l = vec![0.0; block];
                let mut r = l.clone();
                let mut pl = vec![0.0; block];
                let mut pr = pl.clone();
                s.render(&p, 48000, [&mut l, &mut r], [&mut pl, &mut pr]);
                out.extend(l);
            }
            (out, p.position.load(Relaxed))
        }
        let (a, pa) = run(48);
        let (b, pb) = run(240);
        assert_eq!(a, b);
        assert_eq!(pa, pb);
        assert!(a.iter().any(|x| x.abs() > 0.01));
        assert!(a.iter().all(|x| x.is_finite() && x.abs() < 1.0));
    }
    #[test]
    fn pattern_voices_start_on_their_sample_in_any_block_size() {
        use patch_pattern::rt::{RtEvent, RtKind};
        // A kick at graph sample 1000 + `t`, the step sequencer stopped.
        fn onset(t: i64, block: usize) -> usize {
            let p = Shared::default();
            unsafe { p.pattern.push(RtEvent {
                sample: 1_000 + t,
                kind: RtKind::Voice,
                index: 0,
                value: 1.0,
                epoch: 0,
                ..Default::default()
            }) };
            let mut s = Sequencer::default();
            let mut out = vec![];
            let mut at = 1_000i64;
            for _ in 0..12_000 / block {
                let mut l = vec![0.0; block];
                let mut r = l.clone();
                let mut pl = vec![0.0; block];
                let mut pr = pl.clone();
                s.render_at(&p, 48000, Some(at), [&mut l, &mut r], [&mut pl, &mut pr]);
                at += block as i64;
                out.extend(pl);
            }
            out.iter().position(|x| x.abs() > 1e-9).unwrap()
        }
        let base = onset(3_333, 64);
        assert!((3_333..=3_334).contains(&base), "{base}");
        for block in [32, 64, 100, 256, 1024] {
            assert_eq!(onset(3_333, block), base, "block {block}");
            assert_eq!(
                onset(3_334, block),
                base + 1,
                "one sample later, block {block}"
            );
        }
    }
    fn render_frames(s: &mut Sequencer, p: &Shared, rate: u32, frames: usize) -> Vec<f32> {
        let mut l = vec![0.0; frames];
        let mut r = l.clone();
        let (mut pl, mut pr) = (vec![0.0; frames], vec![0.0; frames]);
        s.render(p, rate, [&mut l, &mut r], [&mut pl, &mut pr]);
        l
    }
    #[test]
    fn swing_preserves_the_pair_and_rate_changes_preserve_time() {
        let p = Shared::default();
        let pattern = Pattern {
            bpm: 120,
            swing: 60,
            ..Pattern::default()
        };
        p.set(&pattern);
        p.playing.store(true, Relaxed);
        let mut s = Sequencer::default();
        render_frames(&mut s, &p, 48000, 9600);
        assert_eq!(s.tick, 1);
        render_frames(&mut s, &p, 48000, 2400);
        assert_eq!(s.tick, 2);
        render_frames(&mut s, &p, 48000, 1);
        assert_eq!(s.tick, 3);
        let before = s.remaining / 48000.0;
        render_frames(&mut s, &p, 96000, 960);
        assert!((s.remaining / 96000.0 - (before - 0.01)).abs() < 1e-9);
    }
    #[test]
    fn repeats_probability_and_track_lengths_are_applied() {
        let p = Shared::default();
        let mut pattern = Pattern {
            bpm: 120,
            ..Pattern::default()
        };
        for t in &mut pattern.tracks {
            t.steps = [Step::default(); 32];
        }
        pattern.tracks[0].length = 3;
        pattern.tracks[0].steps[0] = Step {
            velocity: 100,
            probability: 100,
            repeats: 4,
        };
        p.set(&pattern);
        p.playing.store(true, Relaxed);
        let mut s = Sequencer::default();
        render_frames(&mut s, &p, 48000, 5999);
        assert_eq!(s.voices.iter().filter(|v| v.active).count(), 4);
        render_frames(&mut s, &p, 48000, 12002);
        assert_eq!(s.tick, 4);
        assert_eq!(
            s.voices.iter().filter(|v| v.active).count(),
            5,
            "track loops at step 3"
        );
        pattern.tracks[0].steps[0].probability = 0;
        p.set(&pattern);
        let mut s = Sequencer::default();
        assert!(
            render_frames(&mut s, &p, 44100, 44100)
                .iter()
                .all(|x| *x == 0.0)
        );
        pattern.tracks[0].steps[0].probability = 100;
        pattern.tracks[0].muted = true;
        p.set(&pattern);
        assert!(
            render_frames(&mut s, &p, 44100, 44100)
                .iter()
                .all(|x| *x == 0.0)
        );
    }
    #[test]
    fn invalid_ipc_changes_nothing_and_status_polls_are_small() {
        let p = Shared::default();
        let before = serde_json::to_value(p.pattern()).unwrap();
        let mut bad = before.clone();
        bad["tracks"][0]["pan"] = json!(i32::MIN);
        assert!(p.request(&json!({"pattern":bad,"playing":true})).is_err());
        assert_eq!(serde_json::to_value(p.pattern()).unwrap(), before);
        assert!(!p.playing.load(Relaxed));
        assert!(
            p.request(&json!({"status_only":true}))
                .unwrap()
                .get("pattern")
                .is_none()
        );
    }
    #[test]
    fn save_load_round_trip_and_bad_file_preserves_edits() {
        let dir = std::env::temp_dir().join(format!("patch-sequencer-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pattern.json");
        let p = Shared::default();
        p.request_at(&json!({"pattern":Pattern::preset(2),"save":true}), &path)
            .unwrap();
        p.set(&Pattern::preset(1));
        let restored = p.request_at(&json!({"load":true}), &path).unwrap();
        assert_eq!(
            restored["pattern"],
            serde_json::to_value(Pattern::preset(2)).unwrap()
        );
        std::fs::write(&path, b"{} invalid").unwrap();
        assert!(p.request_at(&json!({"load":true}), &path).is_err());
        assert_eq!(
            serde_json::to_value(p.pattern()).unwrap(),
            restored["pattern"]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn all_voices_are_finite_and_decay_to_silence() {
        for i in 0..8 {
            let b = sound(i);
            assert!(b.iter().all(|x| x.is_finite() && x.abs() <= 1.0));
            assert!(b.iter().any(|x| x.abs() > 0.1));
            assert!(b[0].abs() < 1e-6 && b.last().unwrap().abs() < 0.001);
        }
    }
    #[test]
    fn stopped_is_silent_and_stop_rewinds() {
        let p = Shared::default();
        let mut s = Sequencer::default();
        let mut l = [0.0; 512];
        let mut r = l;
        let mut pl = [0.0; 512];
        let mut pr = pl;
        s.render(&p, 48000, [&mut l, &mut r], [&mut pl, &mut pr]);
        assert_eq!(l, [0.0; 512]);
        p.request(&json!({"playing":true})).unwrap();
        s.render(&p, 48000, [&mut l, &mut r], [&mut pl, &mut pr]);
        p.request(&json!({"stop":true})).unwrap();
        s.render(&p, 48000, [&mut l, &mut r], [&mut pl, &mut pr]);
        assert_eq!(s.tick, 0);
    }
}
