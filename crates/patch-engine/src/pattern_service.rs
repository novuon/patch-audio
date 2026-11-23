//! The Pattern Unit in the Patch service: one running pattern, kept by the
//! engine (it plays on with every window closed), on the graph clock.
//!
//! A control thread ticks every 10 ms: it follows the lead deck's grid (or
//! the Pattern's own tempo), asks the scheduler for the next window, and
//! routes each event: Sequencer voices into the real-time queue (played on
//! their sample), commands validated with the program's origin and run at
//! their time, everything else (visual, scene, control, AI) onto a timeline
//! the Visualiser and other units read with the clock that placed it.
//!
//! The engine behind the scheduler is chosen here: the Strudel adapter when
//! it is installed, else Patch's reference engine. Nothing outside this
//! module knows which.
//!
//! The Tidal Unit runs a second service of its own ([`Service::tidal`]):
//! Patch's native Tidal engine, its events played by the engine's
//! instrument from its own queue, and an `eval` action that changes only
//! the channels a block names, the way Tidal does.

use patch_core::command::{Command, Origin};
use patch_core::material::{AssetId, SampleSource};
use patch_pattern::backend::{Backend, Definition, Reference};
use patch_pattern::event::{Action, Event, Target, route};
use patch_pattern::instrument::SampleInfo;
use patch_pattern::rt::{Queue, RtEvent, RtKind};
use patch_pattern::tidal;
use patch_pattern::scheduler::{Quantize, Scheduler};
use patch_pattern::time::{Span, Time};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::Mutex;

/// What the service needs from the engine around it.
pub trait Host: Send + Sync {
    /// The graph sample now (the latest stamp, carried on) and the rate.
    fn now(&self) -> Option<(i64, u32)>;
    /// A deck's beat at a stamped graph sample, and its tempo as played:
    /// (sample, beats since the first downbeat, BPM). `None` for the lead
    /// deck: whichever plays (the louder of two).
    fn deck_clock(&self, deck: Option<usize>) -> Option<(i64, f64, f64)>;
    /// Run a command as `origin` would (validated, limits applied).
    fn command(&self, cmd: Command, origin: Origin) -> Result<(), String>;
    /// Where Sequencer voices go.
    fn voices(&self) -> &Queue;
    /// The sampler slot that plays derived material (a kit's `slot`), for an
    /// event at graph sample `at`; loaded on first use. An error means it
    /// isn't ready: the event is skipped and counted, never waited for.
    fn sample_slot(
        &self,
        _source: &SampleSource,
        _slot: Option<&str>,
        _at: i64,
    ) -> Result<u32, String> {
        Err("no sampler".into())
    }
    /// Fade out every sounding sample (the pattern stopped).
    fn stop_samples(&self) {}
    /// A `derived:…` address (Patch code) as an asset id.
    fn resolve_material(&self, address: &str) -> Result<AssetId, String> {
        Err(format!(
            "'{address}': derived material needs the Patch service"
        ))
    }
    /// The Tidal instrument's sample `n` of folder `name`: `None` when
    /// there is no such folder (a built-in sound may play).
    fn sample(&self, _name: &str, _n: f64) -> Option<Result<SampleInfo, String>> {
        None
    }
    /// Load the samples `code` names before it plays (`wait`: for a
    /// moment, else in the background).
    fn warm_samples(&self, _code: &str, _wait: bool) {}
    /// The sample folders and what is loaded, for the status.
    fn samples_status(&self) -> Value {
        Value::Null
    }
}

/// The host's samples, as the instrument asks for them.
struct HostSamples<'a>(&'a dyn Host);

impl patch_pattern::instrument::Samples for HostSamples<'_> {
    fn sample(&self, name: &str, n: f64) -> Option<Result<SampleInfo, String>> {
        self.0.sample(name, n)
    }
}

/// The host as the DSL's resolver of `derived:` addresses.
struct Resolve<'a>(&'a dyn Host);

impl patch_pattern::dsl::Resolver for Resolve<'_> {
    fn resolve(&self, address: &str) -> Result<AssetId, String> {
        self.0.resolve_material(address)
    }
}

/// Patch code becomes a program here, whichever engine runs it (Strudel
/// takes programs, not Patch code), with `derived:` addresses resolved to
/// asset ids (the engines only see typed ids). The code is kept for the
/// editor (`Saved::source`).
fn resolve_source(
    def: Definition,
    host: &dyn Host,
) -> Result<Definition, Vec<patch_pattern::backend::PatternError>> {
    match def {
        Definition::Source {
            language,
            code,
            cycle_beats,
        } if language == "patch" => {
            let mut program = patch_pattern::dsl::parse_with(&code, &Resolve(host))
                .map_err(|e| e.into_iter().map(Into::into).collect::<Vec<_>>())?;
            if !code.lines().any(|l| l.trim_start().starts_with("cycle")) {
                program.cycle_beats = cycle_beats;
            }
            Ok(Definition::Program { program })
        }
        other => Ok(other),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Tempo {
    /// The lead deck's grid (or one deck's), free-running at the last
    /// tempo while no deck plays.
    Follow {
        #[serde(default)]
        deck: Option<usize>,
    },
    /// The Pattern's own tempo.
    Own { bpm: f64 },
}

impl Default for Tempo {
    fn default() -> Tempo {
        Tempo::Follow { deck: None }
    }
}

/// What survives a restart (`pattern.json` in the state directory).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Saved {
    pub definition: Option<Definition>,
    pub quantize: Quantize,
    pub origin: Option<Origin>,
    pub tempo: Tempo,
    /// "auto" (Strudel when installed), "strudel" or "reference".
    pub engine: Option<String>,
    /// The code as written, when the definition was resolved from it
    /// (`derived:` addresses become asset ids): what the editor shows.
    pub source: Option<Definition>,
}

/// An event on the timeline, with the clock that placed it.
#[derive(Clone, Debug, Serialize)]
pub struct Timed {
    pub sample: i64,
    pub end: i64,
    pub cycle: Time,
    pub event: Event,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Delivery {
    pub voices: u64,
    pub voices_dropped: u64,
    pub commands: u64,
    pub rejected: u64,
    pub last_rejection: Option<String>,
    /// Commands run after their time, worst and last (ms).
    pub command_late_max_ms: f64,
    pub command_late_last_ms: f64,
    pub samples: u64,
    /// Derived material not ready (or unknown) when its event came due.
    pub samples_skipped: u64,
    pub last_sample_error: Option<String>,
    pub timeline: u64,
    /// Instrument events played, and those it had no sound for.
    pub played: u64,
    pub unplayable: u64,
    pub last_unplayable: Option<String>,
}

/// Which Unit a service runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// The Pattern Unit: Strudel or the reference engine, as chosen.
    Pattern,
    /// The Tidal Unit: Patch's native Tidal engine, always.
    Tidal,
}

struct Inner {
    sched: Option<Scheduler>,
    engine_name: String,
    saved: Saved,
    commands: VecDeque<(i64, Command)>,
    timeline: VecDeque<Timed>,
    delivery: Delivery,
    errors: Vec<patch_pattern::backend::PatternError>,
    last_bpm: f64,
    /// The engine failed to start (shown, retried on the next load).
    engine_error: Option<String>,
    /// The loaded definition's layers (name and target), for the surface.
    layers: Vec<patch_pattern::backend::LayerInfo>,
    /// What loaded but won't sound as written.
    warnings: Vec<String>,
    unit: Unit,
}

pub struct Service {
    inner: Mutex<Inner>,
    path: std::path::PathBuf,
}

const TIMELINE: usize = 2048;

fn origin_of(v: &Value) -> Origin {
    serde_json::from_value(v.clone()).unwrap_or(Origin::User)
}

impl Service {
    pub fn new(path: std::path::PathBuf) -> Service {
        Service::for_unit(path, Unit::Pattern)
    }

    /// The Tidal Unit's service (`tidal.json`).
    pub fn tidal(path: std::path::PathBuf) -> Service {
        Service::for_unit(path, Unit::Tidal)
    }

    fn for_unit(path: std::path::PathBuf, unit: Unit) -> Service {
        let saved: Saved = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_else(|| match unit {
                // Tidal starts at its own default, 0.5625 cycles a second.
                Unit::Tidal => Saved {
                    tempo: Tempo::Own { bpm: 135.0 },
                    quantize: Quantize::Now,
                    ..Saved::default()
                },
                Unit::Pattern => Saved::default(),
            });
        Service {
            inner: Mutex::new(Inner {
                sched: None,
                engine_name: String::new(),
                saved,
                commands: VecDeque::new(),
                timeline: VecDeque::new(),
                delivery: Delivery::default(),
                errors: vec![],
                last_bpm: 120.0,
                engine_error: None,
                layers: vec![],
                warnings: vec![],
                unit,
            }),
            path,
        }
    }

    fn save(&self, saved: &Saved) {
        let tmp = self.path.with_extension("tmp");
        if let Ok(b) = serde_json::to_vec_pretty(saved)
            && std::fs::write(&tmp, b).is_ok()
        {
            let _ = std::fs::rename(tmp, &self.path);
        }
    }

    /// The engine the saved choice asks for, started.
    fn engine(choice: &str) -> (Box<dyn Backend>, String, Option<String>) {
        if choice != "reference" {
            match patch_pattern::strudel::locate()
                .map(|d| patch_pattern::strudel::Strudel::start(&d))
            {
                Some(Ok(s)) => return (Box::new(s), "strudel".into(), None),
                Some(Err(e)) => {
                    let why = format!("Strudel: {e}; using the reference engine");
                    return (
                        Box::new(Reference::default()),
                        "reference".into(),
                        Some(why),
                    );
                }
                None if choice == "strudel" => {
                    let why = "Strudel's adapter isn't installed (adapters/strudel: npm install); using the reference engine".to_string();
                    return (
                        Box::new(Reference::default()),
                        "reference".into(),
                        Some(why),
                    );
                }
                None => {}
            }
        }
        (Box::new(Reference::default()), "reference".into(), None)
    }

    fn ensure(inner: &mut Inner, rate: u32) {
        if inner.sched.is_some() {
            return;
        }
        let choice = inner.saved.engine.clone().unwrap_or_else(|| "auto".into());
        let (engine, name, err) = match inner.unit {
            Unit::Tidal => (
                Box::new(tidal::Tidal::default()) as Box<dyn Backend>,
                "tidal".to_string(),
                None,
            ),
            Unit::Pattern => Service::engine(&choice),
        };
        inner.engine_name = name;
        inner.engine_error = err;
        let bpm = match inner.saved.tempo {
            Tempo::Own { bpm } => bpm,
            Tempo::Follow { .. } => inner.last_bpm,
        };
        inner.sched = Some(Scheduler::new(engine, rate, bpm));
    }

    /// The `pattern` op: {"op":"pattern","action":…}.
    pub fn request(&self, host: &dyn Host, req: &Value) -> Result<Value, String> {
        let action = req["action"].as_str().unwrap_or("status");
        // The samples Tidal code names load before it plays, outside the
        // lock (the scheduler ticks on meanwhile).
        let tidal_unit = self.inner.lock().unwrap().unit == Unit::Tidal;
        if tidal_unit && matches!(action, "load" | "eval") && let Some(code) = req["code"].as_str() {
            host.warm_samples(code, true);
        }
        let (now, rate) = host.now().unwrap_or((0, 48_000));
        let mut inner = self.inner.lock().unwrap();
        match action {
            "samples" => Ok(host.samples_status()),
            "status" => Ok(self.status(&mut inner, now)),
            "load" => {
                let def: Definition = if req["definition"].is_object() {
                    serde_json::from_value(req["definition"].clone()).map_err(|e| e.to_string())?
                } else {
                    Definition::Source {
                        language: req["language"].as_str().unwrap_or("strudel").into(),
                        code: req["code"]
                            .as_str()
                            .ok_or("load needs a definition or code")?
                            .into(),
                        cycle_beats: Time::whole(4),
                    }
                };
                let written = def.clone();
                let def = match resolve_source(def, host) {
                    Ok(d) => d,
                    Err(errors) => {
                        inner.errors = errors.clone();
                        return Ok(json!({"ok": false, "errors": errors}));
                    }
                };
                let source = (written != def).then_some(written);
                if let Definition::Program { program } = &def {
                    program.validate()?;
                }
                let q: Quantize =
                    serde_json::from_value(req["quantize"].clone()).unwrap_or(inner.saved.quantize);
                let origin = origin_of(&req["origin"]);
                // An AI may not run live code: it speaks Patch definitions.
                if origin == Origin::Ai && !matches!(def, Definition::Program { .. }) {
                    return Err(
                        "AI patterns are Patch definitions (a program), not source code".into(),
                    );
                }
                Service::ensure(&mut inner, rate);
                let sched = inner.sched.as_mut().unwrap();
                match sched.load(&def, q, now) {
                    Ok((loaded, at)) => {
                        // `setcps`: the code sets its own tempo.
                        if let Some(bpm) = loaded.bpm.filter(|b| (30.0..=300.0).contains(b)) {
                            sched.set_bpm(now, bpm);
                            inner.saved.tempo = Tempo::Own { bpm };
                        }
                        inner.errors.clear();
                        inner.warnings = loaded.warnings.clone();
                        inner.layers = loaded.layers.clone();
                        inner.saved.definition = Some(def);
                        inner.saved.source = source;
                        inner.saved.quantize = q;
                        inner.saved.origin = Some(origin);
                        self.save(&inner.saved);
                        Ok(
                            json!({"ok": true, "at": at, "layers": loaded.layers, "eval_us": loaded.eval_us, "warnings": loaded.warnings}),
                        )
                    }
                    Err(errors) => {
                        inner.errors = errors.clone();
                        Ok(json!({"ok": false, "errors": errors}))
                    }
                }
            }
            "play" => {
                // From the top: a transition or `once` left in the code
                // (cycles counted from where it stopped) is over.
                if inner.unit == Unit::Tidal
                    && !inner.sched.as_ref().is_some_and(|s| s.playing())
                    && let Some(Definition::Source { language, code, .. }) = &inner.saved.definition
                    && language == "tidal"
                    && let Ok(mut session) = tidal::Session::parse(code)
                {
                    let before = session.clone();
                    session.settle(None);
                    if session != before {
                        drop(inner);
                        self.request(host, &json!({"action": "load", "language": "tidal", "code": session.code(), "quantize": "now", "origin": "user"}))?;
                        inner = self.inner.lock().unwrap();
                    }
                }
                Service::ensure(&mut inner, rate);
                if inner.sched.as_ref().unwrap().status(now).active.is_none()
                    && inner.sched.as_ref().unwrap().status(now).pending.is_none()
                {
                    let Some(def) = inner.saved.definition.clone() else {
                        return Err("nothing to play: load a pattern first".into());
                    };
                    let q = inner.saved.quantize;
                    inner
                        .sched
                        .as_mut()
                        .unwrap()
                        .load(&def, q, now)
                        .map_err(|e| {
                            e.iter()
                                .map(|e| e.to_string())
                                .collect::<Vec<_>>()
                                .join("; ")
                        })?;
                }
                let tempo = inner.saved.tempo.clone();
                let lead = match tempo {
                    Tempo::Follow { deck } => host.deck_clock(deck),
                    Tempo::Own { .. } => None,
                };
                let last_bpm = inner.last_bpm;
                let sched = inner.sched.as_mut().unwrap();
                let la = sched.lookahead;
                match (tempo, lead) {
                    // In phase with the deck: its beat now is the cycle now.
                    (Tempo::Follow { .. }, Some((sample, beat, bpm))) => {
                        let cb = sched.tempo.cycle_beats.to_f64();
                        let at = Time::approx(beat / cb, 1 << 20);
                        sched.play(sample, at, bpm);
                        inner.last_bpm = bpm;
                    }
                    (Tempo::Own { bpm }, _) => sched.play(now + la, Time::ZERO, bpm),
                    (_, None) => sched.play(now + la, Time::ZERO, last_bpm),
                }
                Ok(self.status(&mut inner, now))
            }
            "stop" => {
                if let Some(s) = inner.sched.as_mut() {
                    s.stop();
                }
                inner.commands.clear();
                host.voices().cancel();
                host.stop_samples();
                Ok(self.status(&mut inner, now))
            }
            "tempo" => {
                let tempo = if let Some(bpm) = req["bpm"].as_f64() {
                    if !(30.0..=300.0).contains(&bpm) {
                        return Err("tempo is 30 … 300 BPM".into());
                    }
                    Tempo::Own { bpm }
                } else {
                    Tempo::Follow {
                        deck: match req["follow"].as_str() {
                            Some("a") => Some(0),
                            Some("b") => Some(1),
                            Some("c") => Some(2),
                            Some("d") => Some(3),
                            _ => None,
                        },
                    }
                };
                if let (Tempo::Own { bpm }, Some(s)) = (&tempo, inner.sched.as_mut()) {
                    s.set_bpm(now, *bpm);
                }
                inner.saved.tempo = tempo;
                self.save(&inner.saved);
                Ok(self.status(&mut inner, now))
            }
            // Tidal: a block evaluated on top of what plays (only the
            // channels it names change), then played at once.
            "eval" if inner.unit == Unit::Tidal => {
                let block = req["code"].as_str().ok_or("eval needs code")?;
                let running = match &inner.saved.definition {
                    Some(Definition::Source { language, code, .. }) if language == "tidal" => code.clone(),
                    _ => String::new(),
                };
                let mut session = tidal::Session::parse(&running).unwrap_or_default();
                // Transitions and `once` start at the cycle playing now.
                let cycle = inner.sched.as_ref().filter(|s| s.playing()).map(|s| s.status(now).cycle);
                let touched = match session.apply_at(block, cycle) {
                    Ok(t) => t,
                    Err(errors) => {
                        let errors: Vec<_> = errors.iter().map(|e| e.located(block)).collect();
                        inner.errors = errors.clone();
                        return Ok(json!({"ok": false, "errors": errors}));
                    }
                };
                let playing = inner.sched.as_ref().is_some_and(|s| s.playing());
                drop(inner);
                let mut load = json!({
                    "action": "load",
                    "language": "tidal",
                    "code": session.code(),
                    "quantize": req.get("quantize").cloned().unwrap_or(json!("now")),
                    "origin": "user",
                });
                if req["origin"].is_string() {
                    load["origin"] = req["origin"].clone();
                }
                let mut r = self.request(host, &load)?;
                // Stopped, a block that gives a channel code starts it; mute,
                // solo, silence or a let alone don't (a button press on a
                // stopped unit started all of it).
                let plays = touched.iter().any(|t| session.channels.iter().any(|c| &c.0 == t));
                if r["ok"] == true && !playing && plays {
                    self.request(host, &json!({"action": "play"}))?;
                }
                r["touched"] = json!(touched);
                r["code"] = json!(session.code());
                Ok(r)
            }
            "engine" if inner.unit == Unit::Tidal => {
                Err("the Tidal Unit runs Patch's native Tidal engine".into())
            }
            "engine" => {
                let name = req["name"].as_str().unwrap_or("auto");
                if !matches!(name, "auto" | "strudel" | "reference") {
                    return Err("engine is auto, strudel or reference".into());
                }
                let playing = inner.sched.as_ref().is_some_and(|s| s.playing());
                inner.saved.engine = Some(name.into());
                inner.sched = None;
                Service::ensure(&mut inner, rate);
                if let Some(def) = inner.saved.definition.clone() {
                    let q = inner.saved.quantize;
                    let s = inner.sched.as_mut().unwrap();
                    if let Err(e) = s.load(&def, q, now) {
                        inner.errors = e;
                    }
                    if playing {
                        let bpm = inner.last_bpm;
                        let la = inner.sched.as_ref().unwrap().lookahead;
                        inner
                            .sched
                            .as_mut()
                            .unwrap()
                            .play(now + la, Time::ZERO, bpm);
                    }
                }
                self.save(&inner.saved);
                Ok(self.status(&mut inner, now))
            }
            // The running pattern's events over cycles [from, to), for the
            // inspector and AI comparison (nothing is scheduled).
            "events" => {
                let from = req["from"].as_f64().unwrap_or(0.0);
                let to = req["to"].as_f64().unwrap_or(from + 1.0).min(from + 64.0);
                let s = inner.sched.as_mut().ok_or("nothing loaded")?;
                let span = Span::new(Time::approx(from, 1 << 12), Time::approx(to, 1 << 12));
                let ev = s.preview(span).map_err(|e| e.to_string())?;
                // Rows as a person reads them (the surface's list).
                let rows: Vec<Value> = ev
                    .iter()
                    .map(|e| json!({"cycle": e.onset().to_f64(), "target": e.target.label(), "value": e.value.to_string()}))
                    .collect();
                Ok(json!({"events": ev, "rows": rows, "from": from}))
            }
            // What consumers read: timeline events placed after `since`
            // (graph sample), with the clock to place them by.
            "timeline" => {
                let since = req["since"].as_i64().unwrap_or(i64::MIN);
                let only_visual = req["visual"].as_bool().unwrap_or(false);
                let events: Vec<&Timed> = inner
                    .timeline
                    .iter()
                    .filter(|t| t.end > since && (!only_visual || t.event.target.visual()))
                    .collect();
                let clock = inner.sched.as_ref().map(|s| {
                    json!({"rate": s.tempo.rate, "cycle_beats": s.tempo.cycle_beats, "anchors": s.tempo.anchors(), "playing": s.playing()})
                });
                Ok(json!({"now": now, "clock": clock, "events": events}))
            }
            // The AI director's tools, on Patch programs (never engine code). The
            // running program is `a` unless the request gives one.
            "describe" => {
                let p = program_arg(&req["program"])
                    .or_else(|| current_program(&inner.saved))
                    .ok_or(NO_PROGRAM)?;
                Ok(json!(patch_pattern::ai::describe(&p)))
            }
            "diff" | "compare" => {
                let a = program_arg(&req["a"])
                    .or_else(|| current_program(&inner.saved))
                    .ok_or(NO_PROGRAM)?;
                let b = program_arg(&req["b"]).ok_or("give the program to compare with as 'b'")?;
                if action == "diff" {
                    let changes = patch_pattern::ai::diff(&a, &b);
                    let words: Vec<String> = changes
                        .iter()
                        .map(patch_pattern::ai::explain_change)
                        .collect();
                    Ok(json!({"changes": changes, "explain": words}))
                } else {
                    let cycles = req["cycles"].as_i64().unwrap_or(8);
                    Ok(json!(patch_pattern::ai::compare(&a, &b, cycles)))
                }
            }
            // Edit the running program at a path; with "load": true the
            // result replaces it (quantised, with the request's origin).
            "mutate" => {
                let a = program_arg(&req["program"])
                    .or_else(|| current_program(&inner.saved))
                    .ok_or(NO_PROGRAM)?;
                let at: patch_pattern::ai::At =
                    serde_json::from_value(req["at"].clone()).map_err(|e| format!("at: {e}"))?;
                let edit: patch_pattern::ai::Edit = serde_json::from_value(req["edit"].clone())
                    .map_err(|e| format!("edit: {e}"))?;
                let b = patch_pattern::ai::mutate(&a, &at, &edit)?;
                let explain = patch_pattern::ai::explain(&a, &b);
                if !req["load"].as_bool().unwrap_or(false) {
                    return Ok(json!({"program": b, "explain": explain}));
                }
                drop(inner);
                let mut load = json!({"action": "load", "definition": {"kind": "program", "program": b}, "origin": req["origin"]});
                if !req["quantize"].is_null() {
                    load["quantize"] = req["quantize"].clone();
                }
                let mut r = self.request(host, &load)?;
                r["explain"] = json!(explain);
                Ok(r)
            }
            _ => Err(format!("pattern has no action '{action}'")),
        }
    }

    fn status(&self, inner: &mut Inner, now: i64) -> Value {
        let sched = inner.sched.as_ref();
        let (channels, lets) = tidal_session(&inner.saved);
        json!({
            "engine": inner.engine_name,
            "engine_error": inner.engine_error,
            "capabilities": sched.map(|s| s.engine().capabilities()),
            "status": sched.map(|s| s.status(now)),
            "metrics": sched.map(|s| &s.metrics),
            "delivery": inner.delivery,
            "errors": inner.errors,
            "warnings": inner.warnings,
            "channels": channels,
            // The code an event's `locs` count in: channel i's code is
            // source i, then each let's (`tidal::Session::sources`).
            "lets": lets,
            "layers": inner.layers,
            "definition": inner.saved.definition,
            "source": inner.saved.source,
            "quantize": inner.saved.quantize,
            "origin": inner.saved.origin,
            "tempo": inner.saved.tempo,
        })
    }

    /// One tick of the scheduler thread.
    pub fn tick(&self, host: &dyn Host) {
        let Some((now, _rate)) = host.now() else {
            return;
        };
        let mut inner = self.inner.lock().unwrap();
        let tempo = inner.saved.tempo.clone();
        let origin = inner.saved.origin.unwrap_or(Origin::User);
        let Some(sched) = inner.sched.as_mut() else {
            return;
        };
        if !sched.playing() {
            return;
        }
        if let Tempo::Follow { deck } = tempo
            && let Some((sample, beat, bpm)) = host.deck_clock(deck)
            && bpm > 20.0
        {
            let cb = sched.tempo.cycle_beats.to_f64();
            // Re-anchor beyond 1 ms of drift (or a tempo change).
            let tol = sched.tempo.rate as i64 / 1000;
            sched.follow(sample, beat / cb, bpm, tol);
            inner.last_bpm = bpm;
        }
        let sched = inner.sched.as_mut().unwrap();
        let prepared = sched.prepare(now);
        let rate = sched.tempo.rate.max(1) as f64;
        let tempo_map = sched.tempo.clone();
        for s in prepared {
            let cycle = s.event.onset();
            if s.event.target == Target::Instrument {
                let secs = (s.end - s.sample).max(0) as f64 / rate;
                let cps = tempo_map.bpm_at(cycle) / 60.0 / tempo_map.cycle_beats.to_f64().max(1e-6);
                match patch_pattern::instrument::play_with(&s.event, secs, cps, &HostSamples(host)) {
                    Ok(play) => {
                        // SAFETY: the scheduler thread is the queue's only producer.
                        let ok = unsafe { host.voices().push(RtEvent {
                            sample: s.sample,
                            kind: RtKind::Play,
                            index: play.sound,
                            value: play.level,
                            epoch: 0,
                            params: play.params,
                        }) };
                        if ok {
                            inner.delivery.played += 1;
                        } else {
                            inner.delivery.voices_dropped += 1;
                        }
                    }
                    Err(e) => {
                        inner.delivery.unplayable += 1;
                        inner.delivery.last_unplayable = Some(e);
                    }
                }
                continue;
            }
            match route(&s.event.target, &s.event.value) {
                Action::Voice { voice, velocity } => {
                    let v = s
                        .event
                        .param("gain")
                        .or(s.event.param("velocity"))
                        .map_or(velocity, |g| g as f32);
                    // SAFETY: the scheduler thread is the voices' only producer.
                    let ok = unsafe { host.voices().push(RtEvent {
                        sample: s.sample,
                        kind: RtKind::Voice,
                        index: voice as u32,
                        value: v,
                        epoch: 0,
                        ..Default::default()
                    }) };
                    if ok {
                        inner.delivery.voices += 1;
                    } else {
                        inner.delivery.voices_dropped += 1;
                    }
                }
                Action::Command(c) => inner.commands.push_back((s.sample, c)),
                Action::Nothing => {}
                Action::Sample { source, slot, gain } => {
                    // Derived material rides the same queue to the sampler.
                    let gain = s.event.param("gain").map_or(gain, |g| g as f32);
                    match host.sample_slot(&source, slot.as_deref(), s.sample) {
                        Ok(index) => {
                            // SAFETY: the scheduler thread is the voices' only producer.
                            let ok = unsafe { host.voices().push(RtEvent {
                                sample: s.sample,
                                kind: RtKind::Sample,
                                index,
                                value: gain,
                                epoch: 0,
                                ..Default::default()
                            }) };
                            if ok {
                                inner.delivery.samples += 1;
                            } else {
                                inner.delivery.voices_dropped += 1;
                            }
                        }
                        Err(e) => {
                            inner.delivery.samples_skipped += 1;
                            inner.delivery.last_sample_error = Some(e);
                        }
                    }
                }
                _ => {
                    inner.delivery.timeline += 1;
                    push_timeline(&mut inner, s.sample, s.end, cycle, s.event);
                }
            }
        }
        // Commands due within half a tick run now; they were prepared ahead.
        let rate = inner.sched.as_ref().map_or(48_000, |s| s.tempo.rate) as i64;
        let due_by = now + rate / 200;
        while inner.commands.front().is_some_and(|(at, _)| *at <= due_by) {
            let (at, cmd) = inner.commands.pop_front().unwrap();
            let late_ms = (now - at).max(0) as f64 * 1000.0 / rate as f64;
            drop(inner);
            let r = host.command(cmd, origin);
            inner = self.inner.lock().unwrap();
            inner.delivery.command_late_last_ms = late_ms;
            inner.delivery.command_late_max_ms = inner.delivery.command_late_max_ms.max(late_ms);
            match r {
                Ok(()) => inner.delivery.commands += 1,
                Err(e) => {
                    inner.delivery.rejected += 1;
                    inner.delivery.last_rejection = Some(e);
                }
            }
        }
        // Keep the last few seconds behind now.
        let keep_from = now - rate * 4;
        while inner.timeline.front().is_some_and(|t| t.end < keep_from) {
            inner.timeline.pop_front();
        }
    }

    /// The saved definition, loaded (not played) at service start.
    pub fn restore(&self, host: &dyn Host) {
        let mut inner = self.inner.lock().unwrap();
        let Some(def) = inner.saved.definition.clone() else {
            return;
        };
        if inner.unit == Unit::Tidal
            && let Definition::Source { code, .. } = &def
        {
            host.warm_samples(code, false);
        }
        let (now, rate) = host.now().unwrap_or((0, 48_000));
        Service::ensure(&mut inner, rate);
        let q = inner.saved.quantize;
        match inner.sched.as_mut().unwrap().load(&def, q, now) {
            Ok((loaded, _)) => inner.layers = loaded.layers,
            Err(e) => inner.errors = e,
        }
    }

    /// Whether the thread should tick fast (a pattern plays).
    pub fn playing(&self) -> bool {
        self.inner
            .lock()
            .unwrap()
            .sched
            .as_ref()
            .is_some_and(|s| s.playing())
    }
}

/// The Tidal session, for the surface: its channels (name, code, muted,
/// soloed) and its lets (name, code).
fn tidal_session(saved: &Saved) -> (Value, Value) {
    let Some(Definition::Source { language, code, .. }) = &saved.definition else {
        return (json!([]), json!([]));
    };
    if language != "tidal" {
        return (json!([]), json!([]));
    }
    let Ok(s) = tidal::Session::parse(code) else {
        return (json!([]), json!([]));
    };
    let soloing = !s.soloed.is_empty();
    let channels = s
        .channels
        .iter()
        .map(|(name, code)| json!({
            "name": name,
            "code": code,
            "muted": s.muted.contains(name),
            "soloed": s.soloed.contains(name),
            "silenced": soloing && !s.soloed.contains(name),
        }))
        .collect::<Vec<_>>();
    let lets = s.lets.iter().map(|(name, code)| json!({"name": name, "code": code})).collect::<Vec<_>>();
    (json!(channels), json!(lets))
}

const NO_PROGRAM: &str =
    "no Patch program: load one (AI tools work on Patch programs, not Strudel code)";

fn program_arg(v: &Value) -> Option<patch_pattern::Program> {
    serde_json::from_value(v.clone()).ok()
}

/// The running definition as a Patch program, when it is one (Patch code
/// parses; Strudel code is that engine's and stays opaque).
fn current_program(saved: &Saved) -> Option<patch_pattern::Program> {
    match saved.definition.as_ref()? {
        Definition::Program { program } => Some(program.clone()),
        Definition::Source {
            language,
            code,
            cycle_beats,
        } if language == "patch" => {
            let mut p = patch_pattern::dsl::parse(code).ok()?;
            if !code.lines().any(|l| l.trim_start().starts_with("cycle")) {
                p.cycle_beats = *cycle_beats;
            }
            Some(p)
        }
        Definition::Source { .. } => None,
    }
}

fn push_timeline(inner: &mut Inner, sample: i64, end: i64, cycle: Time, event: Event) {
    if inner.timeline.len() >= TIMELINE {
        inner.timeline.pop_front();
    }
    inner.timeline.push_back(Timed {
        sample,
        end,
        cycle,
        event,
    });
}

/// The service thread: ticks every 10 ms while a pattern plays, slowly
/// otherwise.
pub fn run(service: std::sync::Arc<Service>, host: std::sync::Arc<dyn Host>) {
    service.restore(host.as_ref());
    loop {
        let playing = service.playing();
        if playing {
            service.tick(host.as_ref());
        }
        std::thread::sleep(std::time::Duration::from_millis(if playing {
            10
        } else {
            100
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

    struct Fake {
        now: AtomicI64,
        voices: Queue,
        commands: Mutex<Vec<(i64, Command, Origin)>>,
        deck: Mutex<Option<(i64, f64, f64)>>,
    }

    impl Host for Fake {
        fn now(&self) -> Option<(i64, u32)> {
            Some((self.now.load(Ordering::Relaxed), 48_000))
        }
        fn deck_clock(&self, _: Option<usize>) -> Option<(i64, f64, f64)> {
            *self.deck.lock().unwrap()
        }
        fn command(&self, cmd: Command, origin: Origin) -> Result<(), String> {
            let cmd =
                patch_core::command::validate(cmd, origin, &Default::default()).map_err(|e| e.0)?;
            self.commands
                .lock()
                .unwrap()
                .push((self.now.load(Ordering::Relaxed), cmd, origin));
            Ok(())
        }
        fn voices(&self) -> &Queue {
            &self.voices
        }
    }

    fn fake() -> Fake {
        Fake {
            now: AtomicI64::new(0),
            voices: Queue::new(4096),
            commands: Mutex::new(vec![]),
            deck: Mutex::new(None),
        }
    }

    fn service() -> (Service, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "patch-pattern-svc-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("pattern.json");
        let mut s = Service::new(p.clone());
        s.inner.get_mut().unwrap().saved.engine = Some("reference".into());
        (s, p)
    }

    fn rand_suffix() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }

    #[test]
    fn transitions_start_where_it_plays_and_settle_on_a_restart() {
        let dir = std::env::temp_dir().join(format!("patch-tidal-tr-{}-{}", std::process::id(), rand_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = Service::tidal(dir.join("tidal.json"));
        let h = fake();
        s.request(&h, &json!({"action": "eval", "code": "d1 $ s \"bd*4\""})).unwrap();
        run_for(&s, &h, 4.0);
        let r = s.request(&h, &json!({"action": "eval", "code": "xfadeIn 1 2 $ s \"sn*4\"\nonce $ s \"cp\""})).unwrap();
        assert_eq!(r["ok"], true, "{r}");
        let code = r["code"].as_str().unwrap();
        // About 2.25 cycles in (4 s at 0.5625 cycles a second).
        let t = tidal::transition(code.lines().find(|l| l.starts_with("d1")).unwrap().trim_start_matches("d1 $ ")).unwrap();
        assert!((2.0..2.5).contains(&t.start), "{}", t.start);
        assert!(code.contains("p \"once\" $ _once"));
        run_for(&s, &h, 6.0);
        let ev = drain(&h, i64::MAX);
        assert!(ev.iter().any(|e| e.index == 0) && ev.iter().any(|e| e.index == 1), "both during the fade");
        // Stopped and played again from cycle 0: the new code, no fade.
        s.request(&h, &json!({"action": "stop"})).unwrap();
        s.request(&h, &json!({"action": "play"})).unwrap();
        let st = s.request(&h, &json!({"action": "status"})).unwrap();
        assert_eq!(st["channels"].as_array().unwrap().len(), 1);
        assert_eq!(st["channels"][0]["code"], "s \"sn*4\"");
        run_for(&s, &h, 2.0);
        assert!(drain(&h, i64::MAX).iter().all(|e| e.index == 1), "only the snare");
        // Stopped, mute, solo and silence change the session, never start it.
        s.request(&h, &json!({"action": "stop"})).unwrap();
        for code in ["mute 1", "unmute 1", "solo 1", "unsolo 1", "d2 silence"] {
            let r = s.request(&h, &json!({"action": "eval", "code": code})).unwrap();
            assert_eq!(r["ok"], true, "{code}: {r}");
            assert!(!s.playing(), "{code} started it");
        }
        s.request(&h, &json!({"action": "eval", "code": "d1 $ s \"cp\""})).unwrap();
        assert!(s.playing(), "new code starts it");
    }

    fn run_for(s: &Service, h: &Fake, seconds: f64) {
        let end = h.now.load(Ordering::Relaxed) + (seconds * 48_000.0) as i64;
        while h.now.load(Ordering::Relaxed) < end {
            h.now.fetch_add(480, Ordering::Relaxed);
            s.tick(h);
        }
    }

    fn drain(h: &Fake, end: i64) -> Vec<RtEvent> {
        let mut out = vec![];
        unsafe { h.voices.drain_before(end, |e| out.push(e)) };
        out
    }

    #[test]
    fn tidal_blocks_change_only_their_channels_and_play_the_instrument() {
        let dir = std::env::temp_dir().join(format!("patch-tidal-svc-{}-{}", std::process::id(), rand_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tidal.json");
        let s = Service::tidal(path.clone());
        let h = fake();
        // Evaluating starts it, at Tidal's own tempo, no transport needed.
        let r = s.request(&h, &json!({"action": "eval", "code": "d1 $ s \"bd*4\"\nd2 $ n \"0 4 7\" # s \"superpiano\""})).unwrap();
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r["touched"], json!(["d1", "d2"]));
        assert!(s.playing());
        run_for(&s, &h, 2.0);
        let ev = drain(&h, i64::MAX);
        assert!(ev.iter().all(|e| e.kind == RtKind::Play));
        let drums = ev.iter().filter(|e| e.index == 0).count();
        let piano: Vec<&RtEvent> = ev.iter().filter(|e| e.index == patch_pattern::instrument::SYNTH_BASE).collect();
        assert!(drums >= 4, "{drums}");
        assert!(!piano.is_empty());
        assert!(piano.iter().any(|e| e.params.note == 64.0));
        // 135 BPM: a cycle is 4 beats, 1.777… s; bd*4 a beat apart.
        let gap = ev.iter().filter(|e| e.index == 0).map(|e| e.sample).collect::<Vec<_>>();
        assert_eq!(gap[1] - gap[0], (48_000.0 * 60.0 / 135.0_f64).round() as i64);
        // A block naming d2 leaves d1 as it was; an error changes nothing.
        let r = s.request(&h, &json!({"action": "eval", "code": "d2 silence"})).unwrap();
        assert_eq!(r["touched"], json!(["d2"]));
        let r = s.request(&h, &json!({"action": "eval", "code": "d1 $ s \"bd\" # gian 1"})).unwrap();
        assert_eq!(r["ok"], false);
        assert_eq!(r["errors"][0]["line"], 1);
        let st = s.request(&h, &json!({"action": "status"})).unwrap();
        assert_eq!(st["channels"].as_array().unwrap().len(), 1);
        assert_eq!(st["channels"][0]["code"], "s \"bd*4\"");
        // setcps sets the tempo; the session survives a restart.
        s.request(&h, &json!({"action": "eval", "code": "setcps 0.5"})).unwrap();
        let st = s.request(&h, &json!({"action": "status"})).unwrap();
        assert_eq!(st["tempo"], json!({"mode": "own", "bpm": 120.0}));
        let again = Service::tidal(path);
        let st = again.request(&h, &json!({"action": "status"})).unwrap();
        assert!(st["definition"]["code"].as_str().unwrap().contains("d1 $ s \"bd*4\""));
        // Unknown sounds load (with a warning) and are counted, never played.
        let r = s.request(&h, &json!({"action": "eval", "code": "d3 $ s \"nosuch\""})).unwrap();
        assert!(r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("nosuch")));
        run_for(&s, &h, 2.0);
        let st = s.request(&h, &json!({"action": "status"})).unwrap();
        assert!(st["delivery"]["unplayable"].as_u64().unwrap() > 0);
        assert!(s.request(&h, &json!({"action": "engine", "name": "strudel"})).is_err());
    }

    #[test]
    fn a_pattern_plays_voices_commands_and_visuals_on_the_deck_clock() {
        let (s, path) = service();
        let h = fake();
        let code = "s(\"bd*4\")\ndeck(a).filter(\"<0.2 -0.4>\")\nvisual.intensity(\"<0.2 1>\")";
        let r = s
            .request(&h, &json!({"action":"load","language":"patch","code":code}))
            .unwrap();
        assert_eq!(r["ok"], true, "{r}");
        // Deck A: beat 8 (cycle 2) at sample 0, 120 BPM.
        *h.deck.lock().unwrap() = Some((0, 8.0, 120.0));
        s.request(&h, &json!({"action":"play"})).unwrap();
        run_for(&s, &h, 4.0);
        // Kicks every beat, on the deck's beat: sample 0, 24 000, …
        let mut kicks = vec![];
        unsafe { h.voices.drain_before(i64::MAX, |e| kicks.push(e.sample)) };
        assert_eq!(&kicks[..4], [0, 24_000, 48_000, 72_000]);
        // Filter on each cycle, validated, at most one tick late.
        let cmds = h.commands.lock().unwrap();
        assert!(cmds.len() >= 2, "{cmds:?}");
        assert!(cmds.iter().all(|(_, _, o)| *o == Origin::User));
        let st = s.request(&h, &json!({"action":"status"})).unwrap();
        assert!(
            st["delivery"]["command_late_max_ms"].as_f64().unwrap() <= 10.0,
            "{st}"
        );
        // Visual events wait on the timeline with their sample and cycle.
        let tl = s
            .request(&h, &json!({"action":"timeline","visual":true}))
            .unwrap();
        let ev = tl["events"].as_array().unwrap();
        assert!(!ev.is_empty());
        assert_eq!(ev[0]["cycle"], "2", "cycle 2 is where the deck was");
        // Saved: a new service restores the definition.
        drop(cmds);
        let again = Service::new(path);
        assert!(again.inner.lock().unwrap().saved.definition.is_some());
    }

    #[test]
    fn patch_code_reaches_every_engine_as_a_program() {
        let (s, _) = service();
        let h = fake();
        let r = s
            .request(
                &h,
                &json!({"action":"load","language":"patch","code":"s(\"bd*4\")"}),
            )
            .unwrap();
        assert_eq!(r["ok"], true, "{r}");
        let st = s.request(&h, &json!({"action":"status"})).unwrap();
        assert_eq!(st["definition"]["kind"], "program");
        assert_eq!(
            st["source"]["code"], "s(\"bd*4\")",
            "the editor keeps the code"
        );
        // With Strudel installed, the same load runs there too.
        if patch_pattern::strudel::locate().is_some() {
            s.request(&h, &json!({"action":"engine","name":"strudel"}))
                .unwrap();
            let r = s
                .request(
                    &h,
                    &json!({"action":"load","language":"patch","code":"s(\"bd sd\")"}),
                )
                .unwrap();
            assert_eq!(r["ok"], true, "{r}");
            let st = s.request(&h, &json!({"action":"status"})).unwrap();
            assert_eq!(st["engine"], "strudel", "{st}");
        }
    }

    #[test]
    fn ai_director_describes_mutates_and_loads_the_running_program() {
        let (s, _) = service();
        let h = fake();
        s.request(
            &h,
            &json!({"action":"load","language":"patch","code":"s(\"bd sd ~ sd\")"}),
        )
        .unwrap();
        let d = s.request(&h, &json!({"action":"describe"})).unwrap();
        assert!(d["text"].as_str().unwrap().contains("bd sd ~ sd"), "{d}");
        let layer = d["layers"][0]["name"].as_str().unwrap().to_string();
        let edit = json!({"action":"mutate","at":{"layer":layer,"path":[1]},"edit":{"edit":"wrap","transform":{"fast":"2"}}});
        let r = s.request(&h, &edit).unwrap();
        assert_eq!(
            r["explain"][0],
            format!("{layer}: step 2 now goes through fast(2)")
        );
        let b = r["program"].clone();
        let c = s
            .request(&h, &json!({"action":"compare","b":b,"cycles":4}))
            .unwrap();
        assert_eq!(c["density_a"], 3.0);
        assert_eq!(c["density_b"], 4.0);
        let d = s.request(&h, &json!({"action":"diff","b":b})).unwrap();
        assert_eq!(d["changes"][0]["change"], "wrapped");
        // Loaded by an AI: it runs as a program under the AI's origin.
        let mut load = edit.clone();
        load["load"] = json!(true);
        load["origin"] = json!("ai");
        let r = s.request(&h, &load).unwrap();
        assert_eq!(r["ok"], true, "{r}");
        let st = s.request(&h, &json!({"action":"status"})).unwrap();
        assert_eq!(st["origin"], "ai");
        assert_eq!(st["definition"]["kind"], "program");
        // Strudel code stays opaque to the AI tools.
        s.inner.lock().unwrap().saved.definition = Some(Definition::Source {
            language: "strudel".into(),
            code: "s(\"bd\")".into(),
            cycle_beats: Time::whole(4),
        });
        assert!(
            s.request(&h, &json!({"action":"describe"}))
                .unwrap_err()
                .contains("Strudel")
        );
    }

    #[test]
    fn bad_code_keeps_the_running_pattern_and_ai_needs_programs() {
        let (s, _) = service();
        let h = fake();
        s.request(
            &h,
            &json!({"action":"load","language":"patch","code":"s(\"bd\")"}),
        )
        .unwrap();
        s.request(&h, &json!({"action":"play"})).unwrap();
        let r = s
            .request(
                &h,
                &json!({"action":"load","language":"patch","code":"s(\"bd [\")"}),
            )
            .unwrap();
        assert_eq!(r["ok"], false);
        assert_eq!(r["errors"][0]["line"], 1);
        run_for(&s, &h, 2.5);
        let mut n = 0;
        unsafe { h.voices.drain_before(i64::MAX, |_| n += 1) };
        assert!(n >= 2, "still playing");
        assert!(
            s.request(
                &h,
                &json!({"action":"load","language":"strudel","code":"s(\"bd\")","origin":"ai"})
            )
            .is_err()
        );
        let program = patch_pattern::dsl::parse("deck(a).volume(\"1 0\")").unwrap();
        let r = s
            .request(&h, &json!({"action":"load","definition":{"kind":"program","program":program},"origin":"ai"}))
            .unwrap();
        assert_eq!(r["ok"], true);
    }
}
