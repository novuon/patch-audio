//! DJ actions: pitch, EQ knobs, loops, plans, controllers, FX, cue and phones, hot cues.

use super::*;

impl AudioApp {
    /// Read the cue table again when the library has moved under us.
    ///
    /// The store is shared — the engine's cue command, the CLI and this booth all
    /// write it — so a cue set from a controller reaches an open booth through the
    /// library rather than through a message. Without this the booth kept its own
    /// stale copy until something reloaded it, which is why a cue set from hardware
    /// was invisible in an open window.
    pub(super) fn refresh_cues(&self) {
        let stamp = dj::library_stamp();
        if !dj::cues_are_stale(self.frozen, self.cues_stamp.get(), stamp) {
            return;
        }
        self.cues_stamp.set(stamp);
        *self.cues.borrow_mut() = dj::load_cues();
    }

    pub(super) fn deck_index(id: &str) -> usize {
        match id { "b" => 1, "c" => 2, "d" => 3, _ => 0 }
    }

    /// Prefer the explicit tempo master, then the opposite side, then a loaded deck.
    pub(super) fn sync_reference(&self, i: usize) -> usize {
        let master = self.club_master.get();
        let opposite = if i == 0 || i == 2 { 1 } else { 0 };
        [master, opposite, 0, 1, 2, 3].into_iter()
            .find(|&d| d != i && self.decks[d].snapshot().loaded)
            .unwrap_or(opposite)
    }

    /// Change a deck's tempo/key and send it to the player.
    pub(super) fn with_pitch(&self, id: &str, f: impl FnOnce(&mut dj::Pitch)) {
        let i = Self::deck_index(id);
        let p = {
            let mut all = self.pitch.borrow_mut();
            f(&mut all[i]);
            all[i]
        };
        if self.is_live() {
            self.decks[i].set_pitch(&p);
        }
        if p.bend == 0.0 && !self.frozen {
            save_pitch(&self.pitch.borrow());
        }
        self.poll_dj();
    }

    /// Set one EQ knob (0 high, 1 mid, 2 low, 3 filter) and send the deck's
    /// EQ to its engine channel.
    /// EQ knobs live in the service's DJ session: set one there (it applies
    /// the band gains) and show it here at once.
    pub(super) fn set_eq_knob(&self, deck: usize, slot: usize, v: f32) {
        let mut all = self.eq.get();
        all[deck][slot] = v.clamp(-1.0, 1.0);
        self.eq.set(all);
        self.show_eq();
        self.dj_changed.set(Some(Instant::now()));
        if self.is_live() {
            use patch_core::command::{Band, Deck as D};
            let d = [D::A, D::B, D::C, D::D][deck];
            let cmd = match slot {
                0 => Command::DeckEqKnob {
                    deck: d,
                    band: Band::High,
                    value: all[deck][0],
                },
                1 => Command::DeckEqKnob {
                    deck: d,
                    band: Band::Mid,
                    value: all[deck][1],
                },
                2 => Command::DeckEqKnob {
                    deck: d,
                    band: Band::Low,
                    value: all[deck][2],
                },
                _ => Command::DeckFilter {
                    deck: d,
                    amount: all[deck][3],
                },
            };
            let req = serde_json::json!({"op": "command", "command": serde_json::from_str::<serde_json::Value>(&cmd.to_json()).unwrap_or_default(), "origin": "user"});
            // Off this thread like the mixer: per knob, newest value first.
            const KNOBS: [[&str; 4]; 4] = [
                ["eq-a-high", "eq-a-mid", "eq-a-low", "eq-a-filter"],
                ["eq-b-high", "eq-b-mid", "eq-b-low", "eq-b-filter"],
                ["eq-c-high", "eq-c-mid", "eq-c-low", "eq-c-filter"],
                ["eq-d-high", "eq-d-mid", "eq-d-low", "eq-d-filter"],
            ];
            if !self
                .mixer_sends
                .request(KNOBS[deck.min(3)][slot.min(3)], req.clone())
            {
                let _ = patch_engine::client::call(&req);
            }
        }
        self.poll_dj();
    }

    pub(super) fn set_input_trim(&self, deck: usize, value: f32) {
        let db = value.clamp(-1.0, 1.0) * 24.0;
        let mut trims = self.input_trims.get();
        trims[deck] = db;
        self.input_trims.set(trims);
        for w in self.dj_windows() {
            w.global::<DjState>().set_input_trims(ModelRc::new(VecModel::from(trims.to_vec())));
        }
        self.dj_changed.set(Some(Instant::now()));
        if self.is_live() {
            let channel = ["deck-a", "deck-b", "deck-c", "deck-d"][deck];
            let req = serde_json::json!({"op":"trim", "channel":channel, "db":db});
            if !self.mixer_sends.request(["trim-a", "trim-b", "trim-c", "trim-d"][deck], req.clone()) {
                let _ = patch_engine::client::call(&req);
            }
        }
    }

    /// Engage a loop of `beats`, or leave it when that loop is on.
    pub(super) fn loop_toggle(&self, id: &str, beats: f64) -> Result<(), String> {
        let st = self.deck(id).status();
        if st.loop_on && (beats <= 0.0 || (self.deck_loop_beats(id) - beats).abs() < 0.01) {
            self.deck(id)
                .deck_action("loop_exit", serde_json::json!({}))
        } else {
            self.deck(id)
                .deck_action("loop", serde_json::json!({"beats": if beats > 0.0 { beats } else { 4.0 }}))
        }
    }

    /// Beat position of a deck on its grid (beats since the first downbeat),
    /// as heard (engine stamp).
    pub(super) fn deck_beat(&self, i: usize) -> Option<f64> {
        let st = self.decks[i].status();
        // The engine's grid (what sync, FX and quantize use); else the
        // analysis.
        let (bpm, offset, downbeat) = match st.grid {
            Some(grid) => grid,
            None => {
                let a = self.deck_analysis(i, &st)?;
                (
                    a.bpm.filter(|_| a.bpm_confidence >= 0.5)?,
                    a.beat_offset,
                    a.downbeat,
                )
            }
        };
        let secs = self
            .provider
            .borrow()
            .deck_playheads()
            .get(i)
            .copied()
            .flatten()
            .map(|(s, _)| s)
            .unwrap_or(st.time);
        Some((secs - offset) * bpm / 60.0 - downbeat.unwrap_or(0) as f64)
    }

    /// Compile a plan and start it at its bar (or the next bar when that
    /// has passed or it asks for "next", start_bar < 0).
    /// Hand a plan to the service, which runs it on the outgoing deck's
    /// beat clock (it keeps running if every window closes).
    pub(super) fn start_plan(
        &self,
        plan: patch_core::intent::TransitionPlan,
        level: patch_core::intent::ControlLevel,
        origin: Origin,
    ) -> Result<(), String> {
        if !self.is_live() {
            return Err("transitions run on the live engine".into());
        }
        let req = serde_json::json!({"op": "plan", "plan": plan, "level": level, "origin": origin});
        let v = patch_engine::client::call(&req)?;
        *self.dj_status.borrow_mut() = match v["start_bar"].as_f64() {
            Some(b) => format!(
                "{:?} over {} bars from bar {} · {} steps",
                plan.style,
                plan.bars,
                b + 1.0,
                v["steps"]
            ),
            None => "Prepare".into(),
        };
        self.dj_synced.set(None);
        self.dj_changed.set(Some(Instant::now())); // read the plan back at once
        Ok(())
    }

    /// The state an AI DJ visualization (or another frontend) reads: real
    /// data only (engine clock, analysis, plan).
    pub(super) fn ai_view(&self) -> patch_core::intent::AiView {
        use patch_core::intent::{AiView, DeckView, PlanView};
        let mut decks = vec![];
        // Keys (as shifted) of both decks, for compatibility.
        let mut keys = [None, None];
        let mut tension = 0.0f32;
        for i in 0..2 {
            let st = if self.is_live() {
                self.decks[i].snapshot()
            } else if self.frozen {
                dj::DeckStatus::demo(i)
            } else {
                dj::DeckStatus::default()
            };
            let an = if st.loaded {
                self.deck_analysis(i, &st)
            } else {
                None
            };
            let p = self.pitch.borrow()[i];
            let beat = if st.loaded { self.deck_beat(i) } else { None };
            let key = an
                .as_ref()
                .and_then(|a| a.key)
                .map(|k| k.shifted(p.semitones().round() as i32));
            keys[i] = key;
            let bpm = an.as_ref().and_then(|a| a.bpm).map(|b| b * p.speed());
            let second = st.time.max(0.0) as usize;
            let energy_at = |s: usize| {
                an.as_ref()
                    .and_then(|a| a.energy.get(s))
                    .map(|e| *e as f32 / 255.0)
                    .unwrap_or(0.0)
            };
            let playing = st.loaded && !st.paused;
            if playing {
                tension = tension.max((energy_at(second + 8) - energy_at(second)).clamp(-1.0, 1.0));
            }
            decks.push(DeckView {
                deck: self.decks[i].id.to_string(),
                playing,
                beat,
                bar_phase: beat.map(|b| (b / 4.0).rem_euclid(1.0) as f32),
                phrase_phase: beat.map(|b| (b / 32.0).rem_euclid(1.0) as f32),
                bpm,
                key: key.map(|k| k.camelot()),
                energy: energy_at(second),
                energy_ahead: (1..=16).map(|k| energy_at(second + k)).collect(),
                remaining: st.duration.map(|d| (d - st.time).max(0.0)),
            });
        }
        // Harmonic fit (Camelot) × tempo closeness (within 8 %, half/double
        // time included).
        let harmonic = match keys {
            [Some(a), Some(b)] => {
                if a.compatible(b) {
                    1.0
                } else {
                    0.3
                }
            }
            _ => 0.0,
        };
        let tempo = match (decks[0].bpm, decks[1].bpm) {
            (Some(x), Some(y)) => {
                let r = [0.5, 1.0, 2.0]
                    .iter()
                    .map(|m| (x * m / y - 1.0).abs())
                    .fold(f64::MAX, f64::min);
                (1.0 - r / 0.08).clamp(0.0, 1.0) as f32
            }
            _ => 0.0,
        };
        let compatibility = harmonic * tempo;
        let plan = self.plan.borrow().as_ref().map(|p| {
            let (done, total) = (
                p["done"].as_u64().unwrap_or(0) as usize,
                p["total"].as_u64().unwrap_or(1).max(1) as usize,
            );
            PlanView {
                style: p["style"].as_str().unwrap_or("").to_string(),
                progress: done as f32 / total as f32,
                steps_left: total - done,
                confidence: p["confidence"].as_f64().unwrap_or(0.0) as f32,
                rationale: p["rationale"].as_str().unwrap_or("").to_string(),
            }
        });
        let (ai_state, suggestion) = self.ai_summary();
        AiView {
            decks,
            compatibility,
            plan,
            tension,
            ai_state,
            suggestion,
        }
    }

    /// A key (or other input) in a DJ surface: the keymap decides; the
    /// command goes through validation like any other. True when bound.
    pub(super) fn dj_input(&self, context: &str, ev: patch_core::input::InputEvent) -> bool {
        let Some(cmd) = self.keymap.resolve(context, &ev) else {
            return false;
        };
        // The loop key exits or recalls the deck's loop; a deck that has
        // had no loop yet gets a four-beat one, as the key always did.
        let cmd = match cmd {
            Command::DeckReloop { deck }
                if self.decks[Self::deck_index(deck.id())].status().looping.is_none() =>
            {
                Command::DeckLoopToggle { deck, beats: 4.0 }
            }
            cmd => cmd,
        };
        if let Err(e) = self.command(cmd, Origin::User) {
            *self.dj_status.borrow_mut() = e;
        }
        self.poll_dj();
        true
    }

    /// Device input runs in the service, regardless of surface lifetime.
    pub(super) fn controller_tick(&self) {
        if self.frozen || !self.is_live() {
            return;
        }
        let windows: Vec<_> = self
            .windows
            .borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .filter(|w| w.get_surface() == "controllers" && self.in_sight(w))
            .collect();
        if windows.is_empty() {
            return;
        }
        if self
            .controller_poll
            .get()
            .is_none_or(|t| t.elapsed() >= Duration::from_millis(250))
            && self
                .controller_polls
                .request("controllers", serde_json::json!({"op":"controllers"}))
        {
            self.controller_poll.set(Some(Instant::now()));
        }
        if let Some(reply) = self.controller_polls.take("controller-reload") {
            let message = match reply.result {
                Ok(_) => "Reload requested".into(),
                Err(e) => format!("Reload failed · {e}"),
            };
            for w in &windows {
                w.global::<crate::ui::ControllerState>()
                    .set_status(message.as_str().into());
            }
        }
        let Some(reply) = self.controller_polls.take("controllers") else {
            return;
        };
        let result = reply.result.and_then(|v| {
            serde_json::from_value::<patch_controller::Snapshot>(v).map_err(|e| e.to_string())
        });
        let snapshot = match result {
            Ok(snapshot) => snapshot,
            Err(error) => {
                for w in &windows {
                    let c = w.global::<crate::ui::ControllerState>();
                    c.set_status(format!("Controller unavailable · {error}").into());
                    c.set_devices(
                        Rc::new(slint::VecModel::<crate::ui::CtlDevice>::default()).into(),
                    );
                }
                return;
            }
        };
        let connected = snapshot
            .devices
            .iter()
            .filter(|d| d.state == "connected")
            .count();
        let devices: Vec<crate::ui::CtlDevice> = snapshot
            .devices
            .iter()
            .map(|d| {
                // Listed, never opened: the reason, not a row of zeros. A
                // device that isn't connected (refused, an error) shows its
                // state alone: its events and identity say nothing yet.
                let unread = d.state.contains("cannot read yet");
                let connected = d.state == "connected";
                crate::ui::CtlDevice {
                name: if d.name.trim().is_empty() { d.identity.clone() } else { d.name.clone() }.into(),
                transport: d.transport.clone().into(),
                detail: if unread {
                    format!("{} · Patch can't read this device yet", d.path)
                } else if connected {
                    format!("{} · {} events · {}", d.path, d.events, d.identity_strength)
                } else if d.state.contains(d.path.as_str()) {
                    d.state.clone()
                } else {
                    format!("{} · {}", d.path, d.state)
                }
                .into(),
                profile: if unread { String::new() } else { d.profile.clone() }.into(),
                matched: d.matched,
                bindings: d.bindings as i32,
                outputs: d.outputs as i32,
                sent: d.sent as i32,
                dropped: d.dropped as i32,
                }
            })
            .collect();
        let lines: Vec<crate::ui::CtlEvent> = snapshot
            .events
            .iter()
            .map(|e| crate::ui::CtlEvent {
                device: e.device.clone().into(),
                what: e.what.clone().into(),
                action: e.action.clone().into(),
                bound: e.accepted,
            })
            .collect();
        // A device Patch can *see* but not open must not read as "nothing here":
        // an unopenable one is listed in the unit, and the summary has to account
        // for it or the two disagree.
        let seen = snapshot.devices.len();
        let status: slint::SharedString = snapshot
            .config_error
            .clone()
            .map(|e| format!("Profile error · {e}"))
            .unwrap_or_else(|| match (seen, connected) {
                // Short: it is the window's header too, beside the title.
                (0, _) => "no controller".to_string(),
                (1, 0) => "1 device · 0 connected".to_string(),
                (n, 0) => format!("{n} devices · 0 connected"),
                (n, c) if n == c => format!("{c} connected"),
                (n, c) => format!("{n} devices · {c} connected"),
            })
            .into();
        for w in &windows {
            let c = w.global::<crate::ui::ControllerState>();
            c.set_devices(Rc::new(slint::VecModel::from(devices.clone())).into());
            c.set_events(Rc::new(slint::VecModel::from(lines.clone())).into());
            c.set_status(status.clone());
            c.set_keymap(
                patch_controller::config_path()
                    .to_string_lossy()
                    .as_ref()
                    .into(),
            );
        }
    }

    pub(super) fn bind_controllers(&self, w: &PatchWindow) {
        let weak = self.me.clone();
        w.global::<crate::ui::ControllerState>().on_reload(move || {
            if let Some(app) = weak.upgrade() {
                app.controller_polls.request(
                    "controller-reload",
                    serde_json::json!({"op":"controllers_reload"}),
                );
                app.controller_poll.set(None);
            }
        });
    }

    /// Beat FX: update the unit and the engine. Aiming it at the other deck
    /// switches the old one off (its echo or reverb tail rings out).
    pub(super) fn set_fx(
        &self,
        deck: usize,
        kind: Option<patch_core::command::FxKind>,
        on: Option<bool>,
        beats: Option<f32>,
        depth: Option<f32>,
    ) -> Result<(), String> {
        let mut u = self.fx.get();
        if let Some(k) = kind {
            if k != u.kind {
                // A new effect starts from its usual length unless told.
                u.beats = k.default_beats();
            }
            // One effect: a chain it was part of is gone.
            u.kind = k;
            u.chain = [None; 2];
        }
        if let Some(o) = on {
            u.on = o;
        }
        self.apply_fx(deck, u, beats, depth)
    }

    /// Beat FX chain (validated: 1 … 3 effects, each once).
    pub(super) fn set_fx_chain(
        &self,
        deck: usize,
        kinds: &[patch_core::command::FxKind],
        on: bool,
        beats: Option<f32>,
        depth: Option<f32>,
    ) -> Result<(), String> {
        let mut u = self.fx.get();
        let first = *kinds.first().ok_or("an empty chain")?;
        if first != u.kind {
            u.beats = first.default_beats();
        }
        u.kind = first;
        u.chain = [kinds.get(1).copied(), kinds.get(2).copied()];
        u.on = on;
        self.apply_fx(deck, u, beats, depth)
    }

    /// Show `u` and run it on `deck`'s channel. Aiming it at the other deck
    /// switches the old one off (its echo or reverb tail rings out).
    pub(super) fn apply_fx(
        &self,
        deck: usize,
        mut u: FxUnit,
        beats: Option<f32>,
        depth: Option<f32>,
    ) -> Result<(), String> {
        let before = self.fx.get();
        if deck != before.deck && before.on && self.is_live() {
            let old = ["deck-a", "deck-b", "deck-c", "deck-d"][before.deck];
            let _ = patch_engine::client::call(
                &serde_json::json!({"op": "fx", "channel": old, "on": false}),
            );
        }
        self.fx_touched.set(Some(Instant::now()));
        u.deck = deck;
        if let Some(b) = beats {
            u.beats = b;
        }
        if let Some(d) = depth {
            u.depth = d;
        }
        self.fx.set(u);
        if self.is_live() {
            let ch = ["deck-a", "deck-b", "deck-c", "deck-d"][u.deck];
            let chain: Vec<_> = u.kinds().iter().map(|k| k.name()).collect();
            patch_engine::client::call(
                &serde_json::json!({"op": "fx", "channel": ch, "chain": chain, "on": u.on, "beats": u.beats, "depth": u.depth}),
            )?;
        }
        Ok(())
    }

    /// Length of a deck's loop in beats (0 without a loop or grid).
    pub(super) fn deck_loop_beats(&self, id: &str) -> f64 {
        let i = Self::deck_index(id);
        let st = self.decks[i].status();
        match (st.looping, st.grid.map(|g| g.0)) {
            (Some((a, b)), Some(bpm)) => (b - a) * bpm / 60.0,
            _ => 0.0,
        }
    }

    /// Send PFL and the headphone blend/level to the engine.
    pub(super) fn send_cue(&self, request: serde_json::Value) {
        if self.is_live() {
            let _ = patch_engine::client::call(&request);
        }
        self.cue_changed();
    }

    pub(super) fn cue_changed(&self) {
        self.dj_changed.set(Some(Instant::now()));
        self.dj_synced.set(None);
    }

    /// The channel's input selector, one step: its deck → each application
    /// playing now → its deck (a DJM's USB / LINE switch).
    pub(super) fn cycle_line(&self, deck: usize) -> String {
        if !self.is_live() {
            return "LINE inputs need the live engine".into();
        }
        let letter = ["a", "b", "c", "d"][deck.min(3)];
        let now = self.deck_lines.borrow()[deck.min(3)].clone();
        let next = next_line(&self.line_sources.borrow(), &now);
        match patch_engine::client::call(&serde_json::json!({"op": "deck_line", "deck": letter, "app": next})) {
            Ok(v) => {
                self.mirror_dj(&v);
                match next {
                    Some(app) => format!("Channel {}: LINE · {app}", letter.to_uppercase()),
                    None => format!("Channel {}: deck {}", letter.to_uppercase(), letter.to_uppercase()),
                }
            }
            Err(e) => format!("LINE: {e}"),
        }
    }

    /// Next headphone output: none → each output that isn't the program's → none.
    pub(super) fn cycle_phones(&self) -> String {
        if !self.is_live() {
            return "Headphones need the live engine".into();
        }
        let status =
            patch_engine::client::call(&serde_json::json!({"op": "status"})).unwrap_or_default();
        let program = status["output"].as_str().unwrap_or("").to_string();
        let now = status["cue_output"].as_str().map(String::from);
        let outs: Vec<(String, String)> = self
            .provider
            .borrow()
            .devices()
            .into_iter()
            // Live devices are the hardware outputs (sinks).
            .filter(|d| d.id != program)
            .map(|d| (d.id.to_string(), d.name.to_string()))
            .collect();
        let next = match &now {
            None => outs.first().cloned(),
            Some(c) => outs
                .iter()
                .position(|(id, _)| id == c)
                .and_then(|i| outs.get(i + 1))
                .cloned(),
        };
        let _ = patch_engine::client::call(
            &serde_json::json!({"op": "cue_output", "sink": next.as_ref().map(|(id, _)| id.clone())}),
        );
        self.cue_changed();
        *self.phones_name.borrow_mut() = next.as_ref().map(|(_, n)| n.clone());
        match next {
            Some((_, name)) => format!("Headphones on {name}"),
            None => "Headphones off".into(),
        }
    }

    /// A hot cue pad pressed (Club Booth): a set cue on a paused engine deck
    /// plays from there while held, like a CDJ; anything else is `hot_cue`.
    pub(super) fn hot_cue_press(&self, deck: usize, n: usize, clear: bool) {
        let st = self.decks[deck].status();
        let held = (!clear && st.loaded && st.native && st.paused)
            .then(|| self.cues.borrow().get(&st.source).and_then(|c| c[n]))
            .flatten();
        let Some(t) = held else {
            return self.hot_cue(deck, n, clear);
        };
        self.decks[deck].gate_cue(t);
        *self.dj_status.borrow_mut() =
            format!("{}: hot cue {} · {} (held)", self.decks[deck].name, n + 1, dj::clock(t));
        self.poll_dj();
    }

    /// The pad let go: a held hot cue returns to its point, paused.
    pub(super) fn hot_cue_release(&self, deck: usize) {
        self.decks[deck].cue_release();
        self.poll_dj();
    }

    /// Hot cue `n` of a deck: jump to it, or set it here (snapped to the
    /// beat) when empty; `clear` removes it.
    pub(super) fn hot_cue(&self, deck: usize, n: usize, clear: bool) {
        let st = self.decks[deck].status();
        if !st.loaded || !st.seekable {
            return;
        }
        let mut all = self.cues.borrow_mut();
        let cues = all.entry(st.source.clone()).or_default();
        let name = self.decks[deck].name;
        let msg = if clear {
            cues[n] = None;
            format!("{name}: hot cue {} cleared", n + 1)
        } else if let Some(t) = cues[n] {
            self.decks[deck].seek_abs(t);
            format!("{name}: hot cue {} · {}", n + 1, dj::clock(t))
        } else {
            let t = dj::snap_to_beat(st.time, self.analysis(&st.source).as_ref());
            cues[n] = Some(t);
            format!("{name}: hot cue {} set at {}", n + 1, dj::clock(t))
        };
        if !self.frozen {
            dj::save_cues(&all);
        }
        drop(all);
        if let Ok(db) = patch_library::open_quick()
            && let Some(track) = db.track_by_path(&st.source)
        {
            let mut perf = db.performance(track);
            perf.hot_cues.resize(perf.hot_cues.len().max(n + 1), None);
            perf.hot_cues[n] = if clear {
                None
            } else {
                self.cues.borrow().get(&st.source).and_then(|c| c[n])
            };
            if let Err(e) = db.set_performance(track, &perf) {
                eprintln!("Patch DJ cue save: {e}");
            }
            let (uuid, rate) = db
                .tracks()
                .ok()
                .and_then(|ts| {
                    ts.into_iter()
                        .find(|t| t.id == track)
                        .map(|t| (t.track_uuid, t.rate))
                })
                .unwrap_or_else(|| (track.to_string(), patch_analysis::RATE));
            let cue_id = format!("{uuid}:hot:{}", n + 1);
            if clear {
                let _ = db.delete_cue(&cue_id);
            } else if let Some(t) = self.cues.borrow().get(&st.source).and_then(|c| c[n]) {
                let rate = if rate == 0 {
                    patch_analysis::RATE
                } else {
                    rate
                };
                let cue = patch_library::Cue {
                    id: cue_id,
                    kind: "hot".into(),
                    sample_position: (t * rate as f64).round() as u64,
                    sample_rate: rate,
                    beat_position: None,
                    label: format!("Hot {}", n + 1),
                    color: None,
                    comment: String::new(),
                    origin: "performer".into(),
                    confidence: None,
                    generated: false,
                };
                if let Err(e) = db.put_cue(track, &cue) {
                    eprintln!("Patch DJ cue record: {e}");
                }
            }
        }
        self.log_track_event(
            "cue",
            ["a", "b", "c", "d"][deck],
            &st.source,
            serde_json::json!({"slot":n+1,"cleared":clear}),
        );
        *self.dj_status.borrow_mut() = msg;
        self.poll_dj();
    }
}

/// The input selector's next source after `now` ("" = the deck): the deck
/// → each application in turn → the deck.
fn next_line(sources: &[String], now: &str) -> Option<String> {
    if now.is_empty() {
        return sources.first().cloned();
    }
    sources
        .iter()
        .position(|a| a.eq_ignore_ascii_case(now))
        .and_then(|i| sources.get(i + 1))
        .cloned()
}

#[cfg(test)]
mod line_tests {
    use super::next_line;

    #[test]
    fn the_selector_steps_deck_then_each_app_then_deck() {
        let apps = vec!["Chromium".to_string(), "Spotify".to_string()];
        assert_eq!(next_line(&apps, "").as_deref(), Some("Chromium"));
        assert_eq!(next_line(&apps, "chromium").as_deref(), Some("Spotify"));
        assert_eq!(next_line(&apps, "Spotify"), None);
        // An app that stopped playing: back to the deck.
        assert_eq!(next_line(&apps, "mpv"), None);
        assert_eq!(next_line(&[], ""), None);
    }
}
