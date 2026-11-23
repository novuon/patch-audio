//! The Pattern Unit's window. It talks only to the engine's `pattern` op:
//! evaluate (quantised to the next cycle, or now), transport, tempo mode,
//! engine choice; it polls status a few times a second and the next events
//! while it is shown. A failed evaluation shows its errors at line:col and
//! the running pattern keeps playing (the service never swaps in bad code).

use super::*;
use serde_json::{Value, json};

#[derive(Default)]
pub(super) struct View {
    presence_at: Cell<Option<std::time::Instant>>,
    polls: crate::poll::Poller,
    /// The code as edited (every window shows the same).
    code: RefCell<String>,
    language: RefCell<String>,
    /// Edited since the service's copy was shown: polls don't overwrite it.
    dirty: Cell<bool>,
    /// The service's code was shown once (on first contact).
    seeded: Cell<bool>,
    playing: Cell<bool>,
    cycle: Cell<f64>,
    bpm: Cell<f64>,
    engine: RefCell<String>,
    /// Play once an evaluation is accepted (Ctrl+Enter from stopped).
    play_after_load: Cell<bool>,
    last_events: Cell<Option<std::time::Instant>>,
}

fn fmt_cycle(c: f64) -> String {
    // Cycle and beat within it: "12 · 3".
    if !c.is_finite() || c < 0.0 {
        return "–".into();
    }
    format!("{} · {}", c.floor() as i64, ((c.fract() * 4.0).floor() as i64) + 1)
}

impl AudioApp {
    pub(super) fn bind_pattern(&self, w: &PatchWindow) {
        let p = w.global::<PatternState>();
        if self.pattern.language.borrow().is_empty() {
            *self.pattern.language.borrow_mut() = "patch".into();
        }
        p.set_code(self.pattern.code.borrow().as_str().into());
        p.set_language(self.pattern.language.borrow().as_str().into());
        let me = self.me.clone();
        p.on_action(move |action| {
            if let Some(a) = me.upgrade() {
                a.pattern_action(&action);
            }
        });
        let me = self.me.clone();
        p.on_edited(move |code| {
            if let Some(a) = me.upgrade() {
                *a.pattern.code.borrow_mut() = code.to_string();
                a.pattern.dirty.set(true);
                a.pattern_each(|s| {
                    if s.get_code() != code {
                        s.set_code(code.clone());
                    }
                });
            }
        });
    }

    fn pattern_each(&self, f: impl Fn(&PatternState)) {
        for w in self.windows.borrow().iter().filter_map(|w| w.upgrade()) {
            f(&w.global::<PatternState>());
        }
    }

    fn pattern_message(&self, text: &str) {
        self.pattern_each(|s| s.set_message(text.into()));
    }

    fn pattern_send(&self, key: &'static str, req: Value) {
        if !self.pattern.polls.request(key, req) {
            self.pattern_message("Busy · try again");
        }
    }

    fn pattern_action(&self, action: &str) {
        let v = &self.pattern;
        match action {
            "evaluate" | "now" => {
                let code = v.code.borrow().clone();
                if code.trim().is_empty() {
                    self.pattern_message("Nothing to evaluate");
                    return;
                }
                let quantize = if action == "now" { "now" } else { "cycle" };
                v.play_after_load.set(!v.playing.get());
                self.pattern_send(
                    "pat-load",
                    json!({"op": "pattern", "action": "load", "language": *v.language.borrow(), "code": code, "quantize": quantize, "origin": "user"}),
                );
                self.pattern_message(if action == "now" { "Evaluating · now" } else { "Evaluating · on the next cycle" });
            }
            "play" | "stop" => self.pattern_send("pat-transport", json!({"op": "pattern", "action": action})),
            "follow" => self.pattern_send("pat-transport", json!({"op": "pattern", "action": "tempo"})),
            "own" => {
                let bpm = if v.bpm.get() >= 30.0 { v.bpm.get().round() } else { 120.0 };
                self.pattern_send("pat-transport", json!({"op": "pattern", "action": "tempo", "bpm": bpm}));
            }
            "language" => {
                let next = if *v.language.borrow() == "strudel" { "patch" } else { "strudel" };
                *v.language.borrow_mut() = next.into();
                self.pattern_each(|s| s.set_language(next.into()));
            }
            "engine" => {
                // auto → strudel → reference → auto.
                let next = match v.engine.borrow().as_str() {
                    "strudel" => "reference",
                    "reference" => "auto",
                    _ => "strudel",
                };
                self.pattern_send("pat-transport", json!({"op": "pattern", "action": "engine", "name": next}));
            }
            _ => {}
        }
    }

    fn pattern_status(&self, d: &Value) {
        let v = &self.pattern;
        let st = &d["status"];
        v.playing.set(st["playing"].as_bool().unwrap_or(false));
        v.cycle.set(st["cycle"].as_f64().unwrap_or(-1.0));
        v.bpm.set(st["bpm"].as_f64().unwrap_or(0.0));
        let engine = d["engine"].as_str().unwrap_or("").to_string();
        let choice = if d["engine_error"].is_string() { "reference".into() } else { engine.clone() };
        *v.engine.borrow_mut() = choice;
        let caps = &d["capabilities"];
        let mut flags = vec![];
        for f in ["program", "euclidean", "polymeter", "probability", "deterministic"] {
            if caps[f] == true {
                flags.push(f);
            }
        }
        let capabilities = match d["engine_error"].as_str() {
            Some(e) => e.to_string(),
            None if caps.is_object() => format!(
                "{} · {}",
                caps["languages"].as_array().map_or(String::new(), |l| {
                    l.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")
                }),
                flags.join(", ")
            ),
            None => String::new(),
        };
        let tempo = match d["tempo"]["mode"].as_str() {
            Some("own") => format!("own · {:.1} BPM", d["tempo"]["bpm"].as_f64().unwrap_or(0.0)),
            _ => {
                let deck = d["tempo"]["deck"].as_u64().map(|i| ((b'A' + i as u8) as char).to_string());
                format!(
                    "follows {} · {:.1} BPM",
                    deck.map_or("the lead deck".into(), |d| format!("deck {d}")),
                    v.bpm.get()
                )
            }
        };
        let layers: Vec<SharedString> = d["layers"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|l| format!("{} → {}", l["name"].as_str().unwrap_or(""), l["target"].as_str().unwrap_or("")).into())
                    .collect()
            })
            .unwrap_or_default();
        let m = &d["metrics"];
        let metrics = if m.is_object() {
            format!(
                "eval {} µs · query {} µs (max {}) · late {} · swap {:.0} ms",
                m["last_eval_us"], m["last_query_us"], m["max_query_us"], m["late_events"],
                m["last_swap_ms"].as_f64().unwrap_or(0.0)
            )
        } else {
            String::new()
        };
        let dl = &d["delivery"];
        let delivery = format!(
            "voices {} · commands {} ({} rejected) · samples {} ({} not ready) · visual {}",
            dl["voices"], dl["commands"], dl["rejected"], dl["samples"], dl["samples_skipped"], dl["timeline"]
        );
        // The service's code, shown once and whenever nothing local is
        // edited: what was written, else the definition's own source.
        let written = if d["source"].is_object() { &d["source"] } else { &d["definition"] };
        let seed = written["code"].as_str().map(|c| (c.to_string(), written["language"].as_str().unwrap_or("patch").to_string()));
        if !v.dirty.get()
            && !v.seeded.get()
            && let Some((code, lang)) = seed
        {
            v.seeded.set(true);
            *v.code.borrow_mut() = code.clone();
            *v.language.borrow_mut() = lang.clone();
            self.pattern_each(|s| {
                s.set_code(code.as_str().into());
                s.set_language(lang.as_str().into());
            });
        }
        self.pattern_errors(&d["errors"]);
        let layers = Rc::new(VecModel::from(layers));
        let origin = d["origin"].as_str().filter(|o| *o != "user").unwrap_or("").to_string();
        self.pattern_each(|s| {
            s.set_playing(v.playing.get());
            s.set_cycle(fmt_cycle(v.cycle.get()).into());
            s.set_engine(if engine.is_empty() { "starts on first load".into() } else { engine.as_str().into() });
            s.set_capabilities(capabilities.as_str().into());
            s.set_tempo(tempo.as_str().into());
            s.set_origin(origin.as_str().into());
            s.set_layers(layers.clone().into());
            s.set_metrics(metrics.as_str().into());
            s.set_delivery(delivery.as_str().into());
        });
    }

    fn pattern_errors(&self, errors: &Value) {
        let errors: Vec<PatError> = errors
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|e| PatError {
                        line: e["line"].as_i64().unwrap_or(0) as i32,
                        col: e["col"].as_i64().unwrap_or(0) as i32,
                        message: e["message"].as_str().unwrap_or("").into(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let errors = Rc::new(VecModel::from(errors));
        self.pattern_each(|s| s.set_errors(errors.clone().into()));
    }

    fn pattern_events(&self, d: &Value) {
        let rows: Vec<PatEvent> = d["rows"]
            .as_array()
            .map(|a| {
                a.iter()
                    .take(48)
                    .map(|e| PatEvent {
                        cycle: format!("{:.3}", e["cycle"].as_f64().unwrap_or(0.0)).into(),
                        target: e["target"].as_str().unwrap_or("").into(),
                        value: e["value"].as_str().unwrap_or("").into(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let rows = Rc::new(VecModel::from(rows));
        self.pattern_each(|s| s.set_events(rows.clone().into()));
    }

    /// Called from the UI tick: take replies, then poll while shown.
    pub(super) fn poll_pattern(&self) {
        if self.frozen {
            return;
        }
        let v = &self.pattern;
        for key in ["pat-load", "pat-transport", "pat-status", "pat-events"] {
            let Some(reply) = v.polls.take(key) else { continue };
            match (key, reply.result) {
                ("pat-load", Ok(d)) if d["ok"] == false => {
                    let n = d["errors"].as_array().map_or(0, Vec::len);
                    self.pattern_message(&format!(
                        "{n} error{} · {}",
                        if n == 1 { "" } else { "s" },
                        if v.playing.get() { "the previous pattern keeps playing" } else { "nothing loaded" }
                    ));
                    self.pattern_errors(&d["errors"]);
                }
                ("pat-load", Ok(d)) => {
                    v.dirty.set(false);
                    v.seeded.set(true);
                    self.pattern_message(&format!(
                        "Evaluated in {} µs · takes over at cycle {}",
                        d["eval_us"], d["at"].as_str().unwrap_or("now")
                    ));
                    if v.play_after_load.replace(false) {
                        self.pattern_send("pat-transport", json!({"op": "pattern", "action": "play"}));
                    }
                }
                ("pat-events", Ok(d)) => self.pattern_events(&d),
                // Status, and transport replies (which are a status).
                (_, Ok(d)) => self.pattern_status(&d),
                (_, Err(e)) => self.pattern_message(if e.contains("unknown op") {
                    "This engine predates Pattern. Upgrade the running Patch engine to this build (omarchy-patch engine upgrade)."
                } else {
                    &e
                }),
            }
        }
        let shown = self
            .windows
            .borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .any(|w| w.get_surface() == "pattern");
        v.polls.take("pat-presence");
        if !shown {
            if v.presence_at.replace(None).is_some() {
                v.polls.request("pat-presence", json!({"op":"module_presence", "module":"pattern", "owner":std::process::id(), "open":false}));
            }
            return;
        }
        if v.presence_at.get().is_none_or(|t| t.elapsed().as_secs() >= 1)
            && v.polls.request("pat-presence", json!({"op":"module_presence", "module":"pattern", "owner":std::process::id()}))
        {
            v.presence_at.set(Some(std::time::Instant::now()));
        }
        v.polls.request("pat-status", json!({"op": "pattern", "action": "status"}));
        let due = v.last_events.get().is_none_or(|t| t.elapsed().as_millis() >= 500);
        if due && v.playing.get() && v.cycle.get() >= 0.0 {
            let from = v.cycle.get();
            if v.polls.request("pat-events", json!({"op": "pattern", "action": "events", "from": from, "to": from + 2.0})) {
                v.last_events.set(Some(std::time::Instant::now()));
            }
        }
    }
}
