//! AI DJ windows: poll the service's `ai_state`, draw the field, say it in
//! words. The service decides; this only shows (ADR-007).
//!
//! `PATCH_AIVIS_FIXTURE=<name|path>` shows a fixture instead (demo mode,
//! always labelled DEMO); without a live engine the windows show the
//! `three_candidates` fixture, also labelled.

use crate::aivis::{self, Moment, Palette, Quality, View};
use patch_core::aivis::{Mode, Phase, VisualState};
use std::cell::{Cell, RefCell};

const FIXTURES: [(&str, &str); 8] = [
    (
        "observing",
        include_str!("../fixtures/aivis/observing.json"),
    ),
    (
        "three_candidates",
        include_str!("../fixtures/aivis/three_candidates.json"),
    ),
    (
        "low_confidence",
        include_str!("../fixtures/aivis/low_confidence.json"),
    ),
    (
        "transition_ready",
        include_str!("../fixtures/aivis/transition_ready.json"),
    ),
    (
        "transition_active",
        include_str!("../fixtures/aivis/transition_active.json"),
    ),
    (
        "crowd_energy_high",
        include_str!("../fixtures/aivis/crowd_energy_high.json"),
    ),
    (
        "ai_unavailable",
        include_str!("../fixtures/aivis/ai_unavailable.json"),
    ),
    (
        "human_override",
        include_str!("../fixtures/aivis/human_override.json"),
    ),
];

fn evidence_label(value: f32) -> &'static str {
    if value < 0.45 {
        "limited evidence · uncalibrated"
    } else if value < 0.72 {
        "partial evidence · uncalibrated"
    } else {
        "strong evidence · uncalibrated"
    }
}

/// A fixture by name (built in) or path.
pub fn fixture(name: &str) -> Option<VisualState> {
    let text = FIXTURES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, t)| t.to_string())
        .or_else(|| std::fs::read_to_string(name).ok())?;
    serde_json::from_str(&text).ok()
}

#[derive(Default)]
pub struct Panel {
    state: RefCell<Option<VisualState>>,
    mission: RefCell<crate::mission::Tracker>,
    mission_plan: RefCell<serde_json::Value>,
    polls: crate::poll::Poller,
    /// Where it came from: None = the service; Some(name) = a fixture.
    demo: RefCell<Option<String>>,
    offline: Cell<bool>,
    /// When `ai_state` last answered: one late reply (the engine busy with
    /// analysis) must not flip the views to "no service" and back.
    last_ok: Cell<Option<f64>>,
    last_poll: Cell<f64>,
    last_text: Cell<f64>,
    /// When each window's field was last drawn, and at what size.
    drawn: RefCell<std::collections::HashMap<String, (f64, usize, usize)>>,
    delay_ms: Cell<f32>,
    /// Frame time budget: slow frames step the quality down (visuals
    /// degrade, audio never waits).
    quality: Cell<u8>,
    slow_frames: Cell<u32>,
    /// The last transition that played (for the report), and whether it
    /// was reported; polled once a second.
    previous: RefCell<(String, String)>,
    /// The transition planned next: "from → to · technique" (its timing is
    /// added from the plan as it's drawn).
    coming: RefCell<String>,
    /// The planned transition's incoming title and technique, apart.
    coming_parts: RefCell<(String, String)>,
    previous_id: RefCell<String>,
    last_previous: Cell<f64>,
}

fn now_ns() -> u64 {
    patch_engine::now_ns()
}

impl Panel {
    /// Nothing playing and no plan forming (the last state seen).
    fn quiet(&self) -> bool {
        self.state.borrow().as_ref().is_none_or(|s| {
            !s.sources.iter().any(|x| x.playing)
                && matches!(s.phase, Phase::Idle | Phase::Listening | Phase::Unavailable)
        })
    }

    pub fn set_delay_ms(&self, ms: f32) {
        self.delay_ms.set(ms.clamp(0.0, 2500.0));
    }

    fn load_demo(&self, name: &str, frozen: bool) {
        if self.demo.borrow().as_deref() == Some(name) {
            return;
        }
        if let Some(mut s) = fixture(name) {
            // A running demo: the fixture's beat is "now" (frozen shots
            // keep the fixture's own stamp: deterministic).
            if !frozen {
                s.clock.nsec = now_ns();
            }
            s.origin = format!(
                "demo:{}",
                name.rsplit('/')
                    .next()
                    .unwrap_or(name)
                    .trim_end_matches(".json")
            );
            *self.state.borrow_mut() = Some(s);
            *self.demo.borrow_mut() = Some(name.to_string());
        }
    }

    /// Per frame, for the open AI DJ windows.
    pub fn tick(&self, wins: &[crate::ui::PatchWindow], t: f64, live: bool, frozen: bool) {
        use slint::ComponentHandle;
        // No AI window in sight: keep following the AI (2 Hz, nothing drawn)
        // so the deck timeline's checkpoints (prepared, 8-bar check) are
        // recorded when they happen. Stopped, a deck brought back into view
        // showed them late or not at all.
        let hidden = wins.is_empty();
        // The Mini Booth shows the AI's words only (refreshed 5×/s): it
        // alone in sight doesn't need the 20 Hz the field and timelines do.
        let words_only = wins.iter().all(|w| w.get_surface() == "dj-mini");
        let env_fixture = std::env::var("PATCH_AIVIS_FIXTURE")
            .ok()
            .filter(|s| !s.is_empty());
        if let Some(name) = env_fixture.as_deref() {
            self.load_demo(name, frozen);
        } else if !live || frozen {
            self.load_demo("three_candidates", frozen);
        } else if t - self.last_poll.get()
            >= if hidden {
                0.5
            } else if self.quiet() || words_only {
                0.2
            } else {
                0.05
            }
        {
            // 20 Hz while music plays or a plan forms (the beat is
            // interpolated between); 5 Hz when nothing happens.
            self.last_poll.set(t);
            self.demo.replace(None);
            self.polls
                .request("ai_state", serde_json::json!({"op": "ai_state"}));
            self.polls
                .request("mission_plan", serde_json::json!({"op": "plan_state"}));
        }
        if live
            && !frozen
            && env_fixture.is_none()
            && let Some(reply) = self.polls.take("ai_state")
        {
            match reply.result {
                Ok(v) => {
                    self.offline.set(false);
                    self.last_ok.set(Some(t));
                    *self.state.borrow_mut() = serde_json::from_value(v).ok();
                }
                Err(_) => {
                    // Offline only after 2 s without an answer; meanwhile
                    // (and while it reconnects) the last known state stays.
                    self.offline
                        .set(self.last_ok.get().is_none_or(|ok| t - ok > 2.0));
                }
            }
        }
        if let Some(reply) = self.polls.take("mission_plan") {
            *self.mission_plan.borrow_mut() = reply.result.unwrap_or_default();
        }
        if live && !frozen && t - self.last_previous.get() >= 1.0 {
            self.last_previous.set(t);
            self.polls
                .request("previous", serde_json::json!({"op": "previous"}));
            self.polls
                .request("coming", serde_json::json!({"op": "coming"}));
        }
        if live && !frozen {
            if let Some(reply) = self.polls.take("previous") {
                let v = reply.result.unwrap_or(serde_json::Value::Null);
                let text = if v.is_null() {
                    String::new()
                } else {
                    let ago = v["ago_s"]
                        .as_u64()
                        .map(|a| {
                            if a < 60 {
                                format!("{a} s ago")
                            } else {
                                format!("{} min ago", a / 60)
                            }
                        })
                        .unwrap_or_default();
                    format!(
                        "{} → {} · {} · {ago}",
                        v["from"].as_str().unwrap_or(""),
                        v["to"].as_str().unwrap_or(""),
                        v["technique"].as_str().unwrap_or("")
                    )
                };
                *self.previous_id.borrow_mut() = v["id"].as_str().unwrap_or_default().into();
                *self.previous.borrow_mut() =
                    (text, v["reported"].as_str().unwrap_or("").to_string());
            }
            if let Some(reply) = self.polls.take("coming") {
                let v = reply.result.unwrap_or(serde_json::Value::Null);
                *self.coming_parts.borrow_mut() = if v.is_null() {
                    Default::default()
                } else {
                    (
                        v["to"].as_str().unwrap_or("").to_string(),
                        v["technique"].as_str().unwrap_or("").to_string(),
                    )
                };
                *self.coming.borrow_mut() = if v.is_null() {
                    String::new()
                } else {
                    format!(
                        "{} → {} · {}",
                        v["from"].as_str().unwrap_or(""),
                        v["to"].as_str().unwrap_or(""),
                        v["technique"].as_str().unwrap_or("")
                    )
                };
            }
        } else if frozen {
            *self.previous.borrow_mut() = ("Losing It → Tell Me Why · 16-bar intro blend · bass swap · EQ fade out · 2 min ago".into(), String::new());
            *self.coming.borrow_mut() = "Tell Me Why → Lost In My Memories · 24-bar intro blend · bass swap · highs, then mids · noise riser".into();
            *self.coming_parts.borrow_mut() = ("Lost In My Memories".into(), "24-bar intro blend · bass swap · highs, then mids · noise riser".into());
        }
        let unavailable = VisualState {
            phase: Phase::Unavailable,
            origin: "none".into(),
            note: "no Patch service: start it with omarchy-patch engine".into(),
            ..Default::default()
        };
        let st = self.state.borrow();
        let s = st.as_ref().unwrap_or(&unavailable);
        let now = if frozen {
            s.clock.nsec + 60_000_000
        } else {
            now_ns()
        };
        let delay = if self.demo.borrow().is_some() {
            0
        } else {
            (self.delay_ms.get() as f64 * 1e6) as u64
        };
        let m = Moment::at(s, now, delay, t, frozen);
        let q = match self.quality.get() {
            0 => Quality::Full,
            1 => Quality::Reduced,
            _ => Quality::Minimal,
        };
        let text = t - self.last_text.get() >= 0.2 || frozen;
        if text {
            self.last_text.set(t);
        }
        // Frame rate follows what happens: the field only drifts slowly
        // when nothing plays (5 fps is smooth for it); music and running
        // plans at 30 fps, the stage view at 60. A resize draws at once.
        let playing = s.sources.iter().any(|x| x.playing);
        let busy = playing
            || matches!(
                s.phase,
                Phase::Evaluating
                    | Phase::Comparing
                    | Phase::Preparing
                    | Phase::Ready
                    | Phase::Executing
            );
        let (mission_active, events) = self
            .mission
            .borrow_mut()
            .update(s, &self.mission_plan.borrow());
        // Shots of the timeline (`PATCH_SHOT_MISSION`) keep their fixture.
        let staged = patch_host::shot::active() && std::env::var_os("PATCH_SHOT_MISSION").is_some();
        for w in wins {
            let d = w.global::<crate::ui::DjState>();
            if !staged {
                d.set_mission_available(mission_active && !self.offline.get());
                // Replaced only when they changed: a new model every frame
                // made the timeline's repeater rebuild at 60 Hz.
                if !model_is(&d.get_mission_a(), &events[0]) {
                    d.set_mission_a(model(events[0].clone()));
                }
                if !model_is(&d.get_mission_b(), &events[1]) {
                    d.set_mission_b(model(events[1].clone()));
                }
            }
            // The Mini Booth shows the AI's words (Auto or not, live), no
            // field: without them it said "not on Auto" while the DJ
            // Booth said on (the user, 2026-11-16).
            if w.get_surface() == "dj-mini" {
                if text {
                    self.texts(&w.global::<crate::ui::AiDjState>(), s);
                }
                continue;
            }
            if w.get_surface() != "dj" {
                continue;
            }
            let a = w.global::<crate::ui::AiDjState>();
            let th = w.global::<crate::ui::Theme>();
            let rgb = |c: slint::Color| [c.red(), c.green(), c.blue()];
            let pal = Palette {
                bg: rgb(th.get_bg()),
                fg: rgb(th.get_text_bright()),
                dim: rgb(th.get_text_dim()),
                faint: rgb(th.get_text_faint()),
                accent: rgb(th.get_accent()),
                warn: rgb(th.get_warn()),
            };
            let scale = w.window().scale_factor();
            let (fw, fh) = (
                (a.get_field_w() * scale).round().max(1.0) as usize,
                (a.get_field_h() * scale).round().max(1.0) as usize,
            );
            // Internal resolution cap: the field is soft by nature (no hard
            // edges), so the window scales it up smoothly; 900 px drew a
            // third of 1600's pixels for the same look. At rest it only
            // drifts: less still.
            let cap = match q {
                Quality::Full if busy => 900,
                Quality::Full | Quality::Reduced => 700,
                Quality::Minimal => 500,
            };
            let k = (cap as f32 / fw.max(fh) as f32).min(1.0);
            let (rw, rh) = (((fw as f32) * k) as usize, ((fh as f32) * k) as usize);
            let view = if fw < 520 || fh < 220 {
                View::Compact
            } else if a.get_view() == "analysis" {
                View::Analysis
            } else {
                View::Operator
            };
            let every = if !busy { 0.2 } else { 1.0 / 30.0 };
            let key = w.get_surface().to_string();
            let due = frozen
                || self.drawn.borrow().get(&key).is_none_or(|&(at, dw, dh)| {
                    t - at >= every - 0.004 || dw != rw || dh != rh || t < at
                });
            if !due {
                if text {
                    self.texts(&a, s);
                }
                continue;
            }
            self.drawn.borrow_mut().insert(key, (t, rw, rh));
            let t0 = std::time::Instant::now();
            let (px, _) = aivis::render(rw.max(1), rh.max(1), s, m, &pal, view, q);
            let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(
                rw.max(1) as u32,
                rh.max(1) as u32,
            );
            buf.make_mut_bytes().copy_from_slice(&px);
            a.set_field(slint::Image::from_rgba8(buf));
            self.budget(t0.elapsed().as_secs_f64() * 1000.0);
            if text {
                self.texts(&a, s);
            }
        }
    }

    /// Over 6 ms a frame, three frames running: one quality step down;
    /// well under budget for a while: one step back up.
    fn budget(&self, ms: f64) {
        let q = self.quality.get();
        if ms > 6.0 {
            self.slow_frames.set(self.slow_frames.get() + 1);
            if self.slow_frames.get() >= 3 && q < 2 {
                self.quality.set(q + 1);
                self.slow_frames.set(0);
            }
        } else if ms < 1.5 && q > 0 {
            self.slow_frames
                .set(self.slow_frames.get().saturating_sub(1));
            if self.slow_frames.get() == 0 {
                self.quality.set(q - 1);
                self.slow_frames.set(120);
            }
        } else {
            self.slow_frames.set(0);
        }
    }

    fn texts(&self, a: &crate::ui::AiDjState, s: &VisualState) {
        let mode = match s.mode {
            Mode::Observe => "observe",
            Mode::Suggest => "suggest",
            Mode::Prepare => "prepare",
            Mode::Act => "act",
        };
        a.set_mode(mode.into());
        a.set_phase(format!("{:?}", s.phase).to_uppercase().into());
        let (origin, live) = if s.origin.starts_with("demo:") {
            (format!("DEMO · {}", &s.origin[5..]), false)
        } else if self.offline.get() || s.origin == "none" {
            ("NO SERVICE".to_string(), false)
        } else {
            ("LIVE".to_string(), true)
        };
        a.set_origin(origin.into());
        a.set_live(live);
        let lead = s.lead();
        a.set_lead_title(
            lead.map(|l| title(&l.title, &l.artist))
                .unwrap_or_default()
                .into(),
        );
        a.set_lead_meta(
            lead.map(|l| {
                let mut p = vec![l.id.replace("deck-", "deck ").to_uppercase()];
                if let Some(b) = l.bpm {
                    p.push(format!("{b:.1} BPM"));
                }
                if let Some(k) = &l.key {
                    p.push(k.clone());
                }
                p.push(format!("energy {:.0}", l.energy * 10.0));
                p.push(format!("{} left", mmss(l.remaining)));
                p.join(" · ")
            })
            .unwrap_or_default()
            .into(),
        );
        let sel = s.selected();
        a.set_next_title(
            sel.map(|c| title(&c.title, &c.artist))
                .unwrap_or_default()
                .into(),
        );
        a.set_next_meta(sel.map(cand_meta).unwrap_or_default().into());
        a.set_evidence(s.confidence);
        a.set_evidence_text(
            if matches!(s.phase, Phase::Idle | Phase::Unavailable) || s.mode == Mode::Observe {
                String::new()
            } else {
                evidence_label(s.confidence).to_string()
            }
            .into(),
        );
        let ev = &s.evidence;
        a.set_rehearsal_score(ev.rehearsal_score.unwrap_or(-1.0));
        a.set_rehearsal_defects(ev.rehearsal_defects.join(" · ").into());
        a.set_tempo_likely(ev.tempo_likely.unwrap_or(-1.0));
        a.set_key_source(ev.key_source.as_str().into());
        a.set_arrangement(ev.arrangement.as_str().into());
        a.set_arrangement_state(ev.arrangement_state.as_str().into());
        a.set_stems_pair(ev.stems_pair.as_str().into());
        a.set_uncertainty(model(
            s.uncertainty.iter().map(|u| u.as_str().into()).collect(),
        ));
        let cands: Vec<crate::ui::AiCand> = s
            .candidates
            .iter()
            .filter(|c| !c.rejected)
            .enumerate()
            .map(|(i, c)| crate::ui::AiCand {
                rank: i as i32 + 1,
                title: c.title.as_str().into(),
                artist: c.artist.as_str().into(),
                meta: cand_meta(c).into(),
                score: c.score,
                evidence: c.confidence,
                factors: format!(
                    "{} · tempo {:.2} · harmony {:.2} · energy {:.2}",
                    evidence_label(c.confidence),
                    c.factors.tempo,
                    c.factors.harmony,
                    c.factors.energy
                )
                .into(),
                reasons: c.reasons.join(" · ").into(),
                selected: c.selected,
                locked: c.locked,
            })
            .collect();
        a.set_candidates(model(cands));
        match &s.transition {
            Some(tr) => {
                let beat = lead.and_then(|l| l.beat).unwrap_or(0.0);
                let bars_left = ((tr.start_beat - beat) / 4.0).ceil();
                let when = if tr.started {
                    format!("running · step {}/{}", tr.done, tr.total)
                } else if bars_left > 0.0 {
                    format!("in {bars_left:.0} bars")
                } else {
                    "at the next bar".into()
                };
                let mut shape = vec![tr.style.clone(), format!("{} bars", tr.bars)];
                if tr.loops {
                    shape.push("loop".into());
                }
                if tr.effects {
                    shape.push("fx".into());
                }
                let preparation = if tr.started {
                    "transition running".to_string()
                } else if tr.ready {
                    match (tr.incoming_position, tr.cue_seconds) {
                        (Some(pos), Some(cue)) => format!(
                            "{} loaded · cue {} · position {} · waiting",
                            tr.to.to_uppercase(),
                            mmss(cue),
                            mmss(pos)
                        ),
                        (Some(pos), None) => format!(
                            "{} loaded · position {} · waiting",
                            tr.to.to_uppercase(),
                            mmss(pos)
                        ),
                        _ => format!("{} loaded · waiting", tr.to.to_uppercase()),
                    }
                } else {
                    format!(
                        "{} selected · waiting for matching load and cue",
                        tr.to.to_uppercase()
                    )
                };
                a.set_plan_text(format!("{} · {} · {}", shape.join(" "), when, preparation).into());
                a.set_plan_when(when.as_str().into());
                a.set_plan_bars(format!("{} bars", tr.bars).into());
                let coming = self.coming.borrow();
                a.set_coming(if coming.is_empty() {
                    "".into()
                } else {
                    format!("{coming} · {when}").into()
                });
                let (to, how) = &*self.coming_parts.borrow();
                a.set_coming_to(to.as_str().into());
                a.set_coming_how(if how.is_empty() {
                    when.clone().into()
                } else {
                    format!("{how} · {when}").into()
                });
                a.set_plan_progress(if tr.started && tr.total > 0 {
                    tr.done as f32 / tr.total as f32
                } else {
                    0.0
                });
                a.set_plan_ready(tr.ready && !tr.started);
            }
            None => {
                a.set_coming("".into());
                a.set_coming_to("".into());
                a.set_coming_how("".into());
                a.set_plan_text("".into());
                a.set_plan_when("".into());
                a.set_plan_bars("".into());
                a.set_plan_progress(0.0);
                a.set_plan_ready(false);
            }
        }
        a.set_pending(s.mode == Mode::Prepare && s.phase == Phase::Ready);
        let note = if s.note.is_empty() && s.phase == Phase::Idle {
            "listening for music · press play in Light DJ, or start a set (omarchy-patch dj set start 60)"
        } else {
            s.note.as_str()
        };
        a.set_note(note.into());
        {
            let (text, reported) = self.previous.borrow().clone();
            a.set_previous_id(self.previous_id.borrow().clone().into());
            a.set_previous(text.into());
            a.set_previous_reported(reported.into());
        }
        a.set_eval_text(if s.eval_ms > 0.0 {
            format!("{:.1} ms · {:.0} s ago", s.eval_ms, s.eval_age_s).into()
        } else {
            "".into()
        });
        let lat = s.clock.output_latency_ns as f64 / 1e6;
        a.set_timing_text(
            format!(
                "engine clock · heard {:.0} ms later ({}){}",
                if self.demo.borrow().is_some() {
                    lat
                } else {
                    self.delay_ms.get() as f64
                },
                if s.clock.latency_level.is_empty() {
                    "reported"
                } else {
                    &s.clock.latency_level
                },
                lead.map(|l| format!(" · grid {}", evidence_label(l.beat_confidence)))
                    .unwrap_or_default()
            )
            .into(),
        );
        a.set_history(model(
            s.history
                .iter()
                .map(|d| format!("{} · {} · {} ago", d.title, d.style, mmss(d.age_s)).into())
                .collect(),
        ));
        a.set_direction(s.direction.as_str().into());
    }
}

fn model<T: Clone + 'static>(v: Vec<T>) -> slint::ModelRc<T> {
    std::rc::Rc::new(slint::VecModel::from(v)).into()
}

/// The model holds exactly `v` (setting `v` again would change nothing).
fn model_is<T: Clone + PartialEq + 'static>(m: &slint::ModelRc<T>, v: &[T]) -> bool {
    use slint::Model;
    m.row_count() == v.len() && v.iter().enumerate().all(|(i, x)| m.row_data(i).as_ref() == Some(x))
}

fn title(t: &str, artist: &str) -> String {
    if artist.is_empty() {
        t.to_string()
    } else {
        format!("{t} · {artist}")
    }
}

fn mmss(s: f64) -> String {
    let s = s.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn cand_meta(c: &patch_core::aivis::Candidate) -> String {
    let mut p = vec![];
    if let Some(b) = c.bpm {
        p.push(format!("{b:.0}"));
    }
    if let Some(k) = &c.key {
        p.push(k.clone());
    }
    p.push(match c.key_steps {
        0 => "same key".into(),
        n => format!("{n:+} key"),
    });
    p.push(format!("energy {}", crate::num::signed(c.energy_delta * 10.0, 0)));
    p.join(" · ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_fixture_parses_and_is_labelled_demo() {
        for (name, _) in super::FIXTURES {
            let s = super::fixture(name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(s.origin, format!("demo:{name}"));
        }
    }
}
