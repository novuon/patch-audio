//! Patch's keyboard map: the one place every key binding is declared.
//!
//! Three layers, each a table below:
//!
//! * [`GLOBAL`]: the shared grammar every Patch window understands
//!   (resolved by [`resolve`], handled by the session host).
//! * [`LETTERS`]: one letter per Unit window. A letter the focused window
//!   does not use itself opens (or focuses) that window; Shift + the letter
//!   opens another window of a Unit that has several. Surface specs take
//!   their key from here ([`letter`]), so a letter cannot be given twice.
//! * [`CONTROLLER`] and [`surface`]: the keys of the controller and of each
//!   window. Window keys come first: in the Mixer `s` solos, it does not
//!   open the Sequencer.
//!
//! The in-window help (`?`), the footers and the keyboard reference in
//! `docs/user-manual.md` / `.html` and `docs/getting-started.md` are
//! generated from these tables; tests check every hint against the key
//! handler that implements it, and that no key is used twice in one place.
//!
//! This is only what a Patch window receives. It does not bind Super or
//! Right Alt, so Omarchy 4.5's global shortcuts stay Omarchy's: Super+Q
//! closes a window, Super+A selects all, Super+M minimizes, Super+`
//! toggles the Quake console, Super+Alt+L cycles the workspace layout
//! backwards, Super+Shift+Ctrl+arrows moves the window to another monitor,
//! and holding Right Alt dictates.

/// A command produced by the shared grammar, handled by the session host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Tab: the next open Patch window (all Units, in the controller's order).
    NextSurface,
    PrevSurface,
    Controller,
    /// Ctrl+Q: in the controller only, close every Patch window. The
    /// engine keeps playing; nothing else in Patch quits from a key.
    Quit,
    Surface(char),
    /// Shift + a surface mnemonic: open another instance of it.
    NewSurface(char),
    /// Zoom this Unit's windows: in (1), out (-1), back to 100 % (0).
    Zoom(i8),
}

/// Resolve a key press that the surface itself did not consume.
pub fn resolve(text: &str, ctrl: bool, shift: bool) -> Option<Command> {
    // Some backends deliver Ctrl+letter as the ASCII control character.
    let normalized;
    let text = match text.as_bytes() {
        [b @ 0x01..=0x1a] if ctrl && *b != b'\t' => {
            normalized = ((b + 0x60) as char).to_string();
            normalized.as_str()
        }
        _ => text,
    };
    match (text, ctrl) {
        ("\t", false) if shift => Some(Command::PrevSurface),
        ("\t", false) => Some(Command::NextSurface),
        // Shift+Tab arrives as Backtab on some keyboards.
        ("\u{0019}", false) => Some(Command::PrevSurface),
        ("k" | "K", true) => Some(Command::Controller),
        ("q" | "Q", true) => Some(Command::Quit),
        ("=" | "+", true) => Some(Command::Zoom(1)),
        ("-" | "_", true) => Some(Command::Zoom(-1)),
        ("0", true) => Some(Command::Zoom(0)),
        (t, false) => {
            let mut chars = t.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_lowercase() => Some(Command::Surface(c)),
                (Some(c), None) if c.is_ascii_uppercase() && shift => {
                    Some(Command::NewSurface(c.to_ascii_lowercase()))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// One line of help: the keys as shown, what they do, and the key events
/// that do it (checked against the handlers by tests).
///
/// Event names: a character as typed (`"x"`, `"C"`, `"["`), or `space`,
/// `enter`, `esc`, `tab`, `del`, `left`, `right`, `up`, `down`; `ctrl+` and
/// `shift+` prefix a modifier. `mouse:right` / `mouse:drag` are pointer
/// gestures, `cmd:…` an `omarchy-patch` subcommand (`omarchy-patch toggle perf`)
/// that a desktop binding runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hint {
    pub keys: &'static str,
    pub action: &'static str,
    pub events: &'static [&'static str],
}

const fn h(keys: &'static str, action: &'static str, events: &'static [&'static str]) -> Hint {
    Hint {
        keys,
        action,
        events,
    }
}

/// The shared grammar: what every Patch window does with a key it does not
/// use itself. Tab cycles every open Patch window (the Units in the
/// controller's order, several windows of one Unit together), not only the
/// windows of one Unit: most Units have one window.
pub const GLOBAL: &[Hint] = &[
    h("a–z", "open or focus that Unit's window", &[]),
    h("⇧ + letter", "another window of that Unit", &[]),
    h("tab ⇧tab", "next / previous open Patch window", &["tab", "shift+tab"]),
    h("^k", "controller (open · close)", &["ctrl+k"]),
    h("^= ^- ^0", "zoom in · out · 100 %", &["ctrl+=", "ctrl+-", "ctrl+0"]),
    h("?", "help for this window", &["?"]),
    h("esc", "close the help · the controller", &["esc"]),
];

/// The footer's shared hints, in display order.
pub const COMMON_HINTS: &[(&str, &str)] =
    &[("tab", "windows"), ("^k", "controller"), ("?", "help")];

/// The Unit letters: (letter, window id). One letter per window; every
/// letter is taken, so newer windows (Diagnostics, Club Booth, the
/// Visualiser's controls, the Performer, decks C and D) open from the
/// controller or their Unit instead.
pub const LETTERS: &[(char, &str)] = &[
    ('a', "deck-a"),
    ('b', "deck-b"),
    ('c', "broadcast"),
    ('d', "devices"),
    ('e', "inspector"),
    ('f', "tidal"),
    ('g', "graph"),
    ('h', "controllers"),
    ('i', "mic"),
    ('j', "dj"),
    ('k', "eq"),
    ('l', "library"),
    ('m', "mixer"),
    ('n', "analyzer"),
    ('o', "perf"),
    ('p', "presets"),
    ('q', "plugins"),
    ('r', "recorder"),
    ('s', "sequencer"),
    ('t', "pattern"),
    ('u', "tune"),
    ('v', "meters"),
    ('w', "visualiser"),
    ('x', "dj-mixer"),
    ('y', "player"),
    ('z', "network"),
];

const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// The letter that opens a window (by its id), from [`LETTERS`].
pub const fn letter(surface: &str) -> Option<char> {
    let mut i = 0;
    while i < LETTERS.len() {
        if same(LETTERS[i].1, surface) {
            return Some(LETTERS[i].0);
        }
        i += 1;
    }
    None
}

/// The controller (Ctrl+K). Its cursor moves with the arrows only, so every
/// letter opens its Unit, as in any other window, and closes the controller.
/// Quitting takes Ctrl+Q: it closes the windows, never the audio.
pub const CONTROLLER: &[Hint] = &[
    h("↑↓", "move", &["up", "down"]),
    h("enter space", "open · close · change", &["enter", "space"]),
    h("a–z", "open that Unit", &[]),
    h("1–4", "Units · Layouts · Setup · About", &["1", "4"]),
    h("tab ⇧tab", "next / previous tab", &["tab"]),
    h("^q", "close all windows (audio plays on)", &["ctrl+q"]),
    h("esc ^k", "close the controller", &["esc", "ctrl+k"]),
];

const DECK: &[Hint] = &[
    h("space", "play", &["space"]),
    h("c", "cue", &["c"]),
    h("1–8", "hot cue (⇧ clears)", &["1", "2", "3", "4", "5", "6", "7", "8"]),
    h("←→", "jump", &["left", "right"]),
    h("o", "loop", &["o"]),
    h("↑↓", "tempo", &["up", "down"]),
    h("s", "sync", &["s"]),
    h("k", "keylock", &["k"]),
    h("[ ]", "key", &["[", "]"]),
    h(", .", "nudge", &[",", "."]),
    h("x", "sample", &["x"]),
];

/// Each window's own keys (its id → hints), as its help and footer show
/// them. Windows not listed have no keys of their own.
pub const SURFACES: &[(&str, &[Hint])] = &[
    (
        "graph",
        &[
            h("drag ▸", "patch", &["mouse:drag"]),
            h("←↑↓→", "move", &["left", "up", "down", "right"]),
            h("enter", "inspect", &["enter"]),
            h("r", "through Patch", &["r"]),
        ],
    ),
    (
        "mixer",
        &[
            h("←→", "strip", &["left", "right"]),
            h("↑↓", "gain", &["up", "down"]),
            h("space", "mute", &["space"]),
            h("s", "solo", &["s"]),
            h("o", "output device", &["o"]),
        ],
    ),
    (
        "devices",
        &[h("↑↓", "device", &["up", "down"]), h("enter", "expand", &["enter"])],
    ),
    ("inspector", &[h("↑↓", "scroll", &["up", "down"])]),
    (
        "meters",
        &[
            h("x", "reset I · LRA · TP", &["x"]),
            h("o", "output / engine", &["o"]),
            h(", .", "sync ∓10 ms", &[",", "."]),
        ],
    ),
    (
        "analyzer",
        &[
            h("f", "bars fast / slow", &["f"]),
            h("x", "reset hold", &["x"]),
            h("z", "scope auto-gain", &["z"]),
            h("l", "measure latency", &["l"]),
        ],
    ),
    (
        "presets",
        &[h("↑↓", "preset", &["up", "down"]), h("enter", "apply", &["enter"])],
    ),
    ("deck-a", DECK),
    ("deck-b", DECK),
    (
        "dj-mixer",
        &[
            h("←→", "crossfader", &["left", "right"]),
            h("0", "centre", &["0"]),
            h("f", "fx", &["f"]),
            h("4", "two · four decks", &["4"]),
        ],
    ),
    ("perf", &[h("SUPER+ALT+P", "open · close (your Omarchy binding)", &["cmd:toggle"])]),
    (
        "tidal",
        &[
            h("^⏎", "run block", &["ctrl+enter"]),
            h("^⇧⏎", "run all", &["ctrl+shift+enter"]),
            h("^.", "hush", &["ctrl+."]),
            h("^␣", "play · stop", &["ctrl+space"]),
            h("^B", "backdrop", &["ctrl+b"]),
            h("^H", "help", &["ctrl+h"]),
        ],
    ),
    (
        "visualiser",
        &[
            h("space", "next scene", &["space"]),
            h("h", "hold this one", &["h"]),
            h("a", "Ambient on · off", &["a"]),
            h("x", "switch this scene off", &["x"]),
            h("m", "choreography · generative · hybrid", &["m"]),
            h("g", "next generative scene", &["g"]),
            h("d", "DJ · booth · off", &["d"]),
            h("l", "controls window", &["l"]),
            h("f", "fullscreen", &["f"]),
        ],
    ),
    (
        "eq",
        &[
            h("1–8", "band", &["1", "8"]),
            h("←→ ↑↓", "freq · gain", &["left", "right", "up", "down"]),
            h("[ ]", "Q", &["[", "]"]),
            h("f g Q", "set Hz · dB · Q", &["f", "g", "Q"]),
            h("t", "type", &["t"]),
            h("space", "band on/off", &["space"]),
            h("b", "bypass", &["b"]),
            h("- =", "input gain", &["-", "="]),
            h("s", "spectrum on · off", &["s"]),
            h("m", "next: master · apps · DJ booth · mics", &["m"]),
            h("p P", "preset", &["p", "P"]),
        ],
    ),
    (
        "dj",
        &[
            h("space", "pause/resume", &["space"]),
            h("n", "mix now", &["n"]),
            h("1–4", "observe · suggest · prepare · act", &["1", "2", "3", "4"]),
            h("a s d", "pick 1–3", &["a", "s", "d"]),
            h("x", "drop 1", &["x"]),
            h("enter", "approve", &["enter"]),
            h("esc", "cancel", &["esc"]),
            h("v", "overview · why", &["v"]),
        ],
    ),
    (
        "dj-mini",
        &[
            h("space", "pause · resume Auto Mix", &["space"]),
            h("←→", "crossfader", &["left", "right"]),
            h("0", "centre", &["0"]),
            h("esc", "close the picker", &["esc"]),
        ],
    ),
    (
        "library",
        &[
            h("/", "filter", &["/"]),
            h("↑↓", "move", &["up", "down"]),
            h("space", "pre-listen", &["space"]),
            h("1–5", "start · drop 1 · break · drop 2 · outro", &["1", "2", "3", "4", "5"]),
            h("s", "set prep", &["s"]),
            h("a", "load A", &["a"]),
            h("b", "load B", &["b"]),
            h("c d", "load C · D", &["c", "d"]),
            h("q", "queue", &["q"]),
            h("⇧c", "add to collection", &["C"]),
            h("del", "out of collection", &["del"]),
            h("← →", "previous · next part", &["left", "right"]),
            h("⇧← ⇧→", "8 bars back · on", &["shift+left", "shift+right"]),
            h("o", "plays in order · DJ decides", &["o"]),
            h("⇧↑ ⇧↓", "move (in order)", &["shift+up", "shift+down"]),
            h("h", "HUD: track, waveform, set prep fit", &["h"]),
            h("esc", "hide HUD · stop pre-listening", &["esc"]),
        ],
    ),
    (
        "pattern",
        &[
            h("ctrl+enter", "evaluate · next cycle", &["ctrl+enter"]),
            h("ctrl+shift+enter", "now", &["ctrl+shift+enter"]),
            h("ctrl+space", "play · stop", &["ctrl+space"]),
        ],
    ),
    (
        "sequencer",
        &[
            h("space", "play/pause", &["space"]),
            h("esc", "stop", &["esc"]),
            h("right-click", "select step", &["mouse:right"]),
        ],
    ),
    (
        "player",
        &[
            h("space", "play/pause", &["space"]),
            h("1 … =", "play a cart (again: stop it)", &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "-", "="]),
            h("[ ]", "cart bank", &["[", "]"]),
            h("f", "fade the carts out", &["f"]),
            h("esc", "stop the carts", &["esc"]),
            h("e", "edit the pads", &["e"]),
            h("right-click", "edit a pad", &["mouse:right"]),
            h("o", "open file", &["o"]),
            h("c", "cue", &["c"]),
            h("p", "PFL", &["p"]),
            h("←→", "seek", &["left", "right"]),
            h(", .", "back 10 s · on 30 s", &[",", "."]),
            h("n b", "next · back in the queue", &["n", "b"]),
            h("q", "carts, queue or marks", &["q"]),
            h("m", "mark this place", &["m"]),
            h("; '", "the mark before · after", &[";", "'"]),
            h("l", "repeat: this one, all, off", &["l"]),
            h("a", "A-B loop: A, B, clear", &["a"]),
            h("< >", "slower · faster", &["<", ">"]),
        ],
    ),
    (
        "recorder",
        &[
            h("↑↓", "source", &["up", "down"]),
            h("f", "format", &["f"]),
            h("enter", "record / stop", &["enter"]),
            h("space", "pause / resume", &["space"]),
            h("t", "takes", &["t"]),
        ],
    ),
    ("performer", &[h("f", "fullscreen", &["f"])]),
    (
        "network",
        &[
            h("↑↓", "announced stream", &["up", "down"]),
            h("enter", "listen / stop", &["enter"]),
        ],
    ),
    (
        "broadcast",
        &[
            h("↑↓", "source", &["up", "down"]),
            h("enter", "go live / stop", &["enter"]),
            h("esc", "stop", &["esc"]),
        ],
    ),
    ("controller", CONTROLLER),
];

/// A window's own keys (by its base id: `meters#2` → `meters`).
pub fn surface(id: &str) -> &'static [Hint] {
    let base = crate::instance::base(id);
    SURFACES
        .iter()
        .find(|(s, _)| *s == base)
        .map_or(&[], |(_, hints)| *hints)
}

/// (keys, action) pairs, as the footer and the help show them.
pub fn pairs(hints: &[Hint]) -> Vec<(&'static str, &'static str)> {
    hints.iter().map(|h| (h.keys, h.action)).collect()
}

/// The keyboard reference for the docs, as Markdown, from these tables.
/// `titles` names each window id (`"deck-a"` → `"Deck A"`); `full` adds
/// the controller's and every window's own keys.
pub fn markdown(titles: &dyn Fn(&str) -> String, full: bool) -> String {
    let mut out = String::new();
    let row = |out: &mut String, k: &str, a: &str| {
        out.push_str(&format!("| {} | {} |\n", md_cell(k), md_cell(a)));
    };
    out.push_str("**Everywhere** (keys a window does not use itself)\n\n| Key | Does |\n|---|---|\n");
    for h in GLOBAL {
        row(&mut out, h.keys, h.action);
    }
    out.push_str("\n**Unit letters**\n\n| Key | Opens |\n|---|---|\n");
    for (c, id) in LETTERS {
        row(&mut out, &c.to_string(), &titles(id));
    }
    if !full {
        return out;
    }
    out.push_str("\n**Controller** (Ctrl+K)\n\n| Key | Does |\n|---|---|\n");
    for h in CONTROLLER {
        row(&mut out, h.keys, h.action);
    }
    for (id, hints) in SURFACES {
        if *id == "controller" || *id == "deck-b" {
            continue;
        }
        let name = if *id == "deck-a" { "Deck A and Deck B".to_string() } else { titles(id) };
        out.push_str(&format!("\n**{name}**\n\n| Key | Does |\n|---|---|\n"));
        for h in *hints {
            row(&mut out, h.keys, h.action);
        }
    }
    out
}

/// The same reference as HTML, for `docs/user-manual.html`.
pub fn html(titles: &dyn Fn(&str) -> String) -> String {
    let mut out = String::new();
    let table = |out: &mut String, title: &str, head: &str, rows: &[(String, String)]| {
        out.push_str(&format!(
            "<p><strong>{}</strong></p>\n<table>\n<thead>\n<tr>\n<th>Key</th>\n<th>{head}</th>\n</tr>\n</thead>\n<tbody>\n",
            html_text(title)
        ));
        for (k, a) in rows {
            out.push_str(&format!(
                "<tr>\n<td>{}</td>\n<td>{}</td>\n</tr>\n",
                html_text(k),
                html_text(a)
            ));
        }
        out.push_str("</tbody>\n</table>\n");
    };
    let rows = |hints: &[Hint]| -> Vec<(String, String)> {
        hints.iter().map(|h| (h.keys.to_string(), h.action.to_string())).collect()
    };
    table(&mut out, "Everywhere (keys a window does not use itself)", "Does", &rows(GLOBAL));
    let letters: Vec<_> = LETTERS.iter().map(|(c, id)| (c.to_string(), titles(id))).collect();
    table(&mut out, "Unit letters", "Opens", &letters);
    table(&mut out, "Controller (Ctrl+K)", "Does", &rows(CONTROLLER));
    for (id, hints) in SURFACES {
        if *id == "controller" || *id == "deck-b" {
            continue;
        }
        let name = if *id == "deck-a" { "Deck A and Deck B".to_string() } else { titles(id) };
        table(&mut out, &name, "Does", &rows(hints));
    }
    out
}

fn md_cell(s: &str) -> String {
    s.replace('|', "\\|")
}

fn html_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar() {
        assert_eq!(resolve("\t", false, false), Some(Command::NextSurface));
        assert_eq!(resolve("\t", false, true), Some(Command::PrevSurface));
        assert_eq!(resolve("k", true, false), Some(Command::Controller));
        assert_eq!(resolve("\u{b}", true, false), Some(Command::Controller));
        assert_eq!(resolve("q", true, false), Some(Command::Quit));
        assert_eq!(resolve("\u{11}", true, false), Some(Command::Quit));
        assert_eq!(resolve("p", true, false), None, "no palette: Ctrl+K is the controller");
        assert_eq!(resolve("\u{10}", true, false), None);
        assert_eq!(resolve("g", false, false), Some(Command::Surface('g')));
        assert_eq!(resolve("q", false, false), Some(Command::Surface('q')));
        assert_eq!(resolve("V", false, true), Some(Command::NewSurface('v')));
        assert_eq!(resolve("V", false, false), None);
        assert_eq!(resolve("V", true, true), None);
        assert_eq!(resolve("=", true, false), Some(Command::Zoom(1)));
        assert_eq!(resolve("+", true, true), Some(Command::Zoom(1)));
        assert_eq!(resolve("-", true, false), Some(Command::Zoom(-1)));
        assert_eq!(resolve("0", true, false), Some(Command::Zoom(0)));
        assert_eq!(resolve("-", false, false), None, "a bare minus stays the surface's");
    }

    #[test]
    fn every_letter_opens_one_window() {
        let mut seen = std::collections::BTreeMap::new();
        for (c, id) in LETTERS {
            assert!(c.is_ascii_lowercase(), "{c}");
            assert!(seen.insert(*c, *id).is_none(), "letter {c} given twice");
        }
        let mut ids: Vec<_> = LETTERS.iter().map(|(_, id)| *id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), LETTERS.len(), "a window with two letters");
        assert_eq!(letter("player"), Some('y'));
        assert_eq!(letter("diagnostics"), None, "Y is the Player's");
        assert_eq!(letter("dj"), Some('j'));
        assert_eq!(letter("nothing"), None);
    }

    /// The key events a set of hints uses, as one context sees them.
    fn events(hints: &[Hint]) -> Vec<&'static str> {
        hints.iter().flat_map(|h| h.events.iter().copied()).collect()
    }

    #[test]
    fn no_key_is_used_twice_in_one_context() {
        let contexts = SURFACES
            .iter()
            .map(|(id, hints)| (*id, *hints))
            .chain([("everywhere", GLOBAL)]);
        for (id, hints) in contexts {
            let mut seen = std::collections::BTreeSet::new();
            for e in events(hints) {
                assert!(seen.insert(e), "{id}: {e:?} is bound twice");
            }
            let mut shown = std::collections::BTreeSet::new();
            for h in hints {
                assert!(shown.insert(h.keys), "{id}: {:?} listed twice", h.keys);
            }
        }
    }

    #[test]
    fn the_controller_leaves_every_letter_to_the_units() {
        for e in events(CONTROLLER) {
            assert!(
                !(e.len() == 1 && e.chars().all(|c| c.is_ascii_alphabetic())),
                "the controller takes {e:?}, so that Unit can't be opened by letter"
            );
        }
        for c in 'a'..='z' {
            assert_eq!(resolve(&c.to_string(), false, false), Some(Command::Surface(c)));
        }
        // Quit is a chord: a stray letter never closes the windows.
        assert_eq!(resolve("q", true, false), Some(Command::Quit));
        assert!(events(CONTROLLER).contains(&"ctrl+q"));
    }

    #[test]
    fn global_hints_are_the_grammar() {
        let chord = |e: &str| -> Option<Command> {
            let (ctrl, rest) = match e.strip_prefix("ctrl+") {
                Some(r) => (true, r),
                None => (false, e),
            };
            let (shift, rest) = match rest.strip_prefix("shift+") {
                Some(r) => (true, r),
                None => (false, rest),
            };
            let text = if rest == "tab" { "\t" } else { rest };
            resolve(text, ctrl, shift)
        };
        for h in GLOBAL {
            for e in h.events {
                if matches!(*e, "?" | "esc") {
                    continue; // the window frame's own (SurfaceKeys)
                }
                assert!(chord(e).is_some(), "{e:?} ({}) is not in the grammar", h.action);
            }
        }
        for (k, _) in COMMON_HINTS {
            assert!(GLOBAL.iter().any(|h| h.keys.split(' ').any(|x| x == *k)), "{k}");
        }
    }

    #[test]
    fn deck_hints_are_the_deck_keymap() {
        let map = crate::input::Keymap::default_dj();
        for deck in ["deck-a", "deck-b"] {
            for h in surface(deck) {
                for e in h.events {
                    let (shift, text) = match e.strip_prefix("shift+") {
                        Some(t) => (true, t),
                        None => (false, *e),
                    };
                    let ev = crate::input::InputEvent::Key {
                        text: text.into(),
                        shift,
                        ctrl: false,
                        alt: false,
                        down: true,
                    };
                    assert!(map.resolve(deck, &ev).is_some(), "{deck}: {e:?} ({}) does nothing", h.action);
                }
            }
        }
    }

    #[test]
    fn the_docs_reference_is_generated_from_these_tables() {
        let t = |id: &str| id.to_string();
        let md = markdown(&t, true);
        assert!(!markdown(&t, false).contains("Controller"));
        assert!(md.contains("| y | player |"));
        assert!(md.contains("| ⇧c | add to collection |"));
        assert!(!md.contains("palette"));
        let html = html(&t);
        assert!(html.contains("<td>1–8</td>"));
    }
}
