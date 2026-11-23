//! Every key a window's help shows (patch_core::keys) is checked against
//! the handler that implements it: the window's `key-pressed` in its
//! `.slint` file (each style's), the DJ keymap, a Rust key handler or the
//! shared grammar. A hint for a key nothing handles fails here, so the help
//! cannot promise a key again (the Library's C for collections loaded deck
//! C; the Analyzer's L was missing).

use patch_core::input::{InputEvent, Keymap};
use patch_core::keys::{self, Hint};
use std::path::Path;

/// Where one style of a window handles keys: (file, component); an empty
/// component means the whole (Rust) file.
type Style = &'static [(&'static str, &'static str)];

struct Window {
    id: &'static str,
    /// Every style must handle each key (Classic and Pro gear layouts).
    styles: &'static [Style],
    /// The DJ keymap context these windows forward keys to.
    keymap: Option<&'static str>,
    /// Keys the session host resolves (the controller's Ctrl+Q).
    grammar: bool,
}

const fn w(id: &'static str, styles: &'static [Style]) -> Window {
    Window {
        id,
        styles,
        keymap: None,
        grammar: false,
    }
}

const WINDOWS: &[Window] = &[
    w("graph", &[&[("ui/graph.slint", "GraphSurface"), ("ui/graph.slint", "NodeBox")]]),
    w(
        "mixer",
        &[&[("ui/mixer.slint", "MixerSurface")], &[("ui/mixer-classic.slint", "MixerClassic")]],
    ),
    w("devices", &[&[("ui/devices.slint", "DevicesSurface")]]),
    w("inspector", &[&[("ui/inspector.slint", "InspectorSurface")]]),
    w(
        "meters",
        &[&[("ui/meters.slint", "MetersSurface")], &[("ui/meters-classic.slint", "MetersClassic")]],
    ),
    w("analyzer", &[&[("ui/analyzer.slint", "AnalyzerSurface")]]),
    w("presets", &[&[("ui/presets.slint", "PresetsSurface")]]),
    Window {
        keymap: Some("deck-a"),
        ..w("deck-a", &[&[("ui/dj.slint", "DeckSurface")], &[("ui/dj-classic.slint", "DeckClassic")]])
    },
    Window {
        keymap: Some("deck-b"),
        ..w("deck-b", &[&[("ui/dj.slint", "DeckSurface")], &[("ui/dj-classic.slint", "DeckClassic")]])
    },
    Window {
        keymap: Some("dj-mixer"),
        ..w(
            "dj-mixer",
            &[&[("ui/dj.slint", "DjMixerSurface")], &[("ui/dj-classic.slint", "DjMixerClassic")]],
        )
    },
    w("perf", &[&[("../../app/src/main.rs", "")]]),
    w("tidal", &[&[("ui/tidal.slint", "CodeArea")]]),
    w("visualiser", &[&[("ui/visual.slint", "VisualSurface")]]),
    w(
        "eq",
        &[&[("ui/eq.slint", "EqSurface"), ("src/app/eq.rs", ""), ("src/eq_view.rs", "")]],
    ),
    w("dj", &[&[("ui/dj-workspace.slint", "DjWorkspaceSurface")]]),
    w("dj-mini", &[&[("ui/dj-mini.slint", "MiniBoothSurface")]]),
    w("library", &[&[("ui/dj.slint", "LibrarySurface")]]),
    w("pattern", &[&[("ui/pattern.slint", "CodeEditor")]]),
    w("sequencer", &[&[("ui/sequencer.slint", "SequencerSurface"), ("ui/sequencer.slint", "StepPad")]]),
    w("player", &[&[("ui/player.slint", "PlayerSurface"), ("ui/player.slint", "CartButton")]]),
    w("recorder", &[&[("ui/recorder.slint", "RecorderSurface")]]),
    w("performer", &[&[("ui/broadcast-dj.slint", "BroadcastDjSurface")]]),
    w("broadcast", &[&[("ui/broadcast.slint", "BroadcastSurface")]]),
    w("network", &[&[("ui/network.slint", "NetworkSurface")]]),
    Window {
        grammar: true,
        ..w(
            "controller",
            &[&[("../patch-ui/ui/controller.slint", "ControllerView"), ("../patch-ui/ui/frame.slint", "SurfaceKeys")]],
        )
    },
];

fn read(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// A component's source: from its declaration to the next top-level one.
fn component(src: &str, name: &str) -> String {
    let start = [format!("export component {name} "), format!("component {name} ")]
        .iter()
        .find_map(|d| {
            src.match_indices(d.as_str())
                .find(|(i, _)| *i == 0 || src[..*i].ends_with('\n') || src[..*i].ends_with("export "))
                .map(|(i, _)| i)
        })
        .unwrap_or_else(|| panic!("no component {name}"));
    let rest = &src[start + 1..];
    let end = ["\ncomponent ", "\nexport component "]
        .iter()
        .filter_map(|d| rest.find(d))
        .min()
        .map_or(src.len(), |e| start + 1 + e);
    src[start..end].to_string()
}

/// The bodies of a component's key handlers (and its pointer handlers, for
/// mouse hints): what the window really does with a key.
fn handlers(component: &str) -> String {
    let mut out = String::new();
    for marker in ["key-pressed(e) =>", "pointer-event(e) =>", "moved =>"] {
        for (i, _) in component.match_indices(marker) {
            let Some(open) = component[i..].find('{').map(|o| i + o) else { continue };
            let mut depth = 0;
            for (j, c) in component[open..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            out.push_str(&component[open..open + j + 1]);
                            out.push('\n');
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    out
}

/// What a key event looks like in handler source (Slint or Rust).
fn needles(key: &str) -> Vec<String> {
    match key {
        "space" => vec![r#"" ""#.into()],
        "enter" => vec!["Key.Return".into(), r#""\n""#.into()],
        "esc" => vec!["Key.Escape".into(), r#""\u{1b}""#.into()],
        "del" => vec!["Key.Delete".into()],
        "tab" => vec!["Key.Tab".into()],
        "left" => vec!["Key.LeftArrow".into(), r#""\u{f702}""#.into()],
        "right" => vec!["Key.RightArrow".into(), r#""\u{f703}""#.into()],
        "up" => vec!["Key.UpArrow".into(), r#""\u{f700}""#.into()],
        "down" => vec!["Key.DownArrow".into(), r#""\u{f701}""#.into()],
        "mouse:right" => vec!["PointerEventButton.right".into()],
        "mouse:drag" => vec!["self.pressed".into()],
        // `cmd:toggle`: a subcommand the CLI matches (`["toggle", s]`).
        k if k.starts_with("cmd:") => vec![format!("[\"{}\", ", &k["cmd:".len()..])],
        k => vec![format!("\"{k}\"")],
    }
}

/// Whether a source (handler bodies) handles a key event.
fn handles(body: &str, event: &str) -> bool {
    let mut key = event;
    if let Some(k) = key.strip_prefix("ctrl+") {
        if !body.contains("e.modifiers.control") {
            return false;
        }
        key = k;
    }
    if let Some(k) = key.strip_prefix("shift+") {
        if !body.contains("e.modifiers.shift") {
            return false;
        }
        key = k;
    }
    needles(key).iter().any(|n| body.contains(n.as_str()))
        // A Ctrl+letter chord: Slint passes the letter (any case).
        || (event.starts_with("ctrl+") && key.len() == 1 && body.contains(&format!("\"{}\"", key.to_uppercase())))
}

fn in_keymap(map: &Keymap, context: &str, event: &str) -> bool {
    let (shift, text) = match event.strip_prefix("shift+") {
        Some(t) => (true, t),
        None => (false, event),
    };
    let ev = InputEvent::Key {
        text: text.into(),
        shift,
        ctrl: false,
        alt: false,
        down: true,
    };
    map.resolve(context, &ev).is_some()
}

fn in_grammar(event: &str) -> bool {
    let (ctrl, rest) = match event.strip_prefix("ctrl+") {
        Some(r) => (true, r),
        None => (false, event),
    };
    keys::resolve(rest, ctrl, false).is_some()
}

#[test]
fn every_help_hint_is_a_real_binding() {
    let map = Keymap::default_dj();
    let mut failures = Vec::new();
    for win in WINDOWS {
        let hints: &[Hint] = keys::surface(win.id);
        assert!(!hints.is_empty(), "{}: no hints to check", win.id);
        for style in win.styles {
            let body: String = style
                .iter()
                .map(|(file, comp)| {
                    let src = read(file);
                    if comp.is_empty() { src } else { handlers(&component(&src, comp)) }
                })
                .collect();
            for h in hints {
                assert!(!h.events.is_empty() || h.keys == "a–z", "{}: {:?} names no key", win.id, h.keys);
                for e in h.events {
                    let ok = handles(&body, e)
                        || win.keymap.is_some_and(|c| in_keymap(&map, c, e))
                        || (win.grammar && in_grammar(e));
                    if !ok {
                        failures.push(format!("{} ({}): {:?} for {:?}", win.id, style[0].0, e, h.action));
                    }
                }
            }
            // Deck and mixer windows hand their keys to the keymap.
            if win.keymap.is_some() {
                assert!(body.contains("DjState.key("), "{}: keys not forwarded", win.id);
            }
        }
    }
    assert!(failures.is_empty(), "help promises keys nothing handles:\n{}", failures.join("\n"));
}

#[test]
fn every_window_with_hints_is_checked() {
    for (id, _) in keys::SURFACES {
        assert!(WINDOWS.iter().any(|w| w.id == *id), "{id}: hints with no handler to check");
    }
}

#[test]
fn letters_are_the_windows_letters() {
    let spec = crate::spec::spec();
    for (c, id) in keys::LETTERS {
        let s = spec.surface(id).unwrap_or_else(|| panic!("{c}: no window {id}"));
        assert!(s.origin.is_implemented(), "{c} opens planned {id}");
        assert_eq!(spec.surface_for_key(*c).map(|s| s.id), Some(*id), "{c}");
    }
    for s in &spec.surfaces {
        assert_eq!(s.key, keys::letter(s.id), "{}", s.id);
    }
    // Y was the Player's and Diagnostics' both, and always opened the Player.
    assert_eq!(spec.surface("diagnostics").unwrap().key, None);
    assert_eq!(spec.surface_for_key('y').unwrap().id, "player");
}

#[test]
fn the_controller_letters_reach_every_unit() {
    // j k q were the controller's cursor and quit: DJ Booth, EQ and Plugins
    // could not be opened by letter from it.
    let controller = handlers(&component(&read("../patch-ui/ui/controller.slint"), "ControllerView"));
    for c in 'a'..='z' {
        assert!(
            !controller.contains(&format!("e.text == \"{c}\"")),
            "the controller takes {c}"
        );
    }
    let spec = crate::spec::spec();
    for c in ['j', 'k', 'q'] {
        assert!(spec.surface_for_key(c).is_some(), "{c}");
    }
}

#[test]
fn the_docs_keyboard_reference_is_generated() {
    let spec = crate::spec::spec();
    let title = |id: &str| -> String {
        spec.surface(id).map_or_else(
            || if id == "controller" { "Controller".into() } else { id.into() },
            // The window's own name: "Deck A", "MIDI & HID", "Inspector".
            |s| s.title.to_string(),
        )
    };
    let md = keys::markdown(&title, true);
    let short = keys::markdown(&title, false);
    let html = keys::html(&title);
    let block = |text: &str, begin: &str, end: &str| -> Option<String> {
        let i = text.find(begin)? + begin.len();
        let j = text[i..].find(end)? + i;
        Some(text[i..j].trim_matches('\n').to_string())
    };
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
    let bless = std::env::var_os("PATCH_BLESS_KEYS").is_some();
    for (file, want) in [
        ("user-manual.md", md.trim_end().to_string()),
        ("user-manual.html", html.trim_end().to_string()),
        ("getting-started.md", short.trim_end().to_string()),
    ] {
        let path = docs.join(file);
        let text = std::fs::read_to_string(&path).unwrap();
        let (begin, end) = ("<!-- keys:begin (generated from crates/patch-core/src/keys.rs) -->", "<!-- keys:end -->");
        let have = block(&text, begin, end).unwrap_or_else(|| panic!("{file}: no keys block"));
        if have != want && bless {
            let i = text.find(begin).unwrap() + begin.len();
            let j = text[i..].find(end).unwrap() + i;
            std::fs::write(&path, format!("{}\n{want}\n{}", &text[..i], &text[j..])).unwrap();
            continue;
        }
        assert_eq!(
            have, want,
            "{file}: the keyboard reference differs from keys.rs (PATCH_BLESS_KEYS=1 cargo test -p patch-surfaces keymap_tests rewrites it)"
        );
    }
}
