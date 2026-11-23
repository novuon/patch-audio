//! The meter style and the Player surface: binding and polling.

use super::*;
use crate::player::Action;
use crate::player::carts::{self, Board, Do, Pad, Playing};
use crate::player::history::{self, History, Queue};
use crate::player::marks::{self, Marks};
use crate::player::{Snapshot, wave};

/// The Player's desk beside the transport: the queue, what played lately
/// and the waveform, as the windows hold them.
#[derive(Default)]
pub(super) struct DeskUi {
    queue: Rc<VecModel<QueueRow>>,
    recent: Rc<VecModel<QueueRow>>,
    /// The history file as last read, and its time then.
    history: RefCell<History>,
    history_stamp: Cell<Option<std::time::SystemTime>>,
    history_read: Cell<bool>,
    /// The queue as it last stood, shown while the player is not running.
    resting: RefCell<Queue>,
    resting_read: Cell<bool>,
    picker: crate::poll::Poller,
    picking: Cell<bool>,
    /// The waveform of what is loaded (None: a stream, or nothing).
    wave: RefCell<Option<wave::Reading>>,
    wave_rev: Cell<i32>,
    wave_done: Cell<bool>,
    /// The user's marks, as last read or edited here.
    marks: RefCell<Marks>,
    /// The transport moving on between polls (None: not playing), and
    /// the progress the windows show.
    glide: Cell<Option<Glide>>,
    shown: Cell<f32>,
    marks_stamp: Cell<Option<std::time::SystemTime>>,
    marks_read: Cell<bool>,
    /// A name typed and not yet saved (saved at the next poll).
    marks_dirty: Cell<bool>,
    mark_rows: Rc<VecModel<MarkRow>>,
    /// Each row shown: its time, and which of the file's marks it is
    /// (None: one of the file's own chapters).
    mark_places: RefCell<Vec<(f64, Option<usize>)>>,
    mark_focus: Cell<i32>,
    /// Lengths of queued files that have not played yet, probed in the
    /// background (0: could not tell), and the ones asked for.
    lengths: RefCell<HashMap<String, f64>>,
    lengths_asked: RefCell<HashSet<String>>,
    prober: crate::poll::Poller,
}

/// The file's chapters and the user's marks as one list in time order:
/// each row, its time and which mark it is (None for a chapter).
fn mark_list(s: &Snapshot, marks: &[marks::Mark]) -> Vec<(MarkRow, f64, Option<usize>)> {
    let mut rows: Vec<(MarkRow, f64, Option<usize>)> = s
        .chapters
        .iter()
        .enumerate()
        .map(|(i, at)| {
            let name = s.chapter_titles.get(i).cloned().unwrap_or_default();
            (MarkRow { time: marks::stamp_of(*at).into(), name: name.into(), chapter: true }, *at, None)
        })
        .chain(marks.iter().enumerate().map(|(i, m)| {
            (MarkRow { time: marks::stamp_of(m.at).into(), name: m.name.as_str().into(), chapter: false }, m.at, Some(i))
        }))
        .collect();
    rows.sort_by(|a, b| a.1.total_cmp(&b.1));
    rows
}

/// "2 of 4 · 69:03": where the queue is, and its whole length once every
/// entry's is known.
fn queue_detail(at: i64, lengths: &[Option<f64>]) -> String {
    if lengths.len() < 2 {
        return String::new();
    }
    let place = format!("{} of {}", at + 1, lengths.len());
    match lengths.iter().copied().sum::<Option<f64>>() {
        Some(total) if total > 0.0 => format!("{place} · {}", dj::clock(total)),
        _ => place,
    }
}

/// The speeds the button steps through.
const SPEEDS: [f64; 6] = [1.0, 1.25, 1.5, 1.75, 2.0, 0.75];

/// The next of the usual speeds after this one (an odd speed: back to 1).
fn next_speed(speed: f64) -> f64 {
    match SPEEDS.iter().position(|s| (s - speed).abs() < 0.01) {
        Some(i) => SPEEDS[(i + 1) % SPEEDS.len()],
        None => 1.0,
    }
}

/// A quarter slower or faster, on the quarters, within 0.5 to 2.
fn stepped_speed(speed: f64, step: i32) -> f64 {
    (((speed * 4.0).round() + step as f64) / 4.0).clamp(0.5, 2.0)
}

/// "1x", "1.25x", "0.75x".
fn speed_label(speed: f64) -> String {
    let text = format!("{speed:.2}");
    format!("{}x", text.trim_end_matches('0').trim_end_matches('.'))
}

/// Repeat as one number: 0 off, 1 this one, 2 the whole queue.
fn repeat_mode(s: &Snapshot) -> u8 {
    if s.looping {
        1
    } else if s.repeat_all {
        2
    } else {
        0
    }
}

/// Off, this one, the whole queue (only where there is a queue), off.
fn next_repeat(mode: u8, queued: usize) -> u8 {
    match mode {
        0 => 1,
        1 if queued > 1 => 2,
        _ => 0,
    }
}

/// The A-B loop for the waveform: (0 none · 1 A set · 2 looping, A, B) with
/// the ends 0 to 1 and in order.
fn ab_shown(s: &Snapshot) -> (i32, f32, f32) {
    if s.duration <= 0.0 || s.ab_a < 0.0 {
        return (0, 0.0, 0.0);
    }
    let at = |seconds: f64| (seconds / s.duration).clamp(0.0, 1.0) as f32;
    if s.ab_b < 0.0 {
        return (1, at(s.ab_a), at(s.ab_a));
    }
    (2, at(s.ab_a.min(s.ab_b)), at(s.ab_a.max(s.ab_b)))
}

/// A queue line for a source: its title and length when it has played
/// before, else its file name.
fn queue_row(path: &str, history: &History, length: Option<f64>, current: bool, resting: bool) -> QueueRow {
    let known = history.entries.iter().find(|e| e.path == path);
    let title = known.map(|e| e.title.as_str()).filter(|t| !t.is_empty()).map_or_else(|| carts::title_of(path), str::to_owned);
    let time = length.map_or_else(String::new, dj::clock);
    QueueRow { title: title.into(), time: time.into(), current, resting }
}

/// A queued source's length: from when it played, else as probed.
fn length_of(path: &str, history: &History, probed: &HashMap<String, f64>) -> Option<f64> {
    history
        .entries
        .iter()
        .find(|e| e.path == path)
        .map(|e| e.duration)
        .filter(|d| *d > 0.0)
        .or_else(|| probed.get(path).copied().filter(|d| *d > 0.0))
}

/// A line of what played lately: where a long one was left, else its length.
fn recent_row(entry: &history::Entry) -> QueueRow {
    let title = if entry.title.is_empty() { carts::title_of(&entry.path) } else { entry.title.clone() };
    let time = if entry.position > 0.0 {
        format!("{} / {}", dj::clock(entry.position), dj::clock(entry.duration))
    } else if entry.duration > 0.0 {
        dj::clock(entry.duration)
    } else {
        String::new()
    };
    QueueRow { title: title.into(), time: time.into(), current: false, resting: false }
}

/// Lines of "played lately" shown.
const RECENT_SHOWN: usize = 12;

/// The cart wall as the windows hold it: the board (saved in
/// `carts.json`) and the rows shown. One model serves every Player
/// window, and rows change in place, so a poll never rebuilds a pad
/// under the pointer.
#[derive(Default)]
pub(super) struct CartsUi {
    board: RefCell<Board>,
    /// The board file's time when read: another window or the CLI
    /// changing it is seen at the next poll.
    stamp: Cell<Option<std::time::SystemTime>>,
    loaded: Cell<bool>,
    /// Edited here and not yet saved (saved at the next poll: typing a
    /// name is one write, not one per key).
    dirty: Cell<bool>,
    /// Hold releases belong to the bank where the press began.
    held: RefCell<HashMap<usize, (usize, bool)>>,
    pads: Rc<VecModel<CartPad>>,
    banks: Rc<VecModel<CartBank>>,
    picker: crate::poll::Poller,
    picking: Cell<bool>,
    /// Where the chooser's sounds go: (bank, pad).
    target: Cell<(usize, usize)>,
    /// A Player window is open: the spare voice is kept ready.
    open: Cell<bool>,
    /// A press the voices have not answered yet, shown at once.
    pressed: Cell<Option<(usize, usize, Press, Instant)>>,
    /// Shows the truth soon after a press, without waiting for a poll.
    settle: slint::Timer,
    /// The playing pads of the shown bank moving on between polls:
    /// (bank, row, glide).
    glides: RefCell<Vec<(usize, usize, Glide)>>,
}

/// A playing sound's bar between two readings of it. The Player reads
/// positions 5 times a second; the bars move on from the last reading
/// and are set again only when they have moved about a pixel, so a
/// playing file redraws a few times a second, not at the display's rate
/// (an `animate` on the bars did that).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Glide {
    at: Instant,
    /// Where it stood then (s), the reading it came from, its length (s)
    /// and how fast it plays (1: as recorded).
    from: f64,
    read: f64,
    length: f64,
    speed: f64,
}

/// How far the transport's waveform moves before it is drawn again (a
/// pixel of a bar up to 2000 pixels wide), and a pad's foot (400).
const TRANSPORT_STEP: f32 = 1.0 / 2000.0;
const PAD_STEP: f32 = 1.0 / 400.0;

impl Glide {
    /// A new reading of a playing sound. The same reading again (the
    /// player had none newer) changes nothing; one a little behind where
    /// the bar already is (it is a poll old) does not pull the bar back;
    /// a seek does.
    fn read(old: Option<Glide>, now: Instant, at: f64, length: f64, speed: f64) -> Glide {
        let old = old.filter(|g| g.length == length && g.speed == speed);
        if let Some(g) = old.filter(|g| g.read == at) {
            return g;
        }
        let from = match old.map(|g| g.seconds(now)) {
            Some(shown) if (0.0..0.3).contains(&(shown - at)) => shown,
            _ => at,
        };
        Glide { at: now, from, read: at, length, speed }
    }

    fn seconds(&self, now: Instant) -> f64 {
        (self.from + now.saturating_duration_since(self.at).as_secs_f64() * self.speed).min(self.length)
    }

    fn progress(&self, now: Instant) -> f32 {
        if self.length > 0.0 {
            (self.seconds(now) / self.length).clamp(0.0, 1.0) as f32
        } else {
            0.0
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Press {
    Start,
    Cut,
    Fade,
}

/// How long a press stands in for the voices' answer.
const PRESS_SHOWN: Duration = Duration::from_millis(700);

/// A pad's time: minutes and seconds, no leading zero ("0:30").
fn pad_clock(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// One pad as shown: its board entry, what its voice says, and a press
/// still waiting for that.
fn pad_row(index: usize, pad: &Pad, playing: Option<&Playing>, press: Option<Press>) -> CartPad {
    let (mut on, mut fading) = (playing.is_some(), playing.is_some_and(|p| p.fading));
    match press {
        Some(Press::Start) => on = true,
        Some(Press::Cut) => (on, fading) = (false, false),
        Some(Press::Fade) if on => fading = true,
        _ => {}
    }
    let playing = playing.filter(|_| on);
    let length = playing.map_or(pad.seconds, |p| if p.duration > 0.0 { p.duration } else { pad.seconds });
    let time = match playing {
        // Counting down while it plays; a loop or a stream counts up.
        Some(p) if p.remaining() > 0.0 => format!("-{}", pad_clock(p.remaining())),
        Some(p) if p.looped || p.duration <= 0.0 => pad_clock(p.position),
        _ if length > 0.0 => pad_clock(length),
        _ => String::new(),
    };
    CartPad {
        key: carts::KEYS.get(index).copied().unwrap_or_default().into(),
        label: pad.name().into(),
        time: time.into(),
        assigned: pad.assigned(),
        playing: on,
        fading,
        looped: pad.looped,
        fade: pad.fade,
        progress: playing
            .filter(|p| p.duration > 0.0)
            .map_or(0.0, |p| (p.position / p.duration).clamp(0.0, 1.0) as f32),
        gain: (pad.gain / 100.0) as f32,
        hold: pad.hold,
        duck: pad.duck,
        colour: pad.colour as i32,
    }
}

/// A board and its playing sounds for the headless shots: a podcast's
/// wall mid-show.
fn demo_carts() -> (Board, Vec<Playing>) {
    let mut board = Board::default();
    let pad = |label: &str, seconds: f64, looped: bool, fade: bool| Pad {
        label: label.into(),
        file: format!("/podcast/{label}.wav"),
        seconds,
        looped,
        fade,
        ..Pad::default()
    };
    let sounds = [
        pad("Show intro", 24.0, false, true),
        pad("Cold open sting", 4.0, false, false),
        pad("Theme bed", 92.0, true, true),
        pad("Welcome back", 8.0, false, false),
        pad("Sponsor read · Acme", 30.0, false, false),
        pad("Mid-roll bumper", 6.0, false, false),
        pad("Listener mail", 11.0, false, false),
        pad("Applause", 5.0, false, false),
        pad("Outro", 45.0, false, true),
    ];
    for (i, sound) in sounds.into_iter().enumerate() {
        board.banks[0].pads[i] = sound;
    }
    board.banks[1].pads[0] = pad("Acme 30 s", 30.0, false, false);
    {
        // The settings a pad can carry, as they look.
        let pads = &mut board.banks[0].pads;
        (pads[0].colour, pads[2].colour, pads[4].colour, pads[7].colour) = (4, 3, 2, 5);
        pads[4].duck = true;
        pads[1].hold = true;
    }
    let play = |pad: usize, position: f64, duration: f64, looped: bool| {
        let mut p = Playing { bank: 0, pad, position, duration, looped, ..Playing::default() };
        p.label = board.banks[0].pads[pad].name();
        p
    };
    let playing = vec![play(0, 9.0, 24.0, false), play(2, 37.0, 92.0, true)];
    (board, playing)
}

impl AudioApp {
    pub(super) fn bind_meter_style(&self, w: &PatchWindow) {
        let style = w.global::<crate::ui::MeterStyle>();
        show_meter_look(&style, self.meter_look.get());
        style.on_ladder(crate::meter_style::ladder);
        let me = self.me.clone();
        // A click on a scale tries the next style for the session; the
        // default stays the setting's (Settings · Meters).
        style.on_toggle(move || {
            let Some(a) = me.upgrade() else { return };
            a.set_meter_look(a.meter_look.get().next());
        });
    }

    /// Every window's meters in `look`, now.
    pub(super) fn set_meter_look(&self, look: crate::meter_style::Look) {
        self.meter_look.set(look);
        for w in self.windows.borrow().iter().filter_map(|w| w.upgrade()) {
            show_meter_look(&w.global::<crate::ui::MeterStyle>(), look);
        }
    }

    pub(super) fn bind_player(&self, w: &PatchWindow) {
        let p = w.global::<PlayerState>();
        let me = self.me.clone();
        p.on_browse(move || {
            if let Some(a) = me.upgrade()
                && !a.player_picking.get()
                && a.player_picker
                    .request_with("file", serde_json::Value::Null, |_| {
                        crate::portal::pick_file("Open audio for playback")
                            .map(|path| serde_json::json!(path))
                    })
            {
                a.player_picking.set(true);
            }
        });
        let me = self.me.clone();
        p.on_load(move |path| {
            if let Some(a) = me.upgrade() {
                a.player.send(Action::Load(path.trim().to_string()));
            }
        });
        let me = self.me.clone();
        p.on_toggle(move || {
            let Some(a) = me.upgrade() else { return };
            // Nothing loaded and a queue from last time: it goes on.
            let resting = a.desk.resting.borrow().clone();
            if a.queue_rests() && !resting.items.is_empty() {
                a.player.send(Action::Restore(resting.items, resting.current));
            } else {
                a.player.send(Action::Toggle);
            }
        });
        let me = self.me.clone();
        p.on_stop(move || {
            if let Some(a) = me.upgrade() {
                a.player.send(Action::Stop);
            }
        });
        let me = self.me.clone();
        p.on_seek(move |v| {
            if let Some(a) = me.upgrade() {
                a.player.send(Action::Seek(v as f64 * 100.0));
            }
        });
        let me = self.me.clone();
        p.on_set_volume(move |v| {
            if let Some(a) = me.upgrade() {
                a.player.send(Action::Volume(v as f64 * 100.0));
            }
        });
        let me = self.me.clone();
        p.on_skip(move |seconds| {
            if let Some(a) = me.upgrade() {
                a.player.send(Action::Skip(seconds as f64));
            }
        });
        let me = self.me.clone();
        p.on_set_loop(move |v| {
            if let Some(a) = me.upgrade() {
                a.player.send(Action::Loop(v));
            }
        });
        let me = self.me.clone();
        p.on_cue(move || {
            if let Some(a) = me.upgrade() {
                a.player.send(Action::Cue);
            }
        });
        let me = self.me.clone();
        p.on_set_pfl(move |v| {
            if let Some(a) = me.upgrade() {
                a.player.send(Action::Pfl(v));
            }
        });
        let me = self.me.clone();
        p.on_next(move || {
            if let Some(a) = me.upgrade() {
                a.player.send(Action::Next);
            }
        });
        let me = self.me.clone();
        p.on_previous(move || {
            if let Some(a) = me.upgrade() {
                a.player.send(Action::Previous);
            }
        });
        self.bind_desk(&p);
        self.bind_carts(&p);
        if self.frozen {
            // The shots: a show in progress, not an empty window.
            p.set_loaded(true);
            p.set_seekable(true);
            p.set_playing(true);
            p.set_status("playing".into());
            p.set_title("Episode 42 · interview with Mara Lind".into());
            p.set_file("~/Podcasts/episode-42/interview-mara-lind.flac".into());
            p.set_audio("48.0 kHz · 2 channels".into());
            p.set_time("12:41 / 48:06".into());
            p.set_remaining("-35:25".into());
            p.set_progress(0.264);
        }
    }

    /// The queue, what played lately, the waveform, speed, repeat and the
    /// A-B loop.
    fn bind_desk(&self, p: &PlayerState<'_>) {
        p.set_queue(ModelRc::from(self.desk.queue.clone()));
        p.set_recent(ModelRc::from(self.desk.recent.clone()));
        // A window opens on the queue unless the pads hold sounds.
        if !self.carts.loaded.get() && !self.frozen {
            self.carts.loaded.set(true);
            self.carts_read();
        }
        // (The shots show the pads; PATCH_SHOT_PLAYER=queue shows the queue.)
        p.set_prefers_queue(if self.frozen {
            std::env::var("PATCH_SHOT_PLAYER").is_ok_and(|v| v == "queue")
        } else {
            !self.carts.board.borrow().banks.iter().flat_map(|b| &b.pads).any(Pad::assigned)
        });
        let me = self.me.clone();
        p.on_wave(move |width, height, _rev| {
            let Some(a) = me.upgrade() else { return slint::Image::default() };
            let (width, height) = (width.max(1) as u32, height.max(1) as u32);
            let picture = if a.frozen {
                wave::mask(&wave::demo(), width, height)
            } else {
                match a.desk.wave.borrow().as_ref() {
                    Some(reading) => wave::mask(&reading.wave.lock().unwrap(), width, height),
                    None => return slint::Image::default(),
                }
            };
            slint::Image::from_rgba8(picture)
        });
        let me = self.me.clone();
        p.on_queue_add(move || {
            let Some(a) = me.upgrade() else { return };
            if a.frozen || a.desk.picking.get() {
                return;
            }
            let asked = a.desk.picker.request_with("queue-files", serde_json::Value::Null, |_| {
                let files = crate::portal::pick_files("Add audio to the queue")?;
                Ok(serde_json::json!(files.iter().map(|f| f.to_string_lossy().into_owned()).collect::<Vec<_>>()))
            });
            if asked {
                a.desk.picking.set(true);
            }
        });
        let me = self.me.clone();
        p.on_queue_play(move |i| {
            let Some(a) = me.upgrade() else { return };
            let index = i.max(0) as usize;
            if a.queue_rests() {
                let resting = a.desk.resting.borrow().clone();
                a.player.send(Action::Restore(resting.items, index));
            } else {
                a.player.send(Action::Jump(index));
            }
        });
        let me = self.me.clone();
        p.on_queue_remove(move |i| {
            if let Some(a) = me.upgrade() {
                a.player.send(Action::Remove(i.max(0) as usize));
            }
        });
        let me = self.me.clone();
        p.on_queue_up(move |i| {
            if let Some(a) = me.upgrade() {
                a.player.send(Action::Move { index: i.max(0) as usize, up: true });
            }
        });
        let me = self.me.clone();
        p.on_queue_clear(move || {
            let Some(a) = me.upgrade() else { return };
            if a.queue_rests() {
                // Last time's queue: forgotten.
                *a.desk.resting.borrow_mut() = Queue::default();
                Queue::default().save();
            } else {
                a.player.send(Action::ClearQueue);
            }
        });
        let me = self.me.clone();
        p.on_queue_shuffle(move || {
            if let Some(a) = me.upgrade() {
                a.player.send(Action::Shuffle);
            }
        });
        let me = self.me.clone();
        p.on_recent_play(move |i| {
            let Some(a) = me.upgrade() else { return };
            // The row as shown: what plays now is not among them.
            let playing = a.player.snapshot().map(|s| s.path).unwrap_or_default();
            let path = a
                .desk
                .history
                .borrow()
                .entries
                .iter()
                .filter(|e| e.path != playing)
                .nth(i.max(0) as usize)
                .map(|e| e.path.clone());
            if let Some(path) = path {
                a.player.send(Action::Load(path));
            }
        });
        let me = self.me.clone();
        p.on_speed_step(move |step| {
            let Some(a) = me.upgrade() else { return };
            let speed = a.player.snapshot().map_or(1.0, |s| if s.speed > 0.0 { s.speed } else { 1.0 });
            a.player.send(Action::Speed(if step == 0 { next_speed(speed) } else { stepped_speed(speed, step) }));
        });
        let me = self.me.clone();
        p.on_ab_loop(move || {
            if let Some(a) = me.upgrade() {
                a.player.send(Action::AbLoop);
            }
        });
        let me = self.me.clone();
        p.on_repeat_next(move || {
            let Some(a) = me.upgrade() else { return };
            if let Some(s) = a.player.snapshot() {
                a.player.send(Action::Repeat(next_repeat(repeat_mode(&s), s.queue.len())));
            }
        });
        p.set_mark_rows(ModelRc::from(self.desk.mark_rows.clone()));
        let me = self.me.clone();
        p.on_mark_add(move || {
            let Some(a) = me.upgrade() else { return };
            let Some(s) = a.player.snapshot().filter(|s| s.ready && !s.path.is_empty()) else { return };
            let index = a.desk.marks.borrow_mut().add(&s.path, s.position_now());
            a.desk.marks_dirty.set(true);
            a.show_desk(&s);
            // Its line, selected, with its name asked for.
            let row = a.desk.mark_places.borrow().iter().position(|(_, mark)| *mark == Some(index));
            if let Some(row) = row {
                a.desk.mark_focus.set(a.desk.mark_focus.get().wrapping_add(1));
                for w in a.windows.borrow().iter().filter_map(|w| w.upgrade()) {
                    let p = w.global::<PlayerState>();
                    p.set_mark_selected(row as i32);
                    p.set_mark_focus(a.desk.mark_focus.get());
                }
            }
        });
        let me = self.me.clone();
        p.on_mark_jump(move |row| {
            let Some(a) = me.upgrade() else { return };
            let place = a.desk.mark_places.borrow().get(row.max(0) as usize).map(|(at, _)| *at);
            if let Some(at) = place {
                a.player.send(Action::Goto(at));
            }
        });
        let me = self.me.clone();
        p.on_mark_remove(move |row| {
            let Some(a) = me.upgrade() else { return };
            let Some(s) = a.player.snapshot() else { return };
            let mark = a.desk.mark_places.borrow().get(row.max(0) as usize).and_then(|(_, mark)| *mark);
            if let Some(index) = mark {
                a.desk.marks.borrow_mut().remove(&s.path, index);
                a.desk.marks_dirty.set(true);
                a.show_desk(&s);
            }
        });
        let me = self.me.clone();
        p.on_mark_name(move |row, name| {
            let Some(a) = me.upgrade() else { return };
            let Some(s) = a.player.snapshot() else { return };
            let mark = a.desk.mark_places.borrow().get(row.max(0) as usize).and_then(|(_, mark)| *mark);
            if let Some(index) = mark {
                a.desk.marks.borrow_mut().rename(&s.path, index, &name);
                a.desk.marks_dirty.set(true);
                a.show_desk(&s);
            }
        });
        let me = self.me.clone();
        p.on_mark_step(move |step| {
            let Some(a) = me.upgrade() else { return };
            let Some(s) = a.player.snapshot() else { return };
            let places: Vec<f64> = a.desk.mark_places.borrow().iter().map(|(at, _)| *at).collect();
            if let Some(at) = marks::step(&places, s.position_now(), step) {
                a.player.send(Action::Goto(at));
            }
        });
        let me = self.me.clone();
        p.on_marks_copy(move || {
            let Some(a) = me.upgrade() else { return };
            let places: Vec<(f64, String)> = a
                .desk
                .mark_places
                .borrow()
                .iter()
                .zip(a.desk.mark_rows.iter())
                .map(|((at, _), row)| (*at, row.name.to_string()))
                .collect();
            let text = marks::notes(&places);
            let copied = std::process::Command::new("wl-copy")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    use std::io::Write;
                    child.stdin.take().map(|mut input| input.write_all(text.as_bytes())).transpose()?;
                    Ok(())
                })
                .is_ok();
            a.player.send(Action::Notice(if copied {
                format!("Copied {} lines of timestamps.", places.len())
            } else {
                "wl-copy is not installed: the list could not be copied.".into()
            }));
        });
        if self.frozen {
            let mark = |time: &str, name: &str, chapter: bool| MarkRow { time: time.into(), name: name.into(), chapter };
            set_rows(
                &self.desk.mark_rows,
                [
                    mark("00:00", "Cold open", true),
                    mark("01:55", "Theme", true),
                    mark("07:12", "How Mara started", false),
                    mark("12:41", "Sponsor read", false),
                    mark("22:36", "Break", true),
                    mark("31:02", "", false),
                    mark("46:25", "Outro", true),
                ]
                .into_iter(),
            );
            p.set_user_marks(ModelRc::new(VecModel::from(vec![0.15f32, 0.264, 0.645])));
            // The shots: a show's queue, mid-way.
            let demo = |title: &str, time: &str, current: bool| QueueRow { title: title.into(), time: time.into(), current, resting: false };
            set_rows(
                &self.desk.queue,
                [
                    demo("Cold open", "01:12", false),
                    demo("Episode 42 · interview with Mara Lind", "48:06", true),
                    demo("Listener questions", "17:40", false),
                    demo("Outro and credits", "02:05", false),
                ]
                .into_iter(),
            );
            set_rows(
                &self.desk.recent,
                [
                    demo("Episode 41 · the long way round", "31:18 / 52:44", false),
                    demo("Room tone, studio B", "00:30", false),
                    demo("Theme bed (full mix)", "03:12", false),
                ]
                .into_iter(),
            );
            p.set_queue_detail("2 of 4".into());
            p.set_has_wave(true);
            p.set_can_play(true);
            p.set_artist("The Long Way Round".into());
            p.set_marks(ModelRc::new(VecModel::from(vec![0.04f32, 0.47, 0.53, 0.965])));
        }
    }

    /// The player is not running: the queue shown is last time's.
    fn queue_rests(&self) -> bool {
        self.player.snapshot().is_none_or(|s| s.queue.is_empty() && s.path.is_empty())
    }

    /// The queue, what played lately and the waveform for what the player
    /// reports now.
    fn show_desk(&self, s: &Snapshot) {
        let desk = &self.desk;
        // The history: read again when another window or the CLI wrote it.
        let stamp = history::stamp();
        if !desk.history_read.replace(true) || stamp != desk.history_stamp.get() {
            *desk.history.borrow_mut() = History::load();
            desk.history_stamp.set(stamp);
        }
        let resting = s.queue.is_empty() && s.path.is_empty();
        if !resting {
            desk.resting_read.set(false);
        } else if !desk.resting_read.replace(true) {
            *desk.resting.borrow_mut() = Queue::load();
        }
        // Lengths probed in the background for what has not played yet.
        if let Some(reply) = desk.prober.take("lengths")
            && let Ok(found) = reply.result
        {
            let mut lengths = desk.lengths.borrow_mut();
            for (path, seconds) in found.as_object().into_iter().flatten() {
                lengths.insert(path.clone(), seconds.as_f64().unwrap_or(0.0));
            }
        }
        let history = desk.history.borrow();
        let rested = desk.resting.borrow();
        let items: &[String] = if resting { &rested.items } else { &s.queue };
        let lengths: Vec<Option<f64>> = items.iter().map(|path| length_of(path, &history, &desk.lengths.borrow())).collect();
        let current = if resting { rested.current as i64 } else { s.queue_at };
        set_rows(
            &desk.queue,
            items.iter().zip(&lengths).enumerate().map(|(i, (path, length))| queue_row(path, &history, *length, i as i64 == current, resting)),
        );
        let detail = if resting {
            if rested.items.is_empty() { String::new() } else { "from last time · Play goes on".to_owned() }
        } else {
            queue_detail(s.queue_at, &lengths)
        };
        // Ask for the lengths still unknown: local files, a few at a time, once each.
        let unknown: Vec<String> = {
            let asked = desk.lengths_asked.borrow();
            items
                .iter()
                .zip(&lengths)
                .filter(|(path, length)| length.is_none() && !path.contains("://") && !asked.contains(*path))
                .map(|(path, _)| path.clone())
                .take(8)
                .collect()
        };
        if !unknown.is_empty()
            && !self.frozen
            && desk.prober.request_with("lengths", serde_json::json!(unknown), |paths| {
                Ok(serde_json::Value::Object(
                    paths
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|p| p.as_str())
                        .map(|path| (path.to_owned(), serde_json::json!(carts::seconds_of(path))))
                        .collect(),
                ))
            })
        {
            desk.lengths_asked.borrow_mut().extend(unknown);
        }
        // The marks: read again when another window or the CLI wrote them;
        // a name typed here is saved now.
        if desk.marks_dirty.replace(false) {
            desk.marks.borrow().save();
            desk.marks_stamp.set(marks::stamp());
            desk.marks_read.set(true);
        } else {
            let stamp = marks::stamp();
            if !desk.marks_read.replace(true) || stamp != desk.marks_stamp.get() {
                *desk.marks.borrow_mut() = Marks::load();
                desk.marks_stamp.set(stamp);
            }
        }
        let listed = mark_list(s, desk.marks.borrow().of(&s.path));
        *desk.mark_places.borrow_mut() = listed.iter().map(|(_, at, mark)| (*at, *mark)).collect();
        let user_marks: Vec<f32> = if s.duration > 0.0 {
            listed.iter().filter(|(_, at, mark)| mark.is_some() && *at < s.duration).map(|(_, at, _)| (at / s.duration) as f32).collect()
        } else {
            Vec::new()
        };
        set_rows(&desk.mark_rows, listed.into_iter().map(|(row, _, _)| row));
        set_rows(&desk.recent, history.entries.iter().filter(|e| e.path != s.path).take(RECENT_SHOWN).map(recent_row));
        // The waveform: of a file with a length, read once per file.
        let local = s.ready && s.duration > 0.0 && !s.path.contains("://");
        let shown = desk.wave.borrow().as_ref().map(|r| r.path.clone());
        if local && shown.as_deref() != Some(s.path.as_str()) {
            *desk.wave.borrow_mut() = Some(wave::read(&s.path, s.duration));
            desk.wave_done.set(false);
        } else if shown.is_some() && s.path.is_empty() {
            *desk.wave.borrow_mut() = None;
            desk.wave_rev.set(desk.wave_rev.get().wrapping_add(1));
        }
        // It fills in while it is read: drawn again each poll until done.
        if !desk.wave_done.get()
            && let Some(reading) = desk.wave.borrow().as_ref()
        {
            desk.wave_done.set(reading.wave.lock().unwrap().done);
            desk.wave_rev.set(desk.wave_rev.get().wrapping_add(1));
        }
        let (ab, ab_a, ab_b) = ab_shown(s);
        let marks: Vec<f32> = if s.duration > 0.0 {
            s.chapters
                .iter()
                .copied()
                .chain((s.cue_point > 0.0).then_some(s.cue_point))
                .filter(|t| *t > 0.0 && *t < s.duration)
                .map(|t| (t / s.duration) as f32)
                .collect()
        } else {
            Vec::new()
        };
        let speed = if s.speed > 0.0 { s.speed } else { 1.0 };
        for w in self.windows.borrow().iter().filter_map(|w| w.upgrade()) {
            let p = w.global::<PlayerState>();
            p.set_queue_detail(detail.as_str().into());
            p.set_queue_resting(resting && !rested.items.is_empty());
            p.set_can_play(s.ready || (resting && !rested.items.is_empty()));
            p.set_has_wave(desk.wave.borrow().is_some());
            p.set_wave_rev(desk.wave_rev.get());
            p.set_artist(s.artist.as_str().into());
            p.set_ab(ab);
            p.set_ab_a(ab_a);
            p.set_ab_b(ab_b);
            p.set_repeat(repeat_mode(s) as i32);
            p.set_speed_label(speed_label(speed).into());
            p.set_speed_changed((speed - 1.0).abs() > 0.01);
            if !same_marks(&p.get_marks(), &marks) {
                p.set_marks(ModelRc::new(VecModel::from(marks.clone())));
            }
            if !same_marks(&p.get_user_marks(), &user_marks) {
                p.set_user_marks(ModelRc::new(VecModel::from(user_marks.clone())));
            }
        }
    }

    /// The cart wall: one board and one set of rows for every window.
    fn bind_carts(&self, p: &PlayerState<'_>) {
        if !self.carts.loaded.replace(true) {
            if self.frozen {
                *self.carts.board.borrow_mut() = demo_carts().0;
            } else {
                self.carts_read();
            }
        }
        p.set_carts(ModelRc::from(self.carts.pads.clone()));
        p.set_banks(ModelRc::from(self.carts.banks.clone()));
        self.show_carts();
        // This window is not among the windows yet: its own values here.
        {
            let board = self.carts.board.borrow();
            p.set_bank(board.bank as i32);
            p.set_solo(board.solo);
            p.set_carts_playing(if self.frozen { demo_carts().1.len() as i32 } else { 0 });
        }
        let me = self.me.clone();
        p.on_cart_fire(move |i| {
            let Some(a) = me.upgrade() else { return };
            let index = i as usize;
            let (bank, pad, solo) = {
                let board = a.carts.board.borrow();
                (board.bank, board.pad(board.bank, index).cloned(), board.solo)
            };
            let Some(pad) = pad else { return };
            let plays = a.player.carts().iter().find(|p| p.bank == bank && p.pad == index).map(|p| p.fading);
            let press = match plays {
                None => Press::Start,
                Some(false) if pad.fade => Press::Fade,
                Some(_) => Press::Cut,
            };
            a.carts.pressed.set(Some((bank, index, press, Instant::now())));
            a.player.send(Action::Cart(Do::Trigger { bank, index, pad, solo }));
            a.show_carts();
            a.settle_carts();
        });
        let me = self.me.clone();
        p.on_cart_stop_all(move |fade| {
            if let Some(a) = me.upgrade() {
                a.player.send(Action::Cart(Do::StopAll { fade }));
                a.settle_carts();
            }
        });
        let me = self.me.clone();
        p.on_cart_bank(move |bank| {
            if let Some(a) = me.upgrade() {
                a.edit_carts(|board| board.bank = (bank.max(0) as usize).min(carts::BANKS - 1));
            }
        });
        let me = self.me.clone();
        p.on_cart_solo(move |solo| {
            if let Some(a) = me.upgrade() {
                a.edit_carts(|board| board.solo = solo);
            }
        });
        let me = self.me.clone();
        p.on_bank_name(move |name| {
            if let Some(a) = me.upgrade() {
                a.edit_carts(|board| {
                    let bank = board.bank;
                    board.banks[bank].name = name.trim().to_string();
                    // An emptied name gets the bank's own back.
                    board.tidy();
                });
            }
        });
        let me = self.me.clone();
        p.on_cart_label(move |i, label| {
            if let Some(a) = me.upgrade() {
                a.edit_pad(i, |pad| pad.label = label.trim().to_string());
            }
        });
        let me = self.me.clone();
        p.on_cart_loop(move |i, looped| {
            if let Some(a) = me.upgrade() {
                a.edit_pad(i, |pad| pad.looped = looped);
            }
        });
        let me = self.me.clone();
        p.on_cart_fade(move |i, fade| {
            if let Some(a) = me.upgrade() {
                a.edit_pad(i, |pad| pad.fade = fade);
            }
        });
        let me = self.me.clone();
        p.on_cart_down(move |i| {
            let Some(a) = me.upgrade() else { return };
            let index = i.max(0) as usize;
            let (bank, pad, solo) = {
                let board = a.carts.board.borrow();
                (board.bank, board.pad(board.bank, index).cloned(), board.solo)
            };
            let Some(pad) = pad.filter(|p| p.assigned()) else { return };
            if a.carts.held.borrow().contains_key(&index) {
                return;
            }
            a.carts.held.borrow_mut().insert(index, (bank, pad.fade));
            a.carts.pressed.set(Some((bank, index, Press::Start, Instant::now())));
            a.player.send(Action::Cart(Do::Start { bank, index, pad, solo }));
            a.show_carts();
        });
        let me = self.me.clone();
        p.on_cart_up(move |i| {
            let Some(a) = me.upgrade() else { return };
            let index = i.max(0) as usize;
            let Some((bank, fade)) = a.carts.held.borrow_mut().remove(&index) else { return };
            a.carts.pressed.set(Some((bank, index, if fade { Press::Fade } else { Press::Cut }, Instant::now())));
            a.player.send(Action::Cart(Do::Stop { bank, index, fade }));
            a.show_carts();
            a.settle_carts();
        });
        let me = self.me.clone();
        p.on_cart_hold(move |i, hold| {
            if let Some(a) = me.upgrade() {
                a.edit_pad(i, |pad| pad.hold = hold);
            }
        });
        let me = self.me.clone();
        p.on_cart_duck(move |i, duck| {
            if let Some(a) = me.upgrade() {
                a.edit_pad(i, |pad| pad.duck = duck);
            }
        });
        let me = self.me.clone();
        p.on_cart_colour(move |i| {
            if let Some(a) = me.upgrade() {
                a.edit_pad(i, |pad| pad.colour = (pad.colour + 1) % (carts::COLOURS + 1));
            }
        });
        let me = self.me.clone();
        p.on_cart_gain(move |i, gain| {
            let Some(a) = me.upgrade() else { return };
            let gain = (gain as f64 * 100.0).clamp(0.0, 100.0);
            a.edit_pad(i, |pad| pad.gain = gain);
            // Heard at once on a pad that plays.
            let bank = a.carts.board.borrow().bank;
            let index = i.max(0) as usize;
            if a.player.carts().iter().any(|p| p.bank == bank && p.pad == index) {
                a.player.send(Action::Cart(Do::Gain { bank, index, gain }));
            }
        });
        let me = self.me.clone();
        p.on_cart_clear(move |i| {
            let Some(a) = me.upgrade() else { return };
            let bank = a.carts.board.borrow().bank;
            let index = i.max(0) as usize;
            if a.player.carts().iter().any(|p| p.bank == bank && p.pad == index) {
                a.player.send(Action::Cart(Do::Stop { bank, index, fade: false }));
                a.settle_carts();
            }
            a.edit_pad(i, |pad| *pad = Pad::default());
        });
        let me = self.me.clone();
        p.on_cart_files(move |i| {
            let Some(a) = me.upgrade() else { return };
            if a.frozen || a.carts.picking.get() {
                return;
            }
            let asked = a.carts.picker.request_with("cart-files", serde_json::Value::Null, |_| {
                // The lengths are read here too, off the UI thread.
                let files = crate::portal::pick_files("Choose sounds for the pads")?;
                Ok(serde_json::Value::Array(
                    files
                        .iter()
                        .map(|f| {
                            let file = f.to_string_lossy();
                            serde_json::json!({"file": file, "seconds": carts::seconds_of(&file)})
                        })
                        .collect(),
                ))
            });
            if asked {
                a.carts.picking.set(true);
                a.carts.target.set((a.carts.board.borrow().bank, i.max(0) as usize));
            }
        });
    }

    /// Change the board, show it now and save it at the next poll.
    fn edit_carts(&self, change: impl FnOnce(&mut Board)) {
        change(&mut self.carts.board.borrow_mut());
        self.carts.dirty.set(true);
        self.show_carts();
    }

    /// Change one pad of the shown bank.
    fn edit_pad(&self, index: i32, change: impl FnOnce(&mut Pad)) {
        self.edit_carts(|board| {
            let bank = board.bank;
            if let Some(pad) = usize::try_from(index).ok().and_then(|i| board.banks[bank].pads.get_mut(i)) {
                change(pad);
            }
        });
    }

    /// The board from its file, and the file's time.
    fn carts_read(&self) {
        *self.carts.board.borrow_mut() = Board::load();
        self.carts.stamp.set(carts_stamp());
    }

    /// Save what was edited here; otherwise take what changed on disk.
    fn carts_sync(&self) {
        if self.frozen {
            return;
        }
        if self.carts.dirty.replace(false) {
            if let Err(error) = self.carts.board.borrow().save() {
                self.player.send(crate::player::Action::Notice(error));
            }
            self.carts.stamp.set(carts_stamp());
        } else if carts_stamp() != self.carts.stamp.get() {
            self.carts_read();
        }
    }

    /// The voices answer a press within a few tens of milliseconds: show
    /// that then, not a whole poll later.
    fn settle_carts(&self) {
        let me = self.me.clone();
        self.carts.settle.start(slint::TimerMode::SingleShot, Duration::from_millis(180), move || {
            if let Some(a) = me.upgrade() {
                a.player.send(crate::player::Action::Poll);
                a.show_carts();
            }
        });
    }

    /// The shown bank's pads and the bank tabs, rows changed in place.
    fn show_carts(&self) {
        let playing = if self.frozen { demo_carts().1 } else { self.player.carts() };
        let board = self.carts.board.borrow();
        let press = self.carts.pressed.get().filter(|p| p.3.elapsed() < PRESS_SHOWN);
        let bank = board.bank.min(board.banks.len().saturating_sub(1));
        let Some(shown) = board.banks.get(bank) else { return };
        let now = Instant::now();
        let old = self.carts.glides.take();
        let mut glides = Vec::new();
        let rows = shown.pads.iter().enumerate().map(|(i, pad)| {
            let on = playing.iter().find(|p| p.bank == bank && p.pad == i);
            let mut row = pad_row(i, pad, on, press.filter(|p| p.0 == bank && p.1 == i).map(|p| p.2));
            if let Some(p) = on.filter(|p| row.playing && !p.looped && p.duration > 0.0 && !self.frozen) {
                let was = old.iter().find(|g| g.0 == bank && g.1 == i).map(|g| g.2);
                let glide = Glide::read(was, now, p.position, p.duration, 1.0);
                row.progress = glide.progress(now);
                glides.push((bank, i, glide));
            }
            row
        });
        set_rows(&self.carts.pads, rows);
        *self.carts.glides.borrow_mut() = glides;
        let tabs = board.banks.iter().enumerate().map(|(i, b)| CartBank {
            name: b.name.as_str().into(),
            live: playing.iter().any(|p| p.bank == i),
        });
        set_rows(&self.carts.banks, tabs);
        for w in self.windows.borrow().iter().filter_map(|w| w.upgrade()) {
            let p = w.global::<PlayerState>();
            p.set_bank(bank as i32);
            p.set_solo(board.solo);
            p.set_carts_playing(playing.len() as i32);
        }
    }

    pub(super) fn poll_player(&self) {
        if self.frozen {
            return;
        }
        if let Some(reply) = self.player_picker.take("file") {
            self.player_picking.set(false);
            match reply.result {
                Ok(value) => {
                    if let Some(path) = value.as_str() {
                        self.player
                            .send(crate::player::Action::Load(path.to_owned()));
                    }
                }
                Err(error) => self.player.send(crate::player::Action::Notice(error)),
            }
        }
        if let Some(reply) = self.desk.picker.take("queue-files") {
            self.desk.picking.set(false);
            match reply.result {
                Ok(value) => {
                    let files: Vec<String> =
                        value.as_array().into_iter().flatten().filter_map(|f| f.as_str().map(str::to_owned)).collect();
                    if !files.is_empty() {
                        // Onto last time's queue when that is what shows.
                        if self.queue_rests() && !self.desk.resting.borrow().items.is_empty() {
                            let mut queue = self.desk.resting.borrow().clone();
                            queue.items.extend(files);
                            queue.save();
                            *self.desk.resting.borrow_mut() = queue;
                        } else {
                            self.player.send(crate::player::Action::Enqueue(files));
                        }
                    }
                }
                Err(error) => self.player.send(crate::player::Action::Notice(error)),
            }
        }
        if let Some(reply) = self.carts.picker.take("cart-files") {
            self.carts.picking.set(false);
            match reply.result {
                Ok(value) => {
                    let files: Vec<(String, f64)> = value
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|f| Some((f["file"].as_str()?.to_owned(), f["seconds"].as_f64().unwrap_or(0.0))))
                        .collect();
                    if !files.is_empty() {
                        let (bank, pad) = self.carts.target.get();
                        let mut placed = 0;
                        self.edit_carts(|board| placed = board.place(bank, pad, &files));
                        if placed < files.len() {
                            self.player.send(crate::player::Action::Notice(format!(
                                "{placed} of {} sounds found a free pad in this bank; the others were left out.",
                                files.len()
                            )));
                        }
                    }
                }
                Err(error) => self.player.send(crate::player::Action::Notice(error)),
            }
        }
        if !self
            .windows
            .borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .any(|w| w.get_surface() == "player")
        {
            // The last Player window closed: an edit still to save is
            // saved, and the spare voice goes (sounds play on).
            self.desk.glide.set(None);
            self.carts.glides.borrow_mut().clear();
            if self.carts.dirty.get() {
                self.carts_sync();
            }
            if self.desk.marks_dirty.replace(false) {
                self.desk.marks.borrow().save();
            }
            for (index, (bank, fade)) in self.carts.held.borrow_mut().drain() {
                self.player.send(Action::Cart(Do::Stop { bank, index, fade }));
            }
            if self.carts.open.replace(false) {
                self.player.send(crate::player::Action::Cart(Do::Rest));
            }
            return;
        }
        self.carts_sync();
        // A Player window with sounds on its pads: the spare voice waits,
        // so the first press is as quick as the rest.
        if !self.carts.open.get() && self.carts.board.borrow().banks.iter().flat_map(|b| &b.pads).any(Pad::assigned) {
            self.carts.open.set(true);
            self.player.send(crate::player::Action::Cart(Do::Warm));
        }
        self.player.send(crate::player::Action::Poll);
        self.show_carts();
        let Some(s) = self.player.snapshot() else {
            return;
        };
        self.show_desk(&s);
        let now = Instant::now();
        let glide = (s.playing && s.duration > 0.0)
            .then(|| Glide::read(self.desk.glide.get(), now, s.position, s.duration, s.speed));
        self.desk.glide.set(glide);
        let progress = match glide {
            Some(g) => g.progress(now),
            None if s.duration > 0.0 => (s.position / s.duration).clamp(0.0, 1.0) as f32,
            None => 0.0,
        };
        self.desk.shown.set(progress);
        for w in self.windows.borrow().iter().filter_map(|w| w.upgrade()) {
            let p = w.global::<PlayerState>();
            p.set_browsing(self.player_picking.get());
            p.set_loaded(s.ready);
            p.set_status(
                if s.loading {
                    "opening"
                } else if s.playing {
                    "playing"
                } else if s.ready {
                    "paused"
                } else {
                    "ready"
                }
                .into(),
            );
            p.set_audio(s.audio.as_str().into());
            p.set_seekable(s.ready && s.duration > 0.0);
            p.set_title(s.title.as_str().into());
            p.set_file(s.path.as_str().into());
            p.set_playing(s.playing);
            p.set_time(
                format!(
                    "{} / {}",
                    dj::clock(s.position),
                    if s.duration > 0.0 {
                        dj::clock(s.duration)
                    } else {
                        "LIVE".into()
                    }
                )
                .into(),
            );
            p.set_remaining(
                if s.duration > 0.0 {
                    format!("-{}", dj::clock(s.duration - s.position))
                } else {
                    String::new()
                }
                .into(),
            );
            p.set_progress(progress);
            p.set_volume((s.volume / 100.0) as f32);
            p.set_looping(s.looping);
            p.set_pfl(s.pfl);
            p.set_message(s.message.as_str().into());
        }
    }

    /// Every frame: the Player's bars move on from the last reading, and
    /// are set only when one has moved about a pixel.
    pub(super) fn glide_player(&self) {
        let now = Instant::now();
        if let Some(g) = self.desk.glide.get() {
            let progress = g.progress(now);
            if (progress - self.desk.shown.get()).abs() >= TRANSPORT_STEP {
                self.desk.shown.set(progress);
                for w in self.windows.borrow().iter().filter_map(|w| w.upgrade()) {
                    w.global::<PlayerState>().set_progress(progress);
                }
            }
        }
        for &(_, row, g) in self.carts.glides.borrow().iter() {
            let progress = g.progress(now);
            if let Some(mut pad) = self.carts.pads.row_data(row)
                && (progress - pad.progress).abs() >= PAD_STEP
            {
                pad.progress = progress;
                self.carts.pads.set_row_data(row, pad);
            }
        }
    }

    // ---- Recording unit --------------------------------------------------
}

/// Whether a window's marks are these already (a new model would redraw).
fn same_marks(model: &ModelRc<f32>, marks: &[f32]) -> bool {
    model.row_count() == marks.len() && model.iter().zip(marks).all(|(a, b)| (a - b).abs() < 1e-4)
}

/// The board file's time (None: no file yet).
fn carts_stamp() -> Option<std::time::SystemTime> {
    std::fs::metadata(Board::path()?).and_then(|m| m.modified()).ok()
}

/// Make a model hold these rows, touching only the rows that differ.
fn set_rows<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: impl Iterator<Item = T>) {
    let mut count = 0;
    for (i, row) in rows.enumerate() {
        count = i + 1;
        match model.row_data(i) {
            Some(old) if old == row => {}
            Some(_) => model.set_row_data(i, row),
            None => model.push(row),
        }
    }
    while model.row_count() > count {
        model.remove(model.row_count() - 1);
    }
}

fn show_meter_look(style: &crate::ui::MeterStyle<'_>, look: crate::meter_style::Look) {
    use crate::meter_style::Look;
    style.set_banded(look == Look::Banded);
    style.set_led(look == Look::Led);
    style.set_patch_style(look == Look::Patch);
}

#[cfg(test)]
mod cart_tests {
    use super::*;

    fn sound() -> Pad {
        Pad { label: "Sponsor".into(), file: "/ads/sponsor.wav".into(), seconds: 30.0, ..Pad::default() }
    }

    #[test]
    fn a_pad_at_rest_shows_its_length_and_a_playing_one_counts_down() {
        let row = pad_row(4, &sound(), None, None);
        assert_eq!((row.key.as_str(), row.label.as_str(), row.time.as_str()), ("5", "Sponsor", "0:30"));
        assert!(row.assigned && !row.playing && row.progress == 0.0);
        let marked = Pad { hold: true, duck: true, colour: 3, ..sound() };
        let row = pad_row(4, &marked, None, None);
        assert!(row.hold && row.duck && row.colour == 3);
        let playing = Playing { bank: 0, pad: 4, position: 12.0, duration: 30.0, ..Playing::default() };
        let row = pad_row(4, &sound(), Some(&playing), None);
        assert!(row.playing && !row.fading);
        assert_eq!(row.time.as_str(), "-0:18");
        assert!((row.progress - 0.4).abs() < 1e-6);
        // A loop has no end to count to: it counts up, and draws no bar.
        let looped = Playing { looped: true, position: 75.0, duration: 30.0, ..playing.clone() };
        assert_eq!(pad_row(4, &sound(), Some(&looped), None).time.as_str(), "1:15");
        // A free pad: only its key.
        let free = pad_row(11, &Pad::default(), None, None);
        assert_eq!((free.key.as_str(), free.label.as_str(), free.time.as_str(), free.assigned), ("=", "", "", false));
    }

    #[test]
    fn a_press_shows_before_the_voices_answer() {
        let playing = Playing { position: 12.0, duration: 30.0, ..Playing::default() };
        let started = pad_row(0, &sound(), None, Some(Press::Start));
        assert!(started.playing && started.time.as_str() == "0:30", "lit at once, its length until it reports");
        let cut = pad_row(0, &sound(), Some(&playing), Some(Press::Cut));
        assert!(!cut.playing && !cut.fading && cut.progress == 0.0);
        let fading = pad_row(0, &sound(), Some(&playing), Some(Press::Fade));
        assert!(fading.playing && fading.fading);
        // A fade pressed on a pad that already ended shows nothing.
        assert!(!pad_row(0, &sound(), None, Some(Press::Fade)).playing);
    }

    #[test]
    fn rows_change_in_place() {
        let model = VecModel::from(vec![1, 2, 3, 4]);
        set_rows(&model, [1, 5, 3].into_iter());
        assert_eq!(model.iter().collect::<Vec<_>>(), [1, 5, 3]);
        set_rows(&model, [1, 5, 3, 9, 9].into_iter());
        assert_eq!(model.row_count(), 5);
    }

    #[test]
    fn the_shots_show_a_wall_in_use() {
        let (board, playing) = demo_carts();
        assert!(board.banks[0].pads.iter().filter(|p| p.assigned()).count() >= 8);
        assert!(board.banks[0].pads.iter().any(|p| !p.assigned()), "a free pad shows too");
        assert!(playing.iter().all(|p| board.pad(p.bank, p.pad).is_some_and(Pad::assigned)));
    }
}

#[cfg(test)]
mod desk_tests {
    use super::*;

    #[test]
    fn a_playing_bar_moves_on_between_readings_and_only_a_seek_pulls_it_back() {
        let t0 = Instant::now();
        let later = |ms| t0 + Duration::from_millis(ms);
        let g = Glide::read(None, t0, 10.0, 100.0, 1.0);
        assert!((g.seconds(later(500)) - 10.5).abs() < 1e-9);
        // The same reading again: the bar goes on from where it was.
        assert_eq!(Glide::read(Some(g), later(200), 10.0, 100.0, 1.0), g);
        // A reading a poll old does not pull it back.
        let g2 = Glide::read(Some(g), later(400), 10.25, 100.0, 1.0);
        assert!((g2.seconds(later(400)) - 10.4).abs() < 1e-9);
        // A seek back does, and so does one forward.
        assert_eq!(Glide::read(Some(g2), later(600), 2.0, 100.0, 1.0).seconds(later(600)), 2.0);
        assert_eq!(Glide::read(Some(g2), later(600), 50.0, 100.0, 1.0).seconds(later(600)), 50.0);
        // Speed counts, and the bar stops at the end.
        let fast = Glide::read(None, t0, 99.0, 100.0, 2.0);
        assert!((fast.seconds(later(250)) - 99.5).abs() < 1e-9);
        assert_eq!(fast.progress(later(5000)), 1.0);
        // Another file (another length) starts over.
        assert_eq!(Glide::read(Some(g), later(100), 10.0, 60.0, 1.0).seconds(later(100)), 10.0);
    }

    #[test]
    fn a_three_minute_file_redraws_a_few_times_a_second_not_every_frame() {
        let g = Glide::read(None, Instant::now(), 0.0, 180.0, 1.0);
        let t0 = g.at;
        let (mut shown, mut sets) = (0.0f32, 0);
        for frame in 0..60 {
            let p = g.progress(t0 + Duration::from_micros(frame * 16_667));
            if (p - shown).abs() >= TRANSPORT_STEP {
                shown = p;
                sets += 1;
            }
        }
        assert!((8..=12).contains(&sets), "{sets} sets a second");
    }

    #[test]
    fn the_speed_button_walks_the_usual_speeds_and_the_keys_step_quarters() {
        assert_eq!(next_speed(1.0), 1.25);
        assert_eq!(next_speed(2.0), 0.75);
        assert_eq!(next_speed(0.75), 1.0);
        assert_eq!(next_speed(1.1), 1.0, "an odd speed goes back to normal");
        assert_eq!(stepped_speed(1.0, 1), 1.25);
        assert_eq!(stepped_speed(1.0, -1), 0.75);
        assert_eq!(stepped_speed(2.0, 1), 2.0);
        assert_eq!(stepped_speed(0.5, -1), 0.5);
        assert_eq!(speed_label(1.0), "1x");
        assert_eq!(speed_label(1.25), "1.25x");
        assert_eq!(speed_label(1.5), "1.5x");
        assert_eq!(speed_label(0.75), "0.75x");
    }

    #[test]
    fn repeat_all_is_offered_only_with_a_queue() {
        assert_eq!(next_repeat(0, 1), 1);
        assert_eq!(next_repeat(1, 1), 0);
        assert_eq!(next_repeat(1, 5), 2);
        assert_eq!(next_repeat(2, 5), 0);
        let s = Snapshot { looping: true, repeat_all: true, ..Snapshot::default() };
        assert_eq!(repeat_mode(&s), 1, "this one wins: it is what is heard");
    }

    #[test]
    fn the_ab_loop_is_drawn_in_order_and_inside_the_file() {
        let mut s = Snapshot { duration: 100.0, ab_a: -1.0, ab_b: -1.0, ..Snapshot::default() };
        assert_eq!(ab_shown(&s), (0, 0.0, 0.0));
        s.ab_a = 60.0;
        assert_eq!(ab_shown(&s), (1, 0.6, 0.6));
        s.ab_b = 20.0;
        assert_eq!(ab_shown(&s), (2, 0.2, 0.6), "B set before A: still a span");
        s.duration = 0.0;
        assert_eq!(ab_shown(&s).0, 0, "a stream has no span to draw");
    }

    #[test]
    fn chapters_and_marks_are_one_list_in_time_order() {
        let s = Snapshot {
            chapters: vec![0.0, 1356.0],
            chapter_titles: vec!["Cold open".into(), "Break".into()],
            ..Snapshot::default()
        };
        let mine = [marks::Mark { at: 761.0, name: "Sponsor read".into() }, marks::Mark { at: 1862.0, name: String::new() }];
        let list = mark_list(&s, &mine);
        let shown: Vec<(&str, &str, bool, Option<usize>)> =
            list.iter().map(|(row, _, mark)| (row.time.as_str(), row.name.as_str(), row.chapter, *mark)).collect();
        assert_eq!(
            shown,
            [
                ("00:00", "Cold open", true, None),
                ("12:41", "Sponsor read", false, Some(0)),
                ("22:36", "Break", true, None),
                ("31:02", "", false, Some(1)),
            ]
        );
    }

    #[test]
    fn a_line_shows_what_is_known_of_its_source() {
        let mut history = History::default();
        history.opened("/pod/ep42.mp3");
        history.note("/pod/ep42.mp3", "Episode 42", 761.0, 2886.0);
        let probed = HashMap::from([("/music/Other.flac".to_owned(), 185.0), ("/music/Bad.flac".to_owned(), 0.0)]);
        assert_eq!(length_of("/pod/ep42.mp3", &history, &probed), Some(2886.0), "as it played");
        assert_eq!(length_of("/music/Other.flac", &history, &probed), Some(185.0), "as probed");
        assert_eq!(length_of("/music/Bad.flac", &history, &probed), None);
        assert_eq!(queue_detail(1, &[Some(60.0), Some(2886.0)]), "2 of 2 · 49:06");
        assert_eq!(queue_detail(0, &[Some(60.0), None]), "1 of 2", "no total until every length is known");
        assert_eq!(queue_detail(0, &[Some(60.0)]), "", "one source is not a queue");
        let known = queue_row("/pod/ep42.mp3", &history, Some(2886.0), true, false);
        assert_eq!((known.title.as_str(), known.time.as_str(), known.current), ("Episode 42", "48:06", true));
        let new = queue_row("/music/New Track.flac", &history, None, false, true);
        assert_eq!((new.title.as_str(), new.time.as_str(), new.resting), ("New Track", "", true));
        // Lately: where a long one was left.
        assert_eq!(recent_row(&history.entries[0]).time.as_str(), "12:41 / 48:06");
        let short = history::Entry { path: "/a/Jingle.wav".into(), duration: 9.0, ..Default::default() };
        let row = recent_row(&short);
        assert_eq!((row.title.as_str(), row.time.as_str()), ("Jingle", "00:09"));
    }
}
