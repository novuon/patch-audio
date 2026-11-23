//! The filmed DJ: clips made ahead of time, kept on this machine only
//! (`~/.local/share/omarchy-patch/performer-me`), streamed one frame at a time.
//! Nothing runs and nothing is held while the mode is off.
//!
//! Clips are named for what they are: `on-groove-1.386.mp4`, `off-groove-1…`,
//! `on-any-3…` (any gesture), `on-ear-1…` (a gesture the groove can ask for),
//! `puton-1…`, `takeoff-1…`. The last number is how many frames it holds.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, SyncSender, TrySendError, channel, sync_channel};
use std::time::Instant;

pub(super) const WIDTH: i32 = 768;
pub(super) const HEIGHT: i32 = 704;
const FRAME: usize = WIDTH as usize * HEIGHT as usize * 4;
const FPS: f32 = 48.0;
/// How far before a clip's end the next is chosen and starts decoding, in
/// frames (two seconds).
const QUEUE_AHEAD: f32 = 96.0;
/// A clip whose name does not say how many frames it holds.
const FRAMES: f32 = 96.0;

/// What the groove asks the DJ to do, as the drawn DJ's gestures do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Cue {
    Ear,
    Air,
    Clap,
    Point,
}
impl Cue {
    fn name(self) -> &'static str {
        match self {
            Cue::Ear => "ear",
            Cue::Air => "air",
            Cue::Clap => "clap",
            Cue::Point => "point",
        }
    }
    /// A hand pressing the ear cup needs headphones on.
    fn needs_headphones(self) -> bool {
        self == Cue::Ear
    }
    /// The gesture the groove is holding up, if any.
    pub(super) fn from_gestures(gesture: [f32; 4], gesture2: [f32; 4]) -> Option<Self> {
        let strongest = [
            (gesture[0].max(gesture[1]), Cue::Ear),
            (gesture[2].max(gesture[3]), Cue::Air),
            (gesture2[0], Cue::Clap),
            (gesture2[1].max(gesture2[2]), Cue::Point),
        ]
        .into_iter()
        .fold((0.5, None), |best, (v, c)| {
            if v > best.0 { (v, Some(c)) } else { best }
        });
        strongest.1
    }
}

/// A decoded frame, or the end of its clip (empty).
struct Frame(Vec<u8>);

struct Kill(Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Named `on-groove-1.386.mp4`: what it is, its number, how many frames.
fn frames_in(path: &Path) -> f32 {
    path.file_stem()
        .and_then(|n| n.to_str())
        .and_then(|n| n.rsplit_once('.'))
        .and_then(|(_, f)| f.parse().ok())
        .unwrap_or(FRAMES)
}

/// How many beats a clip spans: whole bars, nearest its own speed, so a move
/// keeps its feel and still lands on the bar.
fn span_beats(frames: f32, beats_per_second: f32) -> f32 {
    ((frames / FPS * beats_per_second / 4.0).round() * 4.0).max(4.0)
}

/// Whether a clip may start at this beat: on a bar line, or just after one, so
/// a clip ending on the bar leads straight into the next.
/// The beats a clip starting at `start` spans so that it ends on a bar line,
/// as near its own speed as the bars allow (never more than a fifth faster).
fn span_to_bar(start: f32, frames: f32, beats_per_second: f32) -> f32 {
    let natural = frames / FPS * beats_per_second;
    let mut end = ((start + natural) / 4.0).round() * 4.0;
    if end - start < natural * 0.8 {
        end += 4.0;
    }
    (end - start).max(2.0)
}

fn at_bar_start(beat: f32) -> bool {
    beat.rem_euclid(4.0) < 0.75
}

/// The frame due at this beat in a clip that started on the bar at `anchor`:
/// locked to the beat clock, so it ends exactly on its last bar and never drifts.
fn frame_at(beat: f32, anchor: f32, span: f32, length: f32) -> f32 {
    (beat - anchor) / span * length
}

/// The clips on disk, sorted so a take list is stable.
#[derive(Default)]
struct Clips(Vec<PathBuf>);
impl Clips {
    fn read(dir: &Path) -> Self {
        let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "mp4"))
            .collect();
        found.sort();
        Self(found)
    }
    fn of(&self, prefix: &str) -> Vec<&PathBuf> {
        let prefix = format!("{prefix}-");
        self.0
            .iter()
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&prefix))
            })
            .collect()
    }
}

/// Which clip is next, and whether the DJ will have headphones on after it.
/// Every clip starts and ends at home for its headphones state, so any can
/// follow any. The only way between the states is a put-on or take-off clip.
#[derive(Default)]
struct Choice {
    headphones: bool,
    since_gesture: u32,
    since_smile: u32,
    /// Clips until the next big movement, and until the next friendly one.
    /// Zero means "not chosen yet".
    wait_gesture: u32,
    wait_smile: u32,
    rng: u32,
    last: Option<PathBuf>,
    /// What has played, oldest first, so a clip waits until the others of its
    /// kind have been on. A two-hour set then walks the library instead of
    /// drawing the same handful again and again.
    recent: Vec<PathBuf>,
}
impl Choice {
    fn next(&mut self, clips: &Clips, want: bool, cue: Option<Cue>, drop: bool) -> Option<PathBuf> {
        // Headphones follow the director. A hand-to-ear cue must not put them
        // back on in a breakdown, where he plays with them around his neck.
        if want != self.headphones {
            let change = clips.of(if want { "puton" } else { "takeoff" });
            if let Some(path) = self.pick(&change) {
                self.headphones = want;
                return Some(path);
            }
        }
        let state = if self.headphones { "on" } else { "off" };
        // He mostly just plays. The groove asks for a big movement most
        // phrases; taking each one made almost every other clip a throw or
        // hands in the air. A big movement comes after a short run of playing
        // (about four to seven clips), sooner on a drop, and it is the one the
        // groove is asking for. A minute between them felt like he had stopped.
        if self.wait_gesture == 0 {
            self.wait_gesture = 4 + self.roll(4);
        }
        let due = (drop && self.since_gesture >= 2) || self.since_gesture >= self.wait_gesture;
        // Most big movements are the one the groove asked for. About one in
        // four is a smaller loose flick (`any`), so that take is part of the
        // vocabulary too.
        let loose = due && self.roll(4) == 0;
        if due
            && let Some(path) = cue
                .filter(|c| self.headphones || !c.needs_headphones())
                .and_then(|c| {
                    let named = clips.of(&format!("{state}-{}", c.name()));
                    if loose {
                        self.pick(&clips.of(&format!("{state}-any"))).or_else(|| self.pick(&named))
                    } else {
                        self.pick(&named)
                    }
                })
        {
            self.since_gesture = 0;
            self.wait_gesture = 4 + self.roll(4);
            self.since_smile += 1;
            return Some(path);
        }
        self.since_gesture += 1;
        // A friendly moment on its own, slower still, with a groove between
        // it and a big movement.
        if self.wait_smile == 0 {
            self.wait_smile = 6 + self.roll(5);
        }
        if self.since_gesture >= 2
            && self.since_smile >= self.wait_smile
            && let Some(path) = self.pick(&clips.of(&format!("{state}-smile")))
        {
            self.since_smile = 0;
            self.wait_smile = 6 + self.roll(5);
            return Some(path);
        }
        self.since_smile += 1;
        self.pick(&clips.of(&format!("{state}-groove")))
    }
    fn roll(&mut self, n: u32) -> u32 {
        if self.rng == 0 {
            self.rng = 0x9e37_79b9;
        }
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        self.rng % n.max(1)
    }
    fn pick(&mut self, from: &[&PathBuf]) -> Option<PathBuf> {
        if from.is_empty() {
            return None;
        }
        // The other clips of this kind play before one returns. A pool of one
        // still plays; it is the only take we have.
        let ban = from.len().saturating_sub(1);
        let mut held = Vec::new();
        if ban > 0 {
            for played in self.recent.iter().rev() {
                if held.len() >= ban {
                    break;
                }
                if from.contains(&played) && !held.contains(&played) {
                    held.push(played);
                }
            }
        }
        let mut pool: Vec<&PathBuf> = from
            .iter()
            .copied()
            .filter(|p| !held.contains(p))
            .collect();
        if pool.is_empty() {
            pool = from
                .iter()
                .copied()
                .filter(|p| self.last.as_ref() != Some(*p))
                .collect();
        }
        if pool.is_empty() {
            pool = from.to_vec();
        }
        let mut i = self.roll(pool.len() as u32) as usize;
        if pool.len() > 1 && self.last.as_ref() == Some(pool[i]) {
            i = (i + 1) % pool.len();
        }
        let chosen = pool[i].clone();
        self.last = Some(chosen.clone());
        self.recent.push(chosen.clone());
        if self.recent.len() > 80 {
            self.recent.drain(0..self.recent.len() - 80);
        }
        Some(chosen)
    }
}

/// The colour is the film's left half, the mask its right.
fn decode(path: &Path, out: &SyncSender<Frame>, spare: &Receiver<Vec<u8>>) -> Result<(), ()> {
    let filter = format!(
        "[0:v]crop={WIDTH}:{HEIGHT}:0:0,format=rgb24[c];\
         [0:v]crop={WIDTH}:{HEIGHT}:{WIDTH}:0,format=gray[a];[c][a]alphamerge[o]"
    );
    let child = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(path)
        .args(["-filter_complex", &filter, "-map", "[o]", "-f", "rawvideo"])
        .args(["-pix_fmt", "rgba", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ())?;
    let mut child = Kill(child);
    let mut pipe = child.0.stdout.take().ok_or(())?;
    loop {
        let mut buffer = spare.try_recv().unwrap_or_else(|_| vec![0; FRAME]);
        if pipe.read_exact(&mut buffer).is_err() {
            break;
        }
        out.send(Frame(buffer)).map_err(|_| ())?;
    }
    Ok(())
}

/// One decoder: a thread that streams the clips it is given, each followed
/// by an empty frame.
struct Lane {
    want: SyncSender<PathBuf>,
    frames: Receiver<Frame>,
    spare: Sender<Vec<u8>>,
}
impl Lane {
    fn open() -> Option<Self> {
        let (want, wanted) = sync_channel::<PathBuf>(1);
        let (out, frames) = sync_channel::<Frame>(12);
        let (spare, spares) = channel::<Vec<u8>>();
        std::thread::Builder::new()
            .name("patch-film".into())
            .spawn(move || {
                while let Ok(path) = wanted.recv() {
                    if decode(&path, &out, &spares).is_err()
                        || out.send(Frame(Vec::new())).is_err()
                    {
                        return;
                    }
                }
            })
            .ok()?;
        Some(Self { want, frames, spare })
    }
}

pub(super) struct Player {
    /// Two decoders taking turns: the next clip decodes on the other one while
    /// this one plays, so its first frames are waiting when it starts, even
    /// when its file or ffmpeg is slow to open.
    lanes: [Lane; 2],
    lane: usize,
    clips: Clips,
    choice: Choice,
    /// A gesture the groove asked for that has not been played yet.
    pending: Option<Cue>,
    last_cue: Option<Cue>,
    position: f32,
    shown: f32,
    last: Instant,
    waiting: bool,
    /// The clip queued to follow this one (its frames), chosen just before the
    /// end so the join has no gap.
    next: Option<f32>,
    /// Frames in the clip now playing, the beats it spans (0 until it starts).
    length: f32,
    span: f32,
    /// The bar line the clip started on, in beats.
    anchor: f32,
    last_beat: Option<f32>,
    /// The music's tempo as the beat clock runs, beats per second.
    rate: f32,
}
impl Player {
    pub(super) fn open() -> Option<Self> {
        let dir = PathBuf::from(std::env::var_os("HOME")?)
            .join(".local/share/omarchy-patch/performer-me");
        let clips = Clips::read(&dir);
        if clips.of("on-groove").is_empty() {
            return None;
        }
        Some(Self {
            lanes: [Lane::open()?, Lane::open()?],
            lane: 0,
            clips,
            // He starts at home with his headphones on.
            choice: Choice {
                headphones: true,
                ..Default::default()
            },
            pending: None,
            last_cue: None,
            position: 0.0,
            shown: -1.0,
            last: Instant::now(),
            waiting: false,
            next: None,
            length: FRAMES,
            span: 0.0,
            anchor: 0.0,
            last_beat: None,
            rate: 0.0,
        })
    }
    /// The newest frame due now, if there is one. A clip waits in its home
    /// pose for the next bar line, then runs by the beat clock, so its moves
    /// keep to the music. Where there is no clock it plays at its own speed; a
    /// paused DJ holds still.
    pub(super) fn advance(
        &mut self,
        now: Instant,
        beat: Option<f32>,
        drop: bool,
        headphones: bool,
        cue: Option<Cue>,
    ) -> Option<Vec<u8>> {
        let dt = now.saturating_duration_since(self.last).as_secs_f32().min(0.25);
        self.last = now;
        if cue != self.last_cue {
            if cue.is_some() {
                self.pending = cue;
            }
            self.last_cue = cue;
        }
        let step = match (beat, self.last_beat) {
            (Some(b), Some(l)) if b > l && b - l < 1.0 => Some(b - l),
            _ => None,
        };
        if let Some(step) = step.filter(|_| dt > 0.0) {
            let rate = step / dt;
            self.rate = if self.rate > 0.0 {
                self.rate * 0.95 + rate * 0.05
            } else {
                rate
            };
        }
        self.last_beat = beat;
        if !self.waiting {
            let path = self
                .choice
                .next(&self.clips, headphones, self.pending, drop)?;
            self.pending = None;
            let frames = frames_in(&path);
            match self.lanes[self.lane].want.try_send(path) {
                Ok(()) => {
                    self.waiting = true;
                    self.position = 0.0;
                    self.shown = -1.0;
                    self.length = frames;
                    self.span = 0.0;
                }
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => return None,
            }
        }
        // The next clip is chosen two seconds before this one ends and starts
        // decoding on the other lane, so it follows on the bar line without a pause.
        if self.waiting
            && self.next.is_none()
            && self.span > 0.0
            && self.position >= self.length - QUEUE_AHEAD
            && let Some(path) = self.choice.next(&self.clips, headphones, self.pending, drop)
        {
            let frames = frames_in(&path);
            if self.lanes[1 - self.lane].want.try_send(path).is_ok() {
                self.pending = None;
                self.next = Some(frames);
            }
        }
        // A clip starts at a bar line, timed from its first frame on screen:
        // it never skips frames or runs to catch up while the decoder starts,
        // and still ends on its last bar.
        if self.span == 0.0
            && self.shown >= 0.0
            && let Some(b) = beat
            && (at_bar_start(b) || self.rate <= 0.0)
        {
            let bar = (b / 4.0).floor() * 4.0;
            self.span = (bar + span_beats(self.length, self.rate) - b).max(2.0);
            self.anchor = b;
            #[cfg(test)]
            eprintln!(
                "film clip starts at beat {b:.2}, {} frames over {} beats",
                self.length, self.span
            );
        }
        if self.span > 0.0
            && let Some(b) = beat
        {
            if step.is_some() {
                self.position = frame_at(b, self.anchor, self.span, self.length);
            } else {
                // No steady clock (a jump, or a frozen beat): its own speed from
                // here, and the bar follows it.
                self.position += dt * FPS;
                self.anchor = b - self.position / self.length * self.span;
            }
        }
        let mut newest: Option<Vec<u8>> = None;
        while self.shown + 1.0 <= self.position {
            match self.lanes[self.lane].frames.try_recv() {
                Ok(Frame(b)) if b.is_empty() => {
                    let Some(length) = self.next.take() else {
                        self.waiting = false;
                        break;
                    };
                    self.lane = 1 - self.lane;
                    // The queued clip takes over from its first frame, and is
                    // timed to end on a bar line.
                    let end = self.anchor + self.span;
                    self.position = 0.0;
                    self.shown = -1.0;
                    self.length = length;
                    match beat {
                        Some(b) if self.span > 0.0 && self.rate > 0.0 => {
                            let start = if (b - end).abs() < 0.25 { end } else { b };
                            self.anchor = start;
                            self.span = span_to_bar(start, length, self.rate);
                            self.position = frame_at(b, start, self.span, length).max(0.0);
                        }
                        _ => self.span = 0.0,
                    }
                    #[cfg(test)]
                    eprintln!("film next clip: {length} frames over {} beats, rate {:.3}, from beat {:.2}", self.span, self.rate, self.anchor);
                }
                Ok(Frame(b)) => {
                    self.shown += 1.0;
                    if let Some(old) = newest.replace(b) {
                        let _ = self.lanes[self.lane].spare.send(old);
                    }
                }
                Err(_) => {
                    // The decoder is behind: hold this frame rather than race
                    // to catch up later; the clip's clock waits with it.
                    if self.position > self.shown + 1.5 {
                        #[cfg(test)]
                        eprintln!("film holds at frame {} of {} (position {:.1})", self.shown, self.length, self.position);
                        self.position = self.shown + 1.0;
                        if self.span > 0.0
                            && let Some(b) = beat
                        {
                            self.anchor = b - self.position / self.length * self.span;
                        }
                    }
                    break;
                }
            }
        }
        newest
    }
    pub(super) fn give_back(&self, buffer: Vec<u8>) {
        let _ = self.lanes[self.lane].spare.send(buffer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(names: &[&str]) -> Clips {
        Clips(names.iter().map(PathBuf::from).collect())
    }
    const ALL: &[&str] = &[
        "off-groove-1.96.mp4",
        "on-air-1.96.mp4",
        "on-any-1.96.mp4",
        "on-ear-1.96.mp4",
        "on-groove-1.96.mp4",
        "puton-1.96.mp4",
        "takeoff-1.96.mp4",
    ];
    fn name(path: Option<PathBuf>) -> String {
        path.unwrap().to_string_lossy().split('-').next().unwrap().to_string()
    }

    #[test]
    fn a_clip_says_its_length_and_spans_whole_bars() {
        assert_eq!(frames_in(Path::new("/x/on-groove-1.384.mp4")), 384.0);
        assert_eq!(frames_in(Path::new("/x/on-any-2.mp4")), FRAMES);
        // 4 s at 124 BPM is 8.3 beats: two bars, a little faster.
        assert_eq!(span_beats(192.0, 124.0 / 60.0), 8.0);
        assert_eq!(span_beats(384.0, 140.0 / 60.0), 20.0);
        assert_eq!(span_beats(10.0, 2.0), 4.0);
    }

    #[test]
    fn clips_follow_each_other_bar_after_bar() {
        // A clip from bar 4 over 8 beats ends exactly on bar 12.
        assert_eq!(frame_at(4.0, 4.0, 8.0, 195.0), 0.0);
        assert_eq!(frame_at(8.0, 4.0, 8.0, 195.0), 97.5);
        assert_eq!(frame_at(12.0, 4.0, 8.0, 195.0), 195.0);
        // A clip that starts late still ends on a bar, near its own speed.
        assert_eq!(span_to_bar(13.0, 171.0, 124.0 / 60.0), 7.0);
        assert_eq!(span_to_bar(12.0, 171.0, 124.0 / 60.0), 8.0);
        assert_eq!(span_to_bar(15.5, 171.0, 124.0 / 60.0), 8.5);
        // The next may start just after that bar, not a whole bar later.
        assert!(at_bar_start(12.0) && at_bar_start(12.5));
        assert!(!at_bar_start(13.0) && !at_bar_start(15.9));
    }

    #[test]
    fn headphones_only_change_through_a_clip_of_changing_them() {
        let clips = library(ALL);
        let mut c = Choice { headphones: true, ..Default::default() };
        assert_eq!(name(c.next(&clips, false, None, false)), "takeoff");
        assert!(!c.headphones);
        assert_eq!(name(c.next(&clips, false, None, false)), "off");
        assert_eq!(name(c.next(&clips, true, None, false)), "puton");
        assert!(c.headphones);
    }

    #[test]
    fn an_ear_cue_does_not_change_the_headphones() {
        let clips = library(ALL);
        let mut c = Choice::default();
        // Headphones off (a breakdown): a hand-to-ear cue does not put them on.
        let p = c.next(&clips, false, Some(Cue::Ear), false).unwrap();
        assert!(p.to_string_lossy().starts_with("off-"), "{p:?}");
        assert!(!c.headphones);
        // With them already on, and a big movement due, that cue plays.
        let clips = library(&["on-groove-1.96.mp4", "on-ear-1.96.mp4"]);
        let mut c = Choice {
            headphones: true,
            since_gesture: 5,
            wait_gesture: 5,
            ..Default::default()
        };
        let ear = c.next(&clips, true, Some(Cue::Ear), false).unwrap();
        assert!(ear.to_string_lossy().starts_with("on-ear"), "{ear:?}");
    }

    #[test]
    fn big_movements_come_occasionally_not_every_other_clip() {
        let clips = library(&[
            "on-groove-1.96.mp4",
            "on-groove-2.96.mp4",
            "on-air-1.96.mp4",
            "on-smile-1.96.mp4",
        ]);
        let mut c = Choice {
            headphones: true,
            ..Default::default()
        };
        let kinds: Vec<String> = (0..60)
            .map(|_| {
                c.next(&clips, true, Some(Cue::Air), false)
                    .unwrap()
                    .to_string_lossy()
                    .split('-')
                    .nth(1)
                    .unwrap()
                    .to_string()
            })
            .collect();
        let big = kinds.iter().filter(|k| *k == "air").count();
        // About every four to seven clips: not every other one, and not a
        // minute of nothing between them.
        assert!((6..=16).contains(&big), "{kinds:?}");
        assert!(
            kinds.windows(4).all(|w| w.iter().filter(|k| **k == "air").count() <= 1),
            "{kinds:?}"
        );
        assert!(
            kinds.windows(2).all(|w| w[0] != "air" || w[1] != "smile"),
            "{kinds:?}"
        );
    }

    #[test]
    fn a_drop_brings_the_bigger_movement_sooner() {
        let clips = library(&["on-groove-1.96.mp4", "on-air-1.96.mp4"]);
        let mut early = Choice {
            headphones: true,
            ..Default::default()
        };
        let first = early.next(&clips, true, Some(Cue::Air), true).unwrap();
        assert!(first.to_string_lossy().contains("groove"), "{first:?}");
        let mut due = Choice {
            headphones: true,
            since_gesture: 2,
            wait_gesture: 18,
            ..Default::default()
        };
        let p = due.next(&clips, true, Some(Cue::Air), true).unwrap();
        assert!(p.to_string_lossy().contains("air"), "{p:?}");
    }

    #[test]
    fn nothing_is_played_from_the_wrong_headphones_state() {
        let clips = library(ALL);
        let mut c = Choice::default();
        for _ in 0..12 {
            let p = c.next(&clips, false, Some(Cue::Air), true).unwrap();
            assert!(p.to_string_lossy().starts_with("off-"), "{p:?}");
        }
    }

    #[test]
    fn the_same_clip_never_plays_twice_running() {
        let clips = library(&["on-groove-1.96.mp4", "on-groove-2.96.mp4", "on-groove-3.96.mp4"]);
        let mut c = Choice { headphones: true, ..Default::default() };
        let mut last = None;
        for _ in 0..50 {
            let p = c.next(&clips, true, None, false);
            assert_ne!(p, last);
            last = p;
        }
    }

    #[test]
    fn friendly_moments_come_between_the_grooves() {
        let clips = library(&["on-groove-1.96.mp4", "on-groove-2.96.mp4", "on-smile-1.96.mp4", "on-smile-2.96.mp4", "on-any-1.96.mp4"]);
        let mut c = Choice { headphones: true, ..Default::default() };
        let kinds: Vec<String> = (0..24)
            .map(|_| c.next(&clips, true, None, false).unwrap().to_string_lossy().split('-').nth(1).unwrap().to_string())
            .collect();
        let smiles = kinds.iter().filter(|k| *k == "smile").count();
        let grooves = kinds.iter().filter(|k| *k == "groove").count();
        assert!(smiles >= 1 && smiles * 4 < grooves, "{kinds:?}");
        // Never two in a row: a smile always comes between other clips.
        assert!(kinds.windows(2).all(|w| !(w[0] == "smile" && w[1] == "smile")), "{kinds:?}");
    }

    #[test]
    fn a_clip_waits_until_the_others_of_its_kind_have_played() {
        let names = [
            "on-groove-1.96.mp4",
            "on-groove-2.96.mp4",
            "on-groove-3.96.mp4",
            "on-groove-4.96.mp4",
            "on-groove-5.96.mp4",
        ];
        let clips = library(&names);
        let mut c = Choice {
            headphones: true,
            ..Default::default()
        };
        let played: Vec<String> = (0..15)
            .map(|_| c.next(&clips, true, None, false).unwrap().to_string_lossy().into_owned())
            .collect();
        for window in played.windows(5) {
            let mut seen = window.to_vec();
            seen.sort();
            seen.dedup();
            assert_eq!(seen.len(), 5, "{played:?}");
        }
    }

    #[test]
    fn the_strongest_gesture_is_the_cue() {
        assert_eq!(Cue::from_gestures([0.0; 4], [0.0; 4]), None);
        assert_eq!(Cue::from_gestures([0.9, 0.0, 0.2, 0.0], [0.0; 4]), Some(Cue::Ear));
        assert_eq!(Cue::from_gestures([0.1, 0.0, 0.0, 0.7], [0.0; 4]), Some(Cue::Air));
        assert_eq!(Cue::from_gestures([0.0; 4], [0.8, 0.0, 0.0, 0.0]), Some(Cue::Clap));
        assert_eq!(Cue::from_gestures([0.0; 4], [0.0, 0.0, 0.6, 0.0]), Some(Cue::Point));
    }
}
