//! The Tidal Unit's window: TidalCycles, native to Patch. It talks only to
//! the engine's `tidal` op: Ctrl+Enter sends the block under the cursor
//! (`eval`: only the channels it names change, as in Tidal), status and
//! the next events are polled while it is shown. The code you write is the
//! window's own (saved beside Patch's state, comments and all); what plays
//! is the engine's, and keeps playing with every window closed.
//!
//! With the backdrop on, the Visualiser's picture is drawn under the code
//! (its own director, fed by the music: the decks when they play, else the
//! Tidal session's tempo and the output's level).

use super::*;
use patch_pattern::tidal::editor::{self, SourceMap};
use patch_pattern::tidal::{self, help};
use serde_json::{Value, json};

const WELCOME: &str = "-- Tidal in Patch: TidalCycles, native. No Haskell, no SuperCollider.
-- Put the cursor in a block and press Ctrl+Enter to run it. Words light up
-- as they play; the gutter marks ▌ lines you ran, ▸ lines hitting now, ● errors.
-- Ctrl+Shift+Enter runs all · Ctrl+. hushes · Tab completes · Ctrl+/ comments
-- Ctrl+B puts the Visualiser under the code · Start has whole tracks.

setcps (124/60/4)

d1 $ s \"bd*4\"

d2 $ n \"<0 0 3 5>\" # s \"superreese\" # octave 2 # legato 0.9

d3 $ s \"~ cp ~ cp\" # room 0.25

d4 $ s \"~ hh ~ hh ~ hh ~ hh\" # hcutoff 6000
";

fn editor_path() -> std::path::PathBuf {
    patch_engine::session::state_dir().join("tidal-editor.tidal")
}

fn ui_path() -> std::path::PathBuf {
    patch_engine::session::state_dir().join("tidal-ui.json")
}

/// An event the window shows: its channel, when it sounds (cycles) and the
/// words it came from (`[source, from, to]`).
#[derive(Clone, Default)]
struct Ev {
    layer: usize,
    begin: f64,
    end: f64,
    locs: Vec<[u32; 3]>,
}

/// Snapshots the editor's undo walks through: (code, cursor).
const UNDO_DEPTH: usize = 400;

#[derive(Clone, Default)]
struct Chan {
    name: String,
    code: String,
    muted: bool,
    soloed: bool,
    silenced: bool,
}

#[derive(Default)]
pub(super) struct View {
    presence_at: Cell<Option<Instant>>,
    polls: crate::poll::Poller,
    /// The code as edited (every window shows the same).
    code: RefCell<String>,
    loaded: Cell<bool>,
    /// Edited and not yet saved: saved after a pause in typing.
    edited_at: Cell<Option<Instant>>,
    lines: Cell<usize>,
    playing: Cell<bool>,
    /// The cycle at the last status, when it came, and the tempo.
    cycle: Cell<f64>,
    cycle_at: Cell<Option<Instant>>,
    bpm: Cell<f64>,
    channels: RefCell<Vec<Chan>>,
    /// The channel rows every window shows: one model, changed in place.
    /// A new model each frame (the lights animate) rebuilt the rows under
    /// the pointer, and a click on M, S or ✕ was lost between press and
    /// release.
    rows: Rc<VecModel<TidalChannel>>,
    /// What each channel plays (from the engine's survey).
    sounds: RefCell<HashMap<String, String>>,
    /// Where each channel is written in the editor: name → 0-based line.
    /// The engine knows compiled code, never source lines; this is the link
    /// that lets the gutter show which lines are actually playing.
    channel_lines: RefCell<HashMap<String, usize>>,
    /// The last code the engine reported per channel. A change here means
    /// an eval happened elsewhere (the CLI, another window) and the editor
    /// should flash the lines it touched.
    channel_code: RefCell<HashMap<String, String>>,
    /// Lines of channels that just hit: the editor's live playhead.
    live_lines: RefCell<Vec<usize>>,
    /// Set when this window sent an eval, so a status reply does not flash
    /// the same lines twice.
    last_local_eval: Cell<Option<Instant>>,
    /// Events around now (what played this cycle and what comes next).
    onsets: RefCell<Vec<Ev>>,
    /// The code the engine keeps, which events' words count in: each
    /// channel's expression, then each let's (`Session::sources`), with the
    /// name it is written under.
    sources: RefCell<Vec<(String, String)>>,
    /// Where each source is written in the editor (None: not as it plays).
    /// Cleared when the code or the sources change.
    maps: RefCell<Option<Vec<Option<SourceMap>>>>,
    /// The words lit now: (line, column, length).
    active: RefCell<Vec<(usize, usize, usize)>>,
    /// The cursor, as last reported.
    cursor: Cell<usize>,
    undo: RefCell<Vec<(String, usize)>>,
    redo: RefCell<Vec<(String, usize)>>,
    /// When typing last made an undo step (typing within a second is one).
    typed_at: Cell<Option<Instant>>,
    last_events: Cell<Option<Instant>>,
    /// The block sent last: its first and last line.
    sent: Cell<Option<(usize, usize)>>,
    flash: Cell<Option<((usize, usize), Instant)>>,
    error_lines: RefCell<Vec<usize>>,
    backdrop: Cell<i32>,
    query: RefCell<String>,
    shown: Cell<bool>,
}

fn fmt_cycle(c: f64) -> String {
    if !c.is_finite() || c < 0.0 {
        return "–".into();
    }
    format!("{} · {}", c.floor() as i64, ((c.fract() * 4.0).floor() as i64) + 1)
}

/// How a statement refers to a channel: `1` for d1, `"bass"` for p "bass".
fn channel_ref(name: &str) -> String {
    match name.strip_prefix('d').and_then(|n| n.parse::<u32>().ok()) {
        Some(n) => n.to_string(),
        None => format!("{name:?}"),
    }
}

fn channel_head(name: &str) -> String {
    match name.strip_prefix('d').and_then(|n| n.parse::<u32>().ok()) {
        Some(_) => name.to_string(),
        None => format!("p {name:?}"),
    }
}

/// One line of a channel's code, for its row.
/// A channel's code as shown: in a transition, the new code and what is
/// happening (`xfade · s "sn"`), not the code that carries it.
fn shown_code(code: &str, now: f64) -> String {
    match tidal::transition(code) {
        Some(t) if !t.done(Some(now)) => {
            let what = match t.kind {
                "_xfade" => "xfade".to_string(),
                "_clutch" => "clutch".to_string(),
                "_once" => "once".to_string(),
                _ => format!("jump at {}", t.start),
            };
            format!("{what} · {}", t.new)
        }
        Some(t) => t.new.to_string(),
        None => code.to_string(),
    }
}

fn one_line(code: &str) -> String {
    let flat: String = code.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 64 {
        format!("{}…", flat.chars().take(63).collect::<String>())
    } else {
        flat
    }
}

/// The channel a source line assigns, if any: `d1 $ …` or `p "bass" $ …`.
fn channel_at_line(line: &str) -> Option<String> {
    let t = line.trim_start();
    if let Some(rest) = t.strip_prefix('d') {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            let tail = rest[digits.len()..].trim_start();
            if tail.starts_with('$') || tail.starts_with("silence") {
                return Some(format!("d{digits}"));
            }
        }
    }
    if let Some(rest) = t.strip_prefix('p')
        && let Some(body) = rest.trim_start().strip_prefix('"')
        && let Some(end) = body.find('"')
    {
        let name = &body[..end];
        if !name.trim().is_empty() {
            let tail = body[end + 1..].trim_start();
            if tail.starts_with('$') || tail.starts_with("silence") {
                return Some(name.trim().to_string());
            }
        }
    }
    None
}

/// Every channel's first line in the editor code.
fn channel_lines(code: &str) -> HashMap<String, usize> {
    let mut out = HashMap::new();
    for (i, line) in code.lines().enumerate() {
        if let Some(name) = channel_at_line(line) {
            out.entry(name).or_insert(i);
        }
    }
    out
}

/// The whole block a line belongs to: its assignment plus continuation
/// lines (a blank line or the next assignment ends it).
/// The editor line where what plays was written: the line that starts a
/// statement for `name` (`d1`, `p "bass"`, or `let x`) whose code, comments
/// and spacing aside, is `kept`. The same code written twice: the later.
/// A file holds many `d1 $ …`; the first one is rarely what plays.
fn line_of(code: &str, name: &str, kept: &str) -> Option<usize> {
    let squash = |t: &str| -> String { t.chars().filter(|c| !c.is_whitespace()).collect() };
    let want = squash(tidal::transition(kept).map_or(kept, |t| t.new));
    if want.is_empty() {
        return None;
    }
    let clean = tidal::lex::blank_comments(code);
    let lines: Vec<&str> = clean.lines().collect();
    let let_name = name.strip_prefix("let ").map(|n| n.split_whitespace().next().unwrap_or(""));
    let mut found = None;
    for (i, line) in lines.iter().enumerate() {
        let head = match let_name {
            Some(n) => line
                .trim_start()
                .strip_prefix("let ")
                .is_some_and(|r| r.split(|c: char| c.is_whitespace() || c == '=').next() == Some(n)),
            None => channel_at_line(line).as_deref() == Some(name),
        };
        if !head {
            continue;
        }
        // The statement: this line and the more indented ones under it.
        let indent = line.len() - line.trim_start().len();
        let mut text = line.to_string();
        for l in &lines[i + 1..] {
            if l.trim().is_empty() || l.len() - l.trim_start().len() <= indent {
                break;
            }
            text.push_str(l);
        }
        if squash(&text).ends_with(&want) {
            found = Some(i);
        }
    }
    found
}

fn block_lines(code: &str, line: usize) -> (usize, usize) {
    let lines: Vec<&str> = code.lines().collect();
    if line >= lines.len() {
        return (line, line);
    }
    let mut first = line;
    if channel_at_line(lines[line]).is_none() {
        // A continuation line: include its assignment above, but never a
        // second one (tracks often have assignments on consecutive lines).
        while first > 0 && !lines[first - 1].trim().is_empty() {
            let owner = channel_at_line(lines[first - 1]).is_some();
            first -= 1;
            if owner {
                break;
            }
        }
    }
    let mut last = line;
    while last + 1 < lines.len()
        && !lines[last].trim().is_empty()
        && !lines[last + 1].trim().is_empty()
        && channel_at_line(lines[last + 1]).is_none()
    {
        last += 1;
    }
    (first, last)
}

/// `text` with a mark on each of the given lines (0-based), blank lines
/// elsewhere: a gutter column that lines up with the code.
fn marks(lines: usize, on: impl Fn(usize) -> bool, mark: &str) -> String {
    (0..lines.max(1))
        .map(|i| if on(i) { mark } else { "" })
        .collect::<Vec<_>>()
        .join("\n")
}

impl AudioApp {
    pub(super) fn bind_tidal(&self, w: &PatchWindow) {
        self.tidal_load();
        let t = w.global::<TidalState>();
        let v = &self.tidal;
        t.set_code(v.code.borrow().as_str().into());
        t.set_layers(Self::tidal_layers(&v.code.borrow()));
        t.set_channels(v.rows.clone().into());
        if patch_host::shot::active()
            && let Ok(w) = std::env::var("PATCH_SHOT_TIDAL_ACTIVE")
        {
            let code = v.code.borrow();
            let ranges: Vec<(usize, usize)> = code.match_indices(w.as_str()).map(|(i, m)| (i, i + m.len())).collect();
            let rows: Vec<TidalMark> = editor::cells(&code, &ranges)
                .into_iter()
                .map(|(line, col, len)| TidalMark { line: line as i32, col: col as i32, len: len as i32 })
                .collect();
            t.set_active(Rc::new(VecModel::from(rows)).into());
            // And six channels, as a set that plays has.
            let ch = |i: i32, name: &str, sounds: &str, code: &str| TidalChannel {
                name: name.into(),
                code: code.into(),
                sounds: sounds.into(),
                line: 75 + i,
                muted: i == 3,
                soloed: false,
                silenced: false,
                hit: if i == 0 { 0.9 } else { 0.0 },
            };
            let chans = vec![
                ch(0, "d1", "bd", "s \"bd*4\""),
                ch(1, "d2", "superreese", "n \"<0 0 3 5>\" # s \"superreese\" # octave 2"),
                ch(2, "d3", "cp", "s \"~ cp ~ cp\" # room 0.25"),
                ch(3, "d4", "hh", "s \"~ hh ~ hh ~ hh ~ hh\" # hcutoff 6000"),
                ch(4, "d5", "superpad", "n \"<a'min7 f'maj7>\" # s \"superpad\""),
                ch(5, "d6", "superfm", "n (scale \"minPent\" \"0 ~ 4 7\") # s \"superfm\""),
            ];
            t.set_channels(Rc::new(VecModel::from(chans)).into());
        }
        // This window isn't among `windows` yet: its gutter directly.
        self.tidal_gutter_on(&t);
        self.tidal_gutter_each();
        t.set_backdrop(v.backdrop.get());
        t.set_topics(Rc::new(VecModel::from(
            help::TOPICS.iter().map(|t| SharedString::from(t.1)).collect::<Vec<_>>(),
        )).into());
        t.set_starters(Rc::new(VecModel::from(
            help::STARTERS.iter().map(|s| SharedString::from(s.0)).collect::<Vec<_>>(),
        )).into());
        t.set_help(self.tidal_help_model());
        let me = self.me.clone();
        t.on_run(move |anchor, cursor, all| {
            if let Some(a) = me.upgrade() {
                a.tidal_run(anchor.max(0) as usize, cursor.max(0) as usize, all);
            }
        });
        let me = self.me.clone();
        t.on_action(move |action| {
            if let Some(a) = me.upgrade() {
                a.tidal_action(&action);
            }
        });
        let me = self.me.clone();
        t.on_channel(move |name, what| {
            if let Some(a) = me.upgrade() {
                a.tidal_channel(&name, &what);
            }
        });
        let me = self.me.clone();
        t.on_insert(move |text, cursor| {
            if let Some(a) = me.upgrade() {
                a.tidal_insert(&text, cursor.max(0) as usize);
            }
        });
        let me = self.me.clone();
        t.on_starter(move |i| {
            if let Some(a) = me.upgrade() {
                a.tidal_starter(i.max(0) as usize);
            }
        });
        let me = self.me.clone();
        t.on_search(move |q| {
            if let Some(a) = me.upgrade() {
                *a.tidal.query.borrow_mut() = q.to_string();
                let m = a.tidal_help_model();
                a.tidal_each(|s| s.set_help(m.clone()));
            }
        });
        let me = self.me.clone();
        t.on_edited(move |code, cursor| {
            if let Some(a) = me.upgrade() {
                a.tidal_typed(code.as_str(), cursor.max(0) as usize);
            }
        });
        let me = self.me.clone();
        t.on_cursor_moved(move |cursor| {
            if let Some(a) = me.upgrade() {
                a.tidal.cursor.set(cursor.max(0) as usize);
                a.tidal_hint();
            }
        });
        let me = self.me.clone();
        t.on_edit(move |what, anchor, cursor| match me.upgrade() {
            Some(a) => a.tidal_edit(&what, anchor.max(0) as usize, cursor.max(0) as usize),
            None => -1,
        });
    }

    /// The code once per colour, for the editor's layers.
    fn tidal_layers(code: &str) -> ModelRc<SharedString> {
        let l = editor::layers(code);
        Rc::new(VecModel::from(l.into_iter().map(SharedString::from).collect::<Vec<_>>())).into()
    }

    /// Code as typed: one undo step per burst of typing.
    fn tidal_typed(&self, code: &str, cursor: usize) {
        let v = &self.tidal;
        if *v.code.borrow() == code {
            return;
        }
        let burst = v.typed_at.get().is_some_and(|t| t.elapsed().as_secs_f64() < 1.0);
        // A new line ends a burst, so undo goes a line at a time.
        let newline = code.len() > v.code.borrow().len() && code[..cursor.min(code.len())].ends_with('\n');
        if !burst || newline {
            self.tidal_snapshot();
        }
        v.typed_at.set(Some(Instant::now()));
        v.cursor.set(cursor);
        self.tidal_set_code(code, false);
    }

    /// The code as it is now, onto the undo stack (a new edit follows).
    fn tidal_snapshot(&self) {
        let v = &self.tidal;
        let mut undo = v.undo.borrow_mut();
        let now = (v.code.borrow().clone(), v.cursor.get());
        if undo.last().is_none_or(|u| u.0 != now.0) {
            undo.push(now);
        }
        if undo.len() > UNDO_DEPTH {
            undo.remove(0);
        }
        v.redo.borrow_mut().clear();
    }

    /// An edit the editor makes (see `TidalState.edit`): the cursor after
    /// it, or -1 when the key should type as usual.
    fn tidal_edit(&self, what: &str, anchor: usize, cursor: usize) -> i32 {
        let v = &self.tidal;
        let code = v.code.borrow().clone();
        let edit = match what {
            "undo" | "redo" => {
                let (from, to) = if what == "undo" { (&v.undo, &v.redo) } else { (&v.redo, &v.undo) };
                let Some((text, at)) = from.borrow_mut().pop() else { return -1 };
                to.borrow_mut().push((code, cursor));
                v.typed_at.set(None);
                v.cursor.set(at);
                self.tidal_set_code(&text, true);
                return at as i32;
            }
            "newline" => Some(editor::newline(&code, anchor, cursor)),
            "tab" => Some(editor::tab(&code, anchor, cursor)),
            "comment" => Some(editor::toggle_comment(&code, anchor, cursor)),
            "backspace" => editor::backspace(&code, anchor, cursor),
            t => t.chars().next().and_then(|c| editor::pair(&code, anchor, cursor, c)),
        };
        let Some((text, at)) = edit else { return -1 };
        if text != code {
            v.cursor.set(cursor);
            self.tidal_snapshot();
            // Typing on after an edit starts a new step.
            v.typed_at.set(None);
            self.tidal_set_code(&text, true);
        }
        v.cursor.set(at);
        at as i32
    }

    /// The hint line: what the word at the cursor does, and Tab's choices.
    fn tidal_hint(&self) {
        let v = &self.tidal;
        let code = v.code.borrow();
        let cursor = v.cursor.get().min(code.len());
        let word = editor::word_at(&code, cursor).map(|(a, b)| code[a..b].to_string());
        let found = editor::completions(&code, cursor);
        let about = match &word {
            Some(w) => editor::hint(w),
            // Inside s "…": what the sound is (a folder, built in, missing).
            None => editor::sound_at(&code, cursor).and_then(|(a, b)| editor::sound_hint(&code[a..b])),
        }
        .or_else(|| found.first().and_then(|w| editor::hint(w).or_else(|| editor::sound_hint(w))));
        let mut text = about.unwrap_or_default();
        if !found.is_empty() {
            let list = found.iter().take(6).cloned().collect::<Vec<_>>().join(" · ");
            text = if text.is_empty() { format!("Tab ▸ {list}") } else { format!("Tab ▸ {list}     {text}") };
        }
        drop(code);
        self.tidal_each(|s| {
            if s.get_hint() != text.as_str() {
                s.set_hint(text.as_str().into());
            }
        });
    }

    /// The saved code and backdrop, read once.
    fn tidal_load(&self) {
        let v = &self.tidal;
        if v.loaded.replace(true) {
            return;
        }
        let code = std::fs::read_to_string(editor_path()).unwrap_or_else(|_| WELCOME.to_string());
        v.lines.set(code.lines().count().max(1) + usize::from(code.ends_with('\n')));
        *v.code.borrow_mut() = code.clone();
        *v.channel_lines.borrow_mut() = channel_lines(&code);
        let ui: Value = std::fs::read(ui_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        v.backdrop.set(ui["backdrop"].as_i64().unwrap_or(0).clamp(0, 2) as i32);
    }

    fn tidal_save(&self) {
        let v = &self.tidal;
        let path = editor_path();
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, v.code.borrow().as_bytes()).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
        v.edited_at.set(None);
    }

    fn tidal_save_ui(&self) {
        let _ = std::fs::write(ui_path(), json!({"backdrop": self.tidal.backdrop.get()}).to_string());
    }

    fn tidal_each(&self, f: impl Fn(&TidalState)) {
        for w in self.windows.borrow().iter().filter_map(|w| w.upgrade()) {
            f(&w.global::<TidalState>());
        }
    }

    fn tidal_message(&self, text: &str) {
        self.tidal_each(|s| s.set_message(text.into()));
    }

    fn tidal_send(&self, key: &'static str, req: Value) {
        if !self.tidal.polls.request(key, req) {
            self.tidal_message("Busy · try again");
        }
    }

    /// New code from an edit (or from Patch: `push` shows it in every window).
    fn tidal_set_code(&self, code: &str, push: bool) {
        let v = &self.tidal;
        let lines = code.lines().count().max(1) + usize::from(code.ends_with('\n'));
        *v.code.borrow_mut() = code.to_string();
        *v.channel_lines.borrow_mut() = channel_lines(code);
        *v.maps.borrow_mut() = None;
        v.edited_at.set(Some(Instant::now()));
        let changed_lines = lines != v.lines.replace(lines);
        let layers = Self::tidal_layers(code);
        self.tidal_each(|s| {
            if push || s.get_code() != code {
                s.set_code(code.into());
            }
            s.set_layers(layers.clone());
        });
        if changed_lines {
            v.error_lines.borrow_mut().clear();
            self.tidal_gutter_each();
        }
    }

    fn tidal_gutter_each(&self) {
        self.tidal_each(|s| self.tidal_gutter_on(s));
    }

    fn tidal_gutter_on(&self, s: &TidalState) {
        let v = &self.tidal;
        let n = v.lines.get();
        let gutter = (1..=n).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        let errs = v.error_lines.borrow().clone();
        let marks_text = marks(n, |i| errs.contains(&i), "● ");
        let live = v.live_lines.borrow().clone();
        let live_text = marks(n, |i| live.contains(&i), "▸");
        let flash = match v.flash.get() {
            Some(((a, b), _)) => marks(n, |i| (a..=b).contains(&i), "▌"),
            None => String::new(),
        };
        s.set_gutter(gutter.as_str().into());
        s.set_marks(marks_text.as_str().into());
        s.set_live(live_text.as_str().into());
        s.set_flash(flash.as_str().into());
    }

    fn tidal_help_model(&self) -> ModelRc<TidalHelp> {
        let q = self.tidal.query.borrow().clone();
        let topic = |t: &str| help::TOPICS.iter().find(|x| x.0 == t).map_or("", |x| x.1);
        let rows: Vec<TidalHelp> = help::search(&q)
            .into_iter()
            .map(|r| TidalHelp {
                topic: topic(r.0).into(),
                word: r.1.into(),
                usage: r.2.into(),
                doc: r.3.into(),
                example: r.4.into(),
            })
            .collect();
        Rc::new(VecModel::from(rows)).into()
    }

    fn tidal_eval(&self, block: &str, lines: (usize, usize)) {
        if block.trim().is_empty() {
            self.tidal_message("Nothing to run here: put the cursor in a block of code");
            return;
        }
        self.tidal.sent.set(Some(lines));
        // Sample folders added since: the colours and completions know them.
        let before = patch_pattern::samples::index();
        let after = patch_pattern::samples::refresh();
        if !std::sync::Arc::ptr_eq(&before, &after) {
            let layers = Self::tidal_layers(&self.tidal.code.borrow());
            self.tidal_each(|s| s.set_layers(layers.clone()));
        }
        self.tidal_send("tid-eval", json!({"op": "tidal", "action": "eval", "code": block, "origin": "user"}));
    }

    fn tidal_run(&self, anchor: usize, cursor: usize, all: bool) {
        let code = self.tidal.code.borrow().clone();
        if all {
            let last = code.lines().count().saturating_sub(1);
            self.tidal_eval(&code, (0, last));
            self.tidal_message("Running everything");
            return;
        }
        let (b, e, first, last) = tidal::block_at(&code, anchor, cursor);
        self.tidal_eval(&code[b..e], (first, last));
    }

    fn tidal_action(&self, action: &str) {
        let v = &self.tidal;
        match action {
            "hush" => {
                self.tidal_eval("hush", (usize::MAX, usize::MAX));
                self.tidal_message("Hush");
            }
            "play" | "stop" => self.tidal_send("tid-transport", json!({"op": "tidal", "action": action})),
            "backdrop" => {
                let next = (v.backdrop.get() + 1) % 3;
                v.backdrop.set(next);
                self.tidal_each(|s| s.set_backdrop(next));
                self.tidal_save_ui();
                if next == 0 {
                    self.tidal_visual.blank();
                }
                self.tidal_message(match next {
                    1 => "Backdrop: the Visualiser under the code, dimmed",
                    2 => "Backdrop: bright",
                    _ => "Backdrop off",
                });
            }
            "faster" | "slower" => {
                let bpm = if v.bpm.get() >= 30.0 { v.bpm.get().round() } else { 135.0 };
                let bpm = (bpm + if action == "faster" { 1.0 } else { -1.0 }).clamp(30.0, 300.0);
                self.tidal_send("tid-transport", json!({"op": "tidal", "action": "tempo", "bpm": bpm}));
            }
            "own" => {
                let bpm = if v.bpm.get() >= 30.0 { v.bpm.get().round() } else { 135.0 };
                self.tidal_send("tid-transport", json!({"op": "tidal", "action": "tempo", "bpm": bpm}));
            }
            "follow" => self.tidal_send("tid-transport", json!({"op": "tidal", "action": "tempo"})),
            _ => {}
        }
    }

    fn tidal_channel(&self, name: &str, what: &str) {
        let Some(c) = self.tidal.channels.borrow().iter().find(|c| c.name == name).cloned() else {
            return;
        };
        let r = channel_ref(name);
        let code = match what {
            "mute" if c.muted => format!("unmute {r}"),
            "mute" => format!("mute {r}"),
            "solo" if c.soloed => format!("unsolo {r}"),
            "solo" => format!("solo {r}"),
            "silence" => format!("{} silence", channel_head(name)),
            _ => return,
        };
        self.tidal_eval(&code, (usize::MAX, usize::MAX));
    }

    /// A help example as a new block after the one at the cursor.
    fn tidal_insert(&self, text: &str, cursor: usize) {
        let code = self.tidal.code.borrow().clone();
        let (_, end, _, last) = tidal::block_at(&code, cursor, cursor);
        let end = if code.trim().is_empty() { code.len() } else { end.max(cursor.min(code.len())) };
        let end = code.floor_char_boundary(end);
        let new = format!("{}\n\n{}\n{}", code[..end].trim_end_matches('\n'), text, &code[end..]);
        let new = new.trim_start_matches('\n').to_string();
        self.tidal_snapshot();
        self.tidal_set_code(&new, true);
        let line = if code.trim().is_empty() { 1 } else { last + 3 };
        self.tidal_message(&format!("Added at line {line}: put the cursor on it and press Ctrl+Enter"));
    }

    /// A starter session: added at the end (nothing of yours is lost) and
    /// run from a hush.
    fn tidal_starter(&self, i: usize) {
        let Some((name, starter)) = help::STARTERS.get(i) else { return };
        let code = self.tidal.code.borrow().clone();
        let at = code.lines().count();
        let new = format!("{}\n\n-- ── {name} ──\n\n{starter}\n", code.trim_end());
        self.tidal_snapshot();
        self.tidal_set_code(new.trim_start(), true);
        let lines = new.trim_start().lines().count().saturating_sub(1);
        self.tidal_eval(&format!("hush\n{starter}"), (at + 1, lines));
        self.tidal_message(&format!("{name}: added at the end and playing"));
    }

    fn tidal_status(&self, d: &Value) {
        let v = &self.tidal;
        let st = &d["status"];
        v.playing.set(st["playing"].as_bool().unwrap_or(false));
        v.cycle.set(st["cycle"].as_f64().unwrap_or(-1.0));
        v.cycle_at.set(Some(Instant::now()));
        if let Some(b) = st["bpm"].as_f64() {
            v.bpm.set(b);
        }
        let tempo = match d["tempo"]["mode"].as_str() {
            Some("own") => {
                let bpm = d["tempo"]["bpm"].as_f64().unwrap_or(v.bpm.get());
                v.bpm.set(bpm);
                format!("{bpm:.0} BPM · cps {:.4}", tidal::cps(bpm))
            }
            _ => format!("follows the lead deck · {:.1} BPM", v.bpm.get()),
        };
        let mut sounds = HashMap::new();
        for l in d["layers"].as_array().into_iter().flatten() {
            if let (Some(n), Some(t)) = (l["name"].as_str(), l["target"].as_str()) {
                sounds.insert(n.to_string(), t.to_string());
            }
        }
        *v.sounds.borrow_mut() = sounds;
        let chans: Vec<Chan> = d["channels"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| Chan {
                name: c["name"].as_str().unwrap_or("").into(),
                code: c["code"].as_str().unwrap_or("").into(),
                muted: c["muted"] == true,
                soloed: c["soloed"] == true,
                silenced: c["silenced"] == true,
            })
            .collect();
        *v.channels.borrow_mut() = chans.clone();
        let sources: Vec<(String, String)> = chans
            .iter()
            .map(|c| (c.name.clone(), c.code.clone()))
            .chain(d["lets"].as_array().into_iter().flatten().map(|l| {
                (format!("let {}", l["name"].as_str().unwrap_or("")), l["code"].as_str().unwrap_or("").to_string())
            }))
            .collect();
        if *v.sources.borrow() != sources {
            *v.sources.borrow_mut() = sources;
            *v.maps.borrow_mut() = None;
        }
        // An eval from the CLI (or another window) has no poller reply here.
        // Spot it by the channels it changed and flash their editor lines.
        let changed: Vec<String> = {
            let mut previous = v.channel_code.borrow_mut();
            let mut current = HashMap::new();
            let mut changed = Vec::new();
            for c in &chans {
                current.insert(c.name.clone(), c.code.clone());
                if previous.get(&c.name).is_some_and(|old| *old != c.code) {
                    changed.push(c.name.clone());
                }
            }
            *previous = current;
            changed
        };
        let local = v.last_local_eval.get().is_some_and(|t| t.elapsed().as_secs_f64() < 1.5);
        if !changed.is_empty() && !local {
            let code = v.code.borrow();
            let map = v.channel_lines.borrow();
            let ranges: Vec<(usize, usize)> = changed
                .iter()
                .filter_map(|name| map.get(name).map(|line| block_lines(&code, *line)))
                .collect();
            if let (Some(first), Some(last)) = (
                ranges.iter().map(|r| r.0).min(),
                ranges.iter().map(|r| r.1).max(),
            ) {
                v.flash.set(Some(((first, last), Instant::now())));
                self.tidal_gutter_each();
            }
        }
        let dl = &d["delivery"];
        let delivery = format!(
            "played {} · no sound for {}{}",
            dl["played"].as_u64().unwrap_or(0),
            dl["unplayable"].as_u64().unwrap_or(0),
            dl["last_unplayable"].as_str().map(|e| format!(" ({e})")).unwrap_or_default(),
        );
        let warnings: Vec<SharedString> = d["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|w| w.as_str().map(SharedString::from))
            .collect();
        let warnings = Rc::new(VecModel::from(warnings));
        let cycle = fmt_cycle(v.cycle.get());
        self.tidal_each(|s| {
            s.set_playing(v.playing.get());
            s.set_cycle(cycle.as_str().into());
            s.set_tempo(tempo.as_str().into());
            s.set_delivery(delivery.as_str().into());
            s.set_warnings(warnings.clone().into());
        });
        self.tidal_channels_each();
    }

    /// The channel rows, their lights following the events as they play.
    fn tidal_channels_each(&self) {
        let v = &self.tidal;
        let now = self.tidal_cycle_now();
        let spc = 240.0 / v.bpm.get().max(30.0);
        let onsets = v.onsets.borrow();
        let sounds = v.sounds.borrow();
        let lines = v.channel_lines.borrow();
        let mut live = Vec::new();
        let rows: Vec<TidalChannel> = v
            .channels
            .borrow()
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let last = onsets
                    .iter()
                    .filter(|e| e.layer == i && e.begin <= now)
                    .map(|e| e.begin)
                    .fold(f64::NEG_INFINITY, f64::max);
                let hit = if v.playing.get() && last.is_finite() {
                    (-((now - last) * spc) / 0.12).exp() as f32
                } else {
                    0.0
                };
                let s = sounds.get(&c.name).cloned().unwrap_or_default();
                let line = lines.get(&c.name).map(|l| *l as i32).unwrap_or(-1);
                if hit > 0.15 && line >= 0 {
                    live.push(line as usize);
                }
                TidalChannel {
                    name: c.name.as_str().into(),
                    code: one_line(&shown_code(&c.code, now)).into(),
                    sounds: s.trim_end_matches(" · muted").into(),
                    line,
                    muted: c.muted,
                    soloed: c.soloed,
                    silenced: c.silenced,
                    hit,
                }
            })
            .collect();
        *v.live_lines.borrow_mut() = live;
        drop((onsets, sounds, lines));
        self.tidal_gutter_each();
        super::booth_controls::update_rows(&v.rows, rows);
        self.tidal_active(now);
    }

    /// Light the words whose events sound now, where they are written.
    fn tidal_active(&self, now: f64) {
        let v = &self.tidal;
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        if v.playing.get() {
            self.tidal_maps();
            let maps = v.maps.borrow();
            let maps = maps.as_deref().unwrap_or(&[]);
            let chans = v.channels.borrow();
            for e in v.onsets.borrow().iter() {
                if !(e.begin <= now && now < e.end) {
                    continue;
                }
                if chans.get(e.layer).is_some_and(|c| c.muted || c.silenced) {
                    continue;
                }
                for &[src, a, b] in &e.locs {
                    if let Some(Some(m)) = maps.get(src as usize)
                        && let Some(r) = m.range(a as usize, b as usize)
                    {
                        ranges.push(r);
                    }
                }
            }
        }
        // Headless shots of the highlight (`PATCH_SHOT_TIDAL_ACTIVE=word`):
        // every place that word is written, boxed, to check the boxes sit
        // on the text.
        if patch_host::shot::active()
            && let Ok(w) = std::env::var("PATCH_SHOT_TIDAL_ACTIVE")
        {
            ranges = v.code.borrow().match_indices(w.as_str()).map(|(i, m)| (i, i + m.len())).collect();
        }
        let cells = editor::cells(&v.code.borrow(), &ranges);
        if *v.active.borrow() == cells {
            return;
        }
        let rows: Vec<TidalMark> = cells
            .iter()
            .map(|&(line, col, len)| TidalMark {
                line: line as i32,
                col: col as i32,
                len: len as i32,
            })
            .collect();
        *v.active.borrow_mut() = cells;
        let rows = Rc::new(VecModel::from(rows));
        self.tidal_each(|s| s.set_active(rows.clone().into()));
    }

    /// Find every source in the editor's code (once per change).
    fn tidal_maps(&self) {
        let v = &self.tidal;
        if v.maps.borrow().is_some() {
            return;
        }
        let code = v.code.borrow();
        let lines = v.channel_lines.borrow();
        // Byte offset of each line's start.
        let starts: Vec<usize> = std::iter::once(0).chain(code.match_indices('\n').map(|(i, _)| i + 1)).collect();
        let mut found_lines = vec![];
        let maps = v
            .sources
            .borrow()
            .iter()
            .map(|(name, kept)| {
                // Where it was written; else, as before, the first line
                // that names it.
                if let Some(l) = line_of(&code, name, kept) {
                    if !name.starts_with("let ") {
                        found_lines.push((name.clone(), l));
                    }
                    return SourceMap::find(kept, &code, starts.get(l).copied().unwrap_or(0));
                }
                let near = match name.strip_prefix("let ") {
                    Some(n) => code
                        .lines()
                        .position(|l| l.trim_start().strip_prefix("let ").is_some_and(|r| r.trim_start().starts_with(n)))
                        .and_then(|l| starts.get(l).copied()),
                    None => lines.get(name).and_then(|l| starts.get(*l).copied()),
                };
                SourceMap::find(kept, &code, near.unwrap_or(0))
            })
            .collect();
        *v.maps.borrow_mut() = Some(maps);
        drop((code, lines));
        // The channel list's lines and the live marks follow too.
        let mut cl = v.channel_lines.borrow_mut();
        for (name, l) in found_lines {
            cl.insert(name, l);
        }
    }

    /// The cycle now, carried on from the last status at the tempo.
    fn tidal_cycle_now(&self) -> f64 {
        let v = &self.tidal;
        let c = v.cycle.get();
        match v.cycle_at.get() {
            Some(at) if v.playing.get() && c >= 0.0 => {
                c + at.elapsed().as_secs_f64() * v.bpm.get().max(1.0) / 60.0 / tidal::CYCLE_BEATS as f64
            }
            _ => c,
        }
    }

    fn tidal_errors(&self, errors: &Value, first: Option<usize>) {
        let v = &self.tidal;
        let rows: Vec<TidalError> = errors
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|e| {
                        let line = e["line"].as_i64().unwrap_or(0);
                        // Lines count from the block's first: as in the editor.
                        let line = match (first, line) {
                            (Some(f), l) if l > 0 => l + f as i64,
                            (_, l) => l,
                        };
                        TidalError {
                            line: line as i32,
                            col: e["col"].as_i64().unwrap_or(0) as i32,
                            message: e["message"].as_str().unwrap_or("").into(),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        *v.error_lines.borrow_mut() = rows.iter().filter(|e| e.line > 0).map(|e| e.line as usize - 1).collect();
        let rows = Rc::new(VecModel::from(rows));
        self.tidal_each(|s| s.set_errors(rows.clone().into()));
        self.tidal_gutter_each();
    }

    /// The music the backdrop moves to: the decks' when they play, else the
    /// Tidal session's beat and the output's level and spectrum.
    pub(super) fn tidal_music(&self) -> patch_visual::director::Music {
        let decks = self.visual_music();
        if decks.playing || !self.tidal.playing.get() {
            return decks;
        }
        let m = self.metering.borrow();
        patch_visual::director::Music {
            title: "TIDAL".into(),
            playing: true,
            level: metering::lufs_pos(m.lufs_momentary()),
            beat: self.tidal_cycle_now() * tidal::CYCLE_BEATS as f64,
            bpm: self.tidal.bpm.get(),
            bands: m.spectrum_db().iter().map(|db| ((db + 60.0) / 48.0).clamp(0.0, 1.0)).collect(),
            kicking: true,
            ..Default::default()
        }
    }

    /// Whether the backdrop is on (the frame ticks it in Tidal windows).
    pub(super) fn tidal_backdrop(&self) -> bool {
        self.tidal.backdrop.get() > 0
    }

    /// Called from the UI tick: take replies, then poll while shown.
    pub(super) fn poll_tidal(&self) {
        if self.frozen {
            return;
        }
        let v = &self.tidal;
        for key in ["tid-eval", "tid-transport", "tid-status", "tid-events"] {
            let Some(reply) = v.polls.take(key) else { continue };
            match (key, reply.result) {
                ("tid-eval", Ok(d)) if d["ok"] == false => {
                    let n = d["errors"].as_array().map_or(0, Vec::len);
                    let first = v.sent.get().filter(|s| s.0 != usize::MAX).map(|s| s.0);
                    self.tidal_errors(&d["errors"], first);
                    self.tidal_message(&format!(
                        "{n} error{} · {}",
                        if n == 1 { "" } else { "s" },
                        if v.playing.get() { "what played plays on" } else { "nothing changed" }
                    ));
                }
                ("tid-eval", Ok(d)) => {
                    v.last_local_eval.set(Some(Instant::now()));
                    self.tidal_errors(&json!([]), None);
                    if let Some(lines) = v.sent.take().filter(|s| s.0 != usize::MAX) {
                        v.flash.set(Some((lines, Instant::now())));
                        self.tidal_gutter_each();
                    }
                    let touched: Vec<String> = d["touched"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|t| t.as_str().map(String::from))
                        .collect();
                    let warnings = d["warnings"].as_array().map_or(0, Vec::len);
                    self.tidal_message(&format!(
                        "{} in {} µs{}",
                        if touched.is_empty() { "Done".to_string() } else { format!("Running {}", touched.join(", ")) },
                        d["eval_us"],
                        if warnings > 0 { format!(" · {warnings} warning{}", if warnings == 1 { "" } else { "s" }) } else { String::new() }
                    ));
                    // The status follows at once (channels, tempo).
                    v.polls.request("tid-status", json!({"op": "tidal", "action": "status"}));
                }
                ("tid-events", Ok(d)) => {
                    let time = |t: &Value| t.as_str().and_then(patch_pattern::time::Time::parse).map(|t| t.to_f64());
                    let from = d["from"].as_f64();
                    let mut on = vec![];
                    for e in d["events"].as_array().into_iter().flatten() {
                        let (Some(l), Some(b), Some(end)) = (e["layer"].as_u64(), time(&e["whole"]["begin"]), time(&e["whole"]["end"])) else { continue };
                        let locs = e["locs"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|r| {
                                let r = r.as_array()?;
                                Some([r.first()?.as_u64()? as u32, r.get(1)?.as_u64()? as u32, r.get(2)?.as_u64()? as u32])
                            })
                            .collect();
                        on.push(Ev { layer: l as usize, begin: b, end, locs });
                    }
                    // Keep what started before this reply's span and may still
                    // sound (the lights decay, long notes stay lit).
                    let now = self.tidal_cycle_now();
                    let cut = from.unwrap_or_else(|| on.iter().map(|e| e.begin).fold(f64::INFINITY, f64::min));
                    let mut keep: Vec<Ev> = v
                        .onsets
                        .borrow()
                        .iter()
                        .filter(|e| e.begin < cut && (e.end > now - 0.05 || e.begin >= now - 1.0))
                        .cloned()
                        .collect();
                    keep.extend(on.into_iter().filter(|e| e.begin >= cut));
                    *v.onsets.borrow_mut() = keep;
                }
                (_, Ok(d)) => self.tidal_status(&d),
                (_, Err(e)) => self.tidal_message(if e.contains("unknown op") {
                    "This engine predates the Tidal Unit. Upgrade the running Patch engine to this build (omarchy-patch engine upgrade)."
                } else {
                    &e
                }),
            }
        }
        // Save the code after a second without typing.
        if v.edited_at.get().is_some_and(|t| t.elapsed().as_secs_f64() > 1.0) {
            self.tidal_save();
        }
        // The run lines fade.
        if let Some((_, at)) = v.flash.get() {
            let k = 1.0 - at.elapsed().as_secs_f32() / 1.2;
            if k <= 0.0 {
                v.flash.set(None);
                self.tidal_each(|s| s.set_flash_level(0.0));
                self.tidal_gutter_each();
            } else {
                self.tidal_each(|s| s.set_flash_level(k));
            }
        }
        let shown = self
            .windows
            .borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .any(|w| w.get_surface() == "tidal");
        v.polls.take("tid-presence");
        if !shown {
            if v.presence_at.replace(None).is_some() {
                v.polls.request("tid-presence", json!({"op":"module_presence", "module":"tidal", "owner":std::process::id(), "open":false}));
            }
            if v.shown.replace(false) {
                self.tidal_visual.blank();
            }
            return;
        }
        v.shown.set(true);
        if v.presence_at.get().is_none_or(|t| t.elapsed().as_secs() >= 1)
            && v.polls.request("tid-presence", json!({"op":"module_presence", "module":"tidal", "owner":std::process::id()}))
        {
            v.presence_at.set(Some(Instant::now()));
        }
        v.polls.request("tid-status", json!({"op": "tidal", "action": "status"}));
        let due = v.last_events.get().is_none_or(|t| t.elapsed().as_millis() >= 400);
        if due && v.playing.get() && v.cycle.get() >= 0.0 {
            let from = self.tidal_cycle_now();
            if v.polls.request("tid-events", json!({"op": "tidal", "action": "events", "from": from, "to": from + 1.5})) {
                v.last_events.set(Some(Instant::now()));
            }
        }
        if v.playing.get() {
            self.tidal_channels_each();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_line_up_with_the_code() {
        assert_eq!(marks(4, |i| i == 1 || i == 2, "▌"), "\n▌\n▌\n");
        assert_eq!(channel_ref("d3"), "3");
        assert_eq!(channel_ref("bass"), "\"bass\"");
        assert_eq!(channel_head("bass"), "p \"bass\"");
        assert_eq!(one_line("s \"bd\"\n  # gain 1"), "s \"bd\" # gain 1");
    }

    #[test]
    fn what_plays_is_found_where_it_was_written() {
        // As in a real buffer: d1 written many times; what plays is the
        // block run last, not the first d1.
        let code = "d1 $ s \"bd [sn cp]\"\n\nd1 $ s \"bd*4\"\nlet kick = s \"bd*4\"\n-- d1 $ s \"bd*8\"\nd1 $ s \"bd*4\" # shape 0.4\nd2 $ n \"0 3\"\n  # s \"superreese\"\n\nd1 $ s \"bd*4\"\n";
        assert_eq!(line_of(code, "d1", "s \"bd*4\""), Some(9), "the later of two");
        assert_eq!(line_of(code, "d1", "s \"bd*4\" # shape 0.4"), Some(5));
        assert_eq!(line_of(code, "d2", "n \"0 3\"\n  # s \"superreese\""), Some(6));
        assert_eq!(line_of(code, "d1", "s \"bd*8\""), None, "commented out");
        assert_eq!(line_of(code, "let kick", "s \"bd*4\""), Some(3));
        assert_eq!(line_of(code, "d1", "_xfade 2 4 (s \"bd\") $ s \"bd*4\" # shape 0.4"), Some(5), "a transition: its new code");
    }

    #[test]
    fn editor_lines_map_to_channels() {
        let code = "-- a comment\nsetcps (124/60/4)\n\nd1 $ s \"bd*4\"\n  # gain 0.9\n\np \"bass\" $ n \"0 3\"\n";
        let lines = channel_lines(code);
        assert_eq!(lines.get("d1"), Some(&3));
        assert_eq!(lines.get("bass"), Some(&6));
        assert_eq!(block_lines(code, 4), (3, 4));
        assert_eq!(block_lines(code, 3), (3, 4));
        assert!(channel_at_line("  d2 $ s \"cp\"").is_some());
        assert!(channel_at_line("d1 $ s \"bd\"").is_some());
        assert!(channel_at_line("-- d1 $ s \"bd\"").is_none());
    }

    #[test]
    fn the_welcome_code_runs() {
        let mut s = tidal::Session::default();
        s.apply(WELCOME).unwrap();
        assert_eq!(s.channels.len(), 4);
    }

    /// A Tidal window with long code, for the pointer tests.
    fn window(app: &Rc<AudioApp>) -> PatchWindow {
        let _ = patch_host::shot::install();
        let v = &app.tidal;
        // Loaded already: the user's own code and settings stay unread.
        v.loaded.set(true);
        // Longer than the window, shorter than the hundred lines that once
        // set the code area's height.
        let code: String = (1..=70).map(|i| format!("d1 $ s \"bd*{i}\"\n")).collect();
        *v.code.borrow_mut() = code;
        v.lines.set(71);
        let w = app.create("tidal");
        {
            use patch_host::session::SurfaceWindow;
            let spec = AudioApp::spec();
            w.apply_host(&patch_host::session::HostState::build(&**app, &spec, "tidal", &[], None));
        }
        let _ = patch_host::shot::Shooter { scale: 1.0 }.render(&w, 1600, 900);
        w.show().unwrap();
        w
    }

    fn click(w: &PatchWindow, x: f32, y: f32, between: impl FnOnce()) {
        use slint::platform::{PointerEventButton, WindowEvent};
        let position = slint::LogicalPosition::new(x, y);
        let button = PointerEventButton::Left;
        w.window().dispatch_event(WindowEvent::PointerMoved { position });
        w.window().dispatch_event(WindowEvent::PointerPressed { position, button });
        between();
        w.window().dispatch_event(WindowEvent::PointerReleased { position, button });
    }

    /// The mouse wheel scrolls the code, also once a click has put the
    /// cursor in it (keeping the cursor in view undid every turn).
    #[test]
    fn the_wheel_scrolls_the_code() {
        use slint::platform::WindowEvent;
        let app = AudioApp::new();
        let w = window(&app);
        let moved = Rc::new(Cell::new(false));
        {
            let moved = moved.clone();
            w.global::<TidalState>().on_cursor_moved(move |_| moved.set(true));
        }
        // A click well inside the code: the cursor goes there.
        let spot = (300..600).step_by(4).map(|y| y as f32).find(|&y| {
            moved.set(false);
            click(&w, 300.0, y, || {});
            moved.get()
        });
        let y = spot.expect("no code to click");
        let t = w.global::<TidalState>();
        assert_eq!(t.get_scrolled(), 0.0);
        w.window().dispatch_event(WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(300.0, y),
            delta_x: 0.0,
            delta_y: -600.0,
        });
        // A frame later (the cursor's place is updated as the view moves).
        let img = patch_host::shot::Shooter { scale: 1.0 }.render(&w, 1600, 900);
        if let Some(dir) = std::env::var_os("PATCH_TEST_SHOT") {
            img.save(std::path::Path::new(&dir).join("tidal-wheel.png")).unwrap();
        }
        assert!(t.get_scrolled() > 300.0, "the wheel didn't scroll: {}", t.get_scrolled());
    }

    /// M, S and ✕ take a click while the rows refresh (every frame as
    /// the lights move).
    #[test]
    fn a_channel_click_survives_the_lights() {
        let app = AudioApp::new();
        let v = &app.tidal;
        *v.channels.borrow_mut() = vec![Chan { name: "d1".into(), code: "s \"bd*4\"".into(), muted: false, soloed: false, silenced: false }];
        v.playing.set(true);
        v.bpm.set(120.0);
        let w = window(&app);
        app.tidal_channels_each();
        let got = Rc::new(RefCell::new(Vec::<String>::new()));
        {
            let got = got.clone();
            w.global::<TidalState>().on_channel(move |n, what| got.borrow_mut().push(format!("{n} {what}")));
        }
        let mut at = None;
        'scan: for y in (40..400).step_by(4) {
            for x in (900..1600).rev().step_by(4) {
                click(&w, x as f32, y as f32, || {});
                if got.borrow().iter().any(|g| g == "d1 solo") {
                    at = Some((x as f32, y as f32));
                    break 'scan;
                }
                got.borrow_mut().clear();
            }
        }
        let (x, y) = at.expect("no S on the channel row");
        got.borrow_mut().clear();
        click(&w, x, y, || app.tidal_channels_each());
        assert_eq!(got.borrow().as_slice(), ["d1 solo"], "the click was lost");
    }
}
