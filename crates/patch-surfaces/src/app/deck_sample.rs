//! A deck's sample maker, as its window shows it (`x` on a deck): the
//! panel's choices, the engine's `deck_sample` op behind them, and what was
//! made from the track. Requests run off the UI thread (a poller): the
//! stems' catalogue and ffmpeg take their time.

use super::*;
use crate::poll::Poller;
use serde_json::{Value, json};
use std::time::Instant;

/// Beats the "what just played" choices take (after "the loop").
const BEATS: [(f64, &str); 5] = [(1.0, "1 beat"), (4.0, "1 bar"), (8.0, "2 bars"), (16.0, "4 bars"), (32.0, "8 bars")];
const SLICES: [u32; 4] = [1, 4, 8, 16];

#[derive(Default)]
pub(super) struct SampleMaker {
    polls: Poller,
    /// The deck whose panel is open ("" none).
    pub(super) deck: RefCell<String>,
    /// The engine's last status for it.
    status: RefCell<Value>,
    /// The panel's choices: what (0 loop, 1… beats), from, slices, name.
    what: Cell<usize>,
    from: RefCell<String>,
    slices: Cell<usize>,
    name: RefCell<String>,
    message: RefCell<String>,
    busy: Cell<bool>,
    opened: Cell<Option<Instant>>,
}

impl AudioApp {
    pub(super) fn bind_sample(&self, w: &PatchWindow) {
        // Headless shots of the panel (`PATCH_SHOT_SAMPLE=1`): open on deck
        // A with a loop, stems and three samples made, as a live deck has.
        if patch_host::shot::active() && std::env::var_os("PATCH_SHOT_SAMPLE").is_some() && self.sample.deck.borrow().is_empty() {
            *self.sample.deck.borrow_mut() = "a".into();
            *self.sample.status.borrow_mut() = json!({
                "default_name": "midnight", "bpm": 124.0, "loop_beats": 4.0,
                "stems": ["vocals", "drums", "bass", "other"],
                "settings": {"what": {"kind": "loop"}, "from": "drums", "slices": 1, "name": ""},
                "made": [
                    {"name": "midnight", "n": 2, "files": 1, "label": "loop · 1 bar · drums", "code": "loopAt 1 $ chop 8 $ s \"midnight:2\""},
                    {"name": "midnight", "n": 1, "files": 1, "label": "1 bar · vocals", "code": "loopAt 1 $ chop 8 $ s \"midnight:1\""},
                    {"name": "midnight", "n": 0, "files": 1, "label": "1 beat · mix", "code": "s \"midnight:0\""},
                ],
            });
            *self.sample.from.borrow_mut() = "drums".into();
            *self.sample.name.borrow_mut() = "midnight".into();
        }
        let d = w.global::<DjState>();
        let me = self.me.clone();
        d.on_sample_set(move |kind, i| {
            if let Some(a) = me.upgrade() {
                let i = i.max(0) as usize;
                let s = &a.sample;
                match kind.as_str() {
                    "what" => s.what.set(i),
                    "slices" => s.slices.set(i.min(SLICES.len() - 1)),
                    "from" => {
                        let froms = Self::sample_froms(&s.status.borrow());
                        if let Some(f) = froms.get(i) {
                            *s.from.borrow_mut() = f.clone();
                        }
                    }
                    _ => {}
                }
                s.message.borrow_mut().clear();
                a.sample_show();
            }
        });
        let me = self.me.clone();
        d.on_sample_named(move |t| {
            if let Some(a) = me.upgrade() {
                *a.sample.name.borrow_mut() = t.trim().to_string();
                a.sample.message.borrow_mut().clear();
                a.sample_show();
            }
        });
        let me = self.me.clone();
        d.on_sample_save(move || {
            if let Some(a) = me.upgrade() {
                a.sample_save();
            }
        });
        let me = self.me.clone();
        d.on_sample_close(move || {
            if let Some(a) = me.upgrade() {
                a.sample.deck.borrow_mut().clear();
                a.sample_show();
            }
        });
        let me = self.me.clone();
        d.on_sample_copy(move |i| {
            if let Some(a) = me.upgrade() {
                let code = a.sample.status.borrow()["made"][i.max(0) as usize]["code"].as_str().map(str::to_string);
                if let Some(code) = code {
                    *a.sample.message.borrow_mut() = match to_clipboard(&code) {
                        Ok(()) => format!("copied: {code}"),
                        Err(e) => e,
                    };
                    a.sample_show();
                }
            }
        });
        self.sample_show_on(w);
    }

    /// `x` on a deck: its panel opens (or closes when it is open).
    pub(super) fn sample_toggle(&self, deck: &str) {
        let open = *self.sample.deck.borrow() == deck;
        *self.sample.deck.borrow_mut() = if open { String::new() } else { deck.to_string() };
        if !open {
            self.sample.opened.set(Some(Instant::now()));
            self.sample.message.borrow_mut().clear();
            *self.sample.status.borrow_mut() = Value::Null;
            if !self.sample.polls.request("sample-status", json!({"op": "deck_sample", "deck": deck, "action": "status"})) {
                *self.sample.message.borrow_mut() = "Could not read the deck. Close and reopen Sample to retry.".into();
            }
        }
        self.sample_show();
    }

    /// The panel's choices as the engine takes them.
    fn sample_request(&self, deck: &str) -> Value {
        let s = &self.sample;
        let mut r = json!({"op": "deck_sample", "deck": deck, "action": "save",
            "track": s.status.borrow()["track"],
            "from": *s.from.borrow(), "slices": SLICES[s.slices.get().min(SLICES.len() - 1)],
            "name": *s.name.borrow()});
        match s.what.get() {
            0 => r["what"] = json!("loop"),
            i => {
                r["what"] = json!("beats");
                r["beats"] = json!(BEATS[(i - 1).min(BEATS.len() - 1)].0);
            }
        }
        r
    }

    /// A sample button (a controller, `deck a sample`): saved at once with
    /// the panel's choices when it is open on that deck, else with the
    /// engine's last ones; the panel opens to show what was made.
    pub(super) fn sample_save_on(&self, deck: &str) {
        if self.sample.busy.get() {
            return;
        }
        if *self.sample.deck.borrow() == deck && !self.sample.status.borrow().is_null() {
            self.sample_save();
            return;
        }
        if *self.sample.deck.borrow() != deck {
            self.sample_toggle(deck);
        }
        if self.sample.polls.request("sample-save", json!({"op": "deck_sample", "deck": deck, "action": "save"})) {
            self.sample.busy.set(true);
        }
        self.sample_show();
    }

    pub(super) fn sample_save(&self) {
        let deck = self.sample.deck.borrow().clone();
        if deck.is_empty() || self.sample.busy.get() {
            return;
        }
        if !self.is_live() {
            *self.sample.message.borrow_mut() = "samples are made from a live deck".into();
            self.sample_show();
            return;
        }
        if self.sample.status.borrow().is_null() {
            return;
        }
        let req = self.sample_request(&deck);
        if self.sample.polls.request("sample-save", req) {
            self.sample.busy.set(true);
            self.sample.message.borrow_mut().clear();
        }
        self.sample_show();
    }

    /// Replies that came in: the status (the choices start from the
    /// engine's), a save's result.
    pub(super) fn sample_tick(&self) {
        let s = &self.sample;
        if let Some(r) = s.polls.take("sample-status")
            && !s.deck.borrow().is_empty()
            && s.opened.get().is_some_and(|opened| r.requested >= opened)
        {
            match r.result {
                Ok(v) => {
                    let set = &v["settings"];
                    s.what.set(match set["what"]["kind"].as_str() {
                        Some("beats") => {
                            let b = set["what"]["beats"].as_f64().unwrap_or(4.0);
                            BEATS.iter().position(|x| x.0 == b).map_or(2, |p| p + 1)
                        }
                        _ if v["loop_beats"].is_null() => 2,
                        _ => 0,
                    });
                    let from = set["from"].as_str().unwrap_or("mix").to_string();
                    *s.from.borrow_mut() = if from == "mix" || Self::sample_froms(&v).contains(&from) { from } else { "mix".into() };
                    let n = set["slices"].as_u64().unwrap_or(1) as u32;
                    s.slices.set(SLICES.iter().position(|x| *x == n).unwrap_or(0));
                    let name = set["name"].as_str().filter(|n| !n.is_empty()).unwrap_or_else(|| v["default_name"].as_str().unwrap_or("deck"));
                    *s.name.borrow_mut() = name.to_string();
                    *s.status.borrow_mut() = v;
                }
                Err(e) => *s.message.borrow_mut() = e,
            }
            self.sample_show();
        }
        if let Some(r) = s.polls.take("sample-save") {
            s.busy.set(false);
            if r.result.is_ok() {
                patch_pattern::samples::refresh();
            }
            if s.deck.borrow().is_empty() || s.opened.get().is_none_or(|opened| r.requested < opened) {
                self.sample_show();
                return;
            }
            match r.result {
                Ok(v) => {
                    let saved = &v["saved"];
                    let files = saved["files"].as_u64().unwrap_or(1);
                    let which = if files > 1 {
                        format!("{} … {}", saved["n"], saved["n"].as_u64().unwrap_or(0) + files - 1)
                    } else {
                        saved["n"].to_string()
                    };
                    *s.message.borrow_mut() = format!(
                        "saved {}:{which} · {}",
                        saved["name"].as_str().unwrap_or(""),
                        saved["code"].as_str().unwrap_or("")
                    );
                    if let Some(made) = v.get("made") {
                        s.status.borrow_mut()["made"] = made.clone();
                    }
                }
                Err(e) => *s.message.borrow_mut() = e,
            }
            self.sample_show();
        }
    }

    /// "Mix", then the track's stems that are ready.
    fn sample_froms(status: &Value) -> Vec<String> {
        std::iter::once("mix".to_string())
            .chain(status["stems"].as_array().into_iter().flatten().filter_map(|s| s.as_str().map(str::to_string)))
            .collect()
    }

    pub(super) fn sample_show(&self) {
        for w in self.dj_windows() {
            self.sample_show_on(&w);
        }
    }

    fn sample_show_on(&self, w: &PatchWindow) {
        let d = w.global::<DjState>();
        let s = &self.sample;
        let deck = s.deck.borrow().clone();
        d.set_sample_deck(deck.as_str().into());
        if deck.is_empty() {
            return;
        }
        let st = s.status.borrow();
        let loop_beats = st["loop_beats"].as_f64();
        let mut whats = vec![match loop_beats {
            Some(b) => format!("the loop · {} beats", fmt_beats(b)),
            None => "the loop (none set)".to_string(),
        }];
        whats.extend(BEATS.iter().map(|b| b.1.to_string()));
        let froms = Self::sample_froms(&st);
        let from = s.from.borrow().clone();
        let cap = |t: &str| {
            let mut c = t.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        };
        d.set_sample_whats(model(whats.iter().map(String::as_str)));
        let short_loop = if loop_beats.is_some() { "Loop" } else { "No loop" };
        d.set_sample_whats_short(model(std::iter::once(short_loop).chain(BEATS.iter().map(|b| b.1))));
        d.set_sample_what(s.what.get() as i32);
        d.set_sample_froms(model(froms.iter().map(|f| cap(f)).collect::<Vec<_>>().iter().map(String::as_str)));
        d.set_sample_from(froms.iter().position(|f| *f == from).unwrap_or(0) as i32);
        d.set_sample_from_note(
            if st.is_null() {
                "…"
            } else if froms.len() > 1 {
                "stems are ready for this track"
            } else {
                "stems: prepare them for this track to take one"
            }
            .into(),
        );
        d.set_sample_slices(s.slices.get() as i32);
        let name = s.name.borrow().clone();
        if d.get_sample_name() != name.as_str() {
            d.set_sample_name(name.as_str().into());
        }
        // What saving makes: the name's next n.
        let slices = SLICES[s.slices.get().min(SLICES.len() - 1)] as usize;
        let hint = if name.is_empty() {
            "a name, or the track's".to_string()
        } else if !patch_pattern::samples::valid_name(&name) {
            "letters, digits, _ - . only".to_string()
        } else {
            let have = patch_pattern::samples::index().get(&name).map_or(0, |f| f.files.len());
            let next = if slices > 1 { format!("{name}:{have} … {name}:{}", have + slices - 1) } else { format!("{name}:{have}") };
            match have {
                0 => format!("a new sound · this is {next}"),
                1 => format!("1 sound in it already · this is {next}"),
                n => format!("{n} sounds in it already · this is {next}"),
            }
        };
        d.set_sample_hint(hint.as_str().into());
        let bpm = st["bpm"].as_f64().filter(|b| *b > 0.0);
        let beats = match s.what.get() {
            0 => loop_beats,
            i => Some(BEATS[(i - 1).min(BEATS.len() - 1)].0),
        };
        let length = match (beats, bpm) {
            (Some(b), Some(bpm)) => format!("{:.2} s at the track's {bpm:.0} BPM · as the track is, not time-stretched", b * 60.0 / bpm),
            _ => "as the track is, not time-stretched".to_string(),
        };
        d.set_sample_length(length.as_str().into());
        d.set_sample_message(s.message.borrow().as_str().into());
        d.set_sample_busy(s.busy.get());
        d.set_sample_ready(!st.is_null());
        let rows: Vec<SampleRow> = st["made"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| {
                let n = m["n"].as_u64().unwrap_or(0);
                let files = m["files"].as_u64().unwrap_or(1);
                let name = m["name"].as_str().unwrap_or("");
                SampleRow {
                    name: if files > 1 { format!("{name}:{n} … {}", n + files - 1) } else { format!("{name}:{n}") }.into(),
                    label: m["label"].as_str().unwrap_or("").into(),
                    code: m["code"].as_str().unwrap_or("").into(),
                }
            })
            .collect();
        d.set_sample_made(ModelRc::new(VecModel::from(rows)));
    }
}

fn fmt_beats(b: f64) -> String {
    let s = format!("{b:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn model<'a>(items: impl Iterator<Item = &'a str>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(items.map(SharedString::from).collect::<Vec<_>>()))
}

/// Text to the clipboard (Wayland's `wl-copy`).
fn to_clipboard(text: &str) -> Result<(), String> {
    use std::io::Write;
    let mut child = std::process::Command::new("wl-copy")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| "copying needs wl-copy (wl-clipboard)".to_string())?;
    child.stdin.take().ok_or("wl-copy")?.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    child.wait().map_err(|e| e.to_string())?;
    Ok(())
}
