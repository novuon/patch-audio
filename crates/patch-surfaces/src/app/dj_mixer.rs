//! The DJ mixer: starting and stopping DJ, engine state, crossfader and EQ readouts, gains, mirroring the engine.

use super::*;

/// Display mode, independent of audio routing. A new C/D load reveals both
/// extra channels; polling the same load must preserve a manual selection.
/// When the last track leaves C and D (an eject, an ended set) the
/// Crossfader goes back to two decks: the user, 2026-11-16, a fresh layout
/// kept showing four empty strips long after C and D were cleared.
#[derive(Default)]
pub(super) struct MixerDecks {
    pub four: bool,
    loads: [Option<(String, Option<u32>)>; 2],
}

impl MixerDecks {
    pub fn observe(&mut self, decks: [&dj::DeckStatus; 2]) {
        let had = self.loads.iter().any(Option::is_some);
        for (previous, deck) in self.loads.iter_mut().zip(decks) {
            let current = deck.loaded.then(|| (deck.source.clone(), deck.load));
            if current.is_some() && *previous != current {
                self.four = true;
            }
            *previous = current;
        }
        if had && self.loads.iter().all(Option::is_none) {
            self.four = false;
        }
    }
}

impl AudioApp {
    /// Start the DJ booth (idempotent): the decks play into their own
    /// virtual soundcards, which the Patch engine mixes in the same graph
    /// cycle, so both decks are sample-synchronous. Nothing already playing
    /// is moved or re-clocked.
    pub(super) fn start_dj(&self) -> Result<(), String> {
        if !self.is_live() {
            return Err("the DJ booth plays real audio only with PipeWire".into());
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        patch_engine::client::ensure_running(&exe)?;
        for d in &self.decks {
            d.attach();
        }
        self.apply_dj_gains();
        self.after_routing();
        Ok(())
    }

    /// Deck channels with live audio (none without the engine or in mock).
    pub(super) fn live_decks(&self) -> Vec<String> {
        if !self.is_live() {
            return vec![];
        }
        patch_engine::client::live_decks()
    }

    /// Stop the decks. Their channels stay in the engine, silent; every other
    /// stream keeps playing where it is.
    pub(super) fn stop_dj(&self) {
        for d in &self.decks {
            d.quit();
        }
        self.after_routing();
    }

    /// Whether the engine runs, and a one-line description of it.
    pub(super) fn engine_state(&self) -> (bool, SharedString) {
        let rate = self.provider.borrow().engine().sample_rate;
        let khz = format!("{:.1} kHz", rate as f32 / 1000.0).replace(".0 ", " ");
        if self.frozen {
            return (
                true,
                format!(
                    "mixing · {} · {khz} · Patch engine",
                    patch_engine::backend()
                )
                .into(),
            );
        }
        if !self.is_live() {
            return (false, "needs PipeWire".into());
        }
        if !self.engine_online.get() {
            return (false, "the Patch engine is not running".into());
        }
        if self.decks.iter().any(|d| d.snapshot().loaded) {
            (
                true,
                format!(
                    "mixing · {} · {khz} · Patch engine",
                    patch_engine::backend()
                )
                .into(),
            )
        } else {
            (false, "stopped · starts when you load a deck".into())
        }
    }

    /// Send the fader positions to the engine's deck channels (it ramps to
    /// them over one block, click-free) and remember them.
    /// Send the DJ mixer (crossfader, faders, auto-gain) to the service,
    /// which owns it and turns it into deck gains; mirror what it returns.
    /// The crossfader's gains as the mixer shows them: "A −3.0 · B −3.0 dB".
    pub(super) fn xfader_text(&self) -> SharedString {
        let (xa, xb) = self.xf_curve.get().gains(self.xfader.get());
        let db = |g: f32| {
            db_text(if g > 0.0 {
                (20.0 * g.log10()).max(-60.0)
            } else {
                -60.0
            })
        };
        if self.decks[2..].iter().any(|d| d.snapshot().loaded) {
            format!("A+C {} · B+D {}", db(xa), db(xb)).into()
        } else { format!("A {} · B {}", db(xa), db(xb)).into() }
    }

    /// The EQ knobs in every DJ window, now (only what changed: this runs
    /// every frame).
    pub(super) fn show_eq(&self) {
        let k = self.eq.get();
        let item = |k: [f32; 4]| EqKnobs {
            high: k[0],
            mid: k[1],
            low: k[2],
            filter: k[3],
            high_text: eq_text(k[0]).into(),
            mid_text: eq_text(k[1]).into(),
            low_text: eq_text(k[2]).into(),
            filter_text: filter_text(k[3]).into(),
        };
        let same = |e: &EqKnobs, k: [f32; 4]| {
            e.high == k[0] && e.mid == k[1] && e.low == k[2] && e.filter == k[3]
        };
        for w in self.dj_windows() {
            let d = w.global::<DjState>();
            if !d.get_eq_live() || !same(&d.get_eq_a(), k[0]) {
                d.set_eq_a(item(k[0]));
            }
            if !d.get_eq_live() || !same(&d.get_eq_b(), k[1]) {
                d.set_eq_b(item(k[1]));
            }
            if !d.get_eq_live() || !same(&d.get_eq_c(), k[2]) { d.set_eq_c(item(k[2])); }
            if !d.get_eq_live() || !same(&d.get_eq_d(), k[3]) { d.set_eq_d(item(k[3])); }
            d.set_eq_live(true);
        }
    }

    /// The crossfader's position and gains in every DJ window, now.
    pub(super) fn show_xfader(&self) {
        let (x, text) = (self.xfader.get(), self.xfader_text());
        for w in self.dj_windows() {
            let d = w.global::<DjState>();
            d.set_xfader(x);
            d.set_xfader_text(text.clone());
        }
    }

    /// Set a channel's crossfader side ("a", "thru", "b") or the curve
    /// ("smooth", "mix", "cut") on the engine, which owns them.
    pub(super) fn set_xf(&self, key: &str, value: &str) -> String {
        use patch_engine::session::{XfAssign, XfCurve};
        if !self.is_live() {
            // Mock: shown here only.
            if let Some(i) = ["assign_a", "assign_b", "assign_c", "assign_d"].iter().position(|k| *k == key)
                && let Some(a) = XfAssign::parse(value)
            {
                let mut now = self.xf_assign.get();
                now[i] = a;
                self.xf_assign.set(now);
            } else if let Some(c) = XfCurve::parse(value) {
                self.xf_curve.set(c);
            }
            return String::new();
        }
        let mut req = serde_json::json!({"op": "dj"});
        req[key] = serde_json::Value::String(value.into());
        match patch_engine::client::call(&req) {
            Ok(v) => {
                self.mirror_dj(&v);
                String::new()
            }
            Err(e) => format!("Crossfader: {e}"),
        }
    }

    pub(super) fn apply_dj_gains(&self) {
        self.mixer_touched.set(Some(Instant::now()));
        self.dj_changed.set(Some(Instant::now()));
        if !self.is_live() {
            return;
        }
        let g = self.deck_gain.get();
        let req = serde_json::json!({"op": "dj", "xfader": self.xfader.get(), "fader_a": g[0], "fader_b": g[1], "fader_c": g[2], "fader_d": g[3], "auto_gain": self.auto_gain.get()});
        // Off this thread: the whole mixer each time, so a newer position
        // replaces one not sent yet; the reply is mirrored in `poll_dj`
        // unless the mixer was moved again meanwhile.
        if !self.mixer_sends.request("mixer", req.clone())
            && let Ok(v) = patch_engine::client::call(&req)
        {
            self.mirror_dj(&v);
        }
    }

    /// The deck Light DJ follows: the one playing (A when both or neither).
    pub(super) fn now_deck(&self) -> usize {
        if !self.is_live() {
            return 0;
        }
        Self::now_of(&self.decks[0].status(), &self.decks[1].status())
    }

    /// [`Self::now_deck`] for display, from the background status poll: the
    /// frame tick never waits on the engine for it.
    pub(super) fn now_deck_shown(&self) -> usize {
        if !self.is_live() {
            return 0;
        }
        Self::now_of(&self.decks[0].snapshot(), &self.decks[1].snapshot())
    }

    pub(super) fn now_of(a: &dj::DeckStatus, b: &dj::DeckStatus) -> usize {
        let playing = |s: &dj::DeckStatus| s.loaded && !s.paused;
        if playing(b) && !playing(a) || (!a.loaded && b.loaded) {
            1
        } else {
            0
        }
    }

    pub(super) fn session_queue(&self) -> Vec<String> {
        self.light.borrow().queue.clone()
    }

    /// Take the service's DJ mixer state (another frontend may have moved
    /// it: the CLI, MIDI, Light DJ in another window).
    pub(super) fn mirror_dj(&self, v: &serde_json::Value) {
        if let Some(master) = v["tempo_master"].as_str() { self.club_master.set(match master { "b" => 1, "c" => 2, "d" => 3, _ => 0 }); }
        if let (Some(mix), Some(level)) = (v["cue"]["mix"].as_f64(), v["cue"]["level_db"].as_f64()) {
            self.cue_knobs.set((mix as f32 * 2.0 - 1.0, cue_level_knob(level as f32)));
        }
        if let (Some(a), Some(b)) = (v["cue"]["pfl"][0].as_bool(), v["cue"]["pfl"][1].as_bool()) {
            self.pfl.set([a, b, v["cue"]["pfl"][2].as_bool().unwrap_or(false), v["cue"]["pfl"][3].as_bool().unwrap_or(false)]);
        }
        if let Some(lines) = v["lines"].as_object() {
            *self.deck_lines.borrow_mut() = ["deck-a", "deck-b", "deck-c", "deck-d"]
                .map(|ch| lines.get(ch).and_then(|a| a.as_str()).unwrap_or("").to_string());
        }
        if let Some(rows) = v["assign"].as_array() {
            let now = self.xf_assign.get();
            self.xf_assign.set(std::array::from_fn(|i| {
                rows.get(i).and_then(|a| a.as_str()).and_then(patch_engine::session::XfAssign::parse).unwrap_or(now[i])
            }));
        }
        if let Some(c) = v["curve"].as_str().and_then(patch_engine::session::XfCurve::parse) {
            self.xf_curve.set(c);
        }
        if let Some(apps) = v["line_sources"].as_array() {
            *self.line_sources.borrow_mut() = apps.iter().filter_map(|a| a.as_str().map(String::from)).collect();
        }
        if let Some(cue) = v.get("cue") {
            let output = cue["output"].as_str();
            let devices = self.provider.borrow().devices();
            *self.phones_name.borrow_mut() = output.map(|id| {
                devices.iter().find(|d| d.id == id).map_or_else(|| id.to_string(), |d| d.name.to_string())
            });
        }
        let f = |x: &serde_json::Value| x.as_f64().map(|v| v as f32);
        if let Some(x) = f(&v["xfader"]) {
            self.xfader.set(x);
        }
        if let (Some(a), Some(b)) = (f(&v["faders"][0]), f(&v["faders"][1])) {
            self.deck_gain.set([a, b, f(&v["faders4"][2]).unwrap_or(0.0), f(&v["faders4"][3]).unwrap_or(0.0)]);
        }
        if let Some(rows) = v["input_trims_db"].as_array() {
            self.input_trims.set(std::array::from_fn(|i| rows.get(i).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32));
        }
        if let Some(a) = v["auto_gain"].as_bool() {
            self.auto_gain.set(a);
        }
        if let (Some(a), Some(b)) = (v["trims_db"][0].as_f64(), v["trims_db"][1].as_f64()) {
            self.trims.set([a, b, v["trims4_db"][2].as_f64().unwrap_or(0.0), v["trims4_db"][3].as_f64().unwrap_or(0.0)]);
        }
        if let Some(rows) = v.get("eq4").unwrap_or(&v["eq"]).as_array() {
            let mut k = self.eq.get();
            for (d, row) in rows.iter().take(4).enumerate() {
                for (slot, x) in row.as_array().into_iter().flatten().take(4).enumerate() {
                    if let Some(x) = x.as_f64() {
                        k[d][slot] = x as f32;
                    }
                }
            }
            self.eq.set(k);
        }
        // The Beat FX as the engine runs them: the AI DJ's moves show on
        // the panel (effect, beats, depth, deck, ON), like a DJ's hands on
        // the mixer. The deck with an effect on; else the panel's own.
        if let Some(fx) = v["fx"].as_array()
            && !self
                .fx_touched
                .get()
                .is_some_and(|t| t.elapsed() < Duration::from_millis(1500))
        {
            let mut u = self.fx.get();
            let d = (0..4)
                .find(|&d| fx.get(d).and_then(|f| f["on"].as_bool()) == Some(true))
                .unwrap_or(u.deck);
            if let Some(f) = fx.get(d).filter(|f| f.is_object()) {
                let kinds: Vec<patch_core::command::FxKind> = f["chain"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|k| k.as_str().and_then(patch_core::command::FxKind::parse))
                    .collect();
                if let Some(first) = kinds.first() {
                    u.kind = *first;
                    u.chain = [kinds.get(1).copied(), kinds.get(2).copied()];
                }
                if let Some(b) = f["beats"].as_f64() {
                    u.beats = b as f32;
                }
                if let Some(x) = f["depth"].as_f64() {
                    u.depth = x as f32;
                }
                u.on = f["on"].as_bool().unwrap_or(false);
                u.deck = d;
            }
            self.fx.set(u);
        }
        if v.get("plan").is_some() {
            *self.plan.borrow_mut() = (!v["plan"].is_null()).then(|| v["plan"].clone());
            let anchor = v["plan"].as_object().and_then(|p| {
                let from = if p.get("from")?.as_str()? == "b" {
                    1
                } else {
                    0
                };
                Some((
                    p.get("beats_in")?.as_f64()?,
                    from,
                    p.get("bars").and_then(|b| b.as_f64()).unwrap_or(0.0),
                ))
            });
            self.mix_anchor
                .set(anchor.map(|(b, f, bars)| (b, f, self.beat_phases()[f], bars)));
        }
        let mut l = self.light.borrow_mut();
        if let Some(q) = v["queue"].as_array() {
            l.queue = q
                .iter()
                .filter_map(|p| p.as_str().map(String::from))
                .collect();
        }
        let am = &v["automix"];
        if am.is_object() {
            l.automix = am["on"].as_bool().unwrap_or(false);
            l.direction = am["direction"].as_str().unwrap_or("hold").to_string();
            l.note = am["note"].as_str().unwrap_or("").to_string();
            let n = &am["next"];
            l.next_title = n["title"]
                .as_str()
                .filter(|t| !t.is_empty())
                .map(String::from)
                .unwrap_or_else(|| {
                    n["path"]
                        .as_str()
                        .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
                        .unwrap_or_default()
                });
            // One word for the crossfader (the full description is the AI
            // DJ unit's): what kind of transition it is.
            l.next_style = {
                let t = n["technique"]
                    .as_str()
                    .filter(|t| !t.is_empty())
                    .or(n["style"].as_str())
                    .unwrap_or("");
                if t.contains("hook loop") {
                    "Mash-up"
                } else if t.contains("breakdown entry") {
                    "Breakdown"
                } else if t.contains("bass replace") {
                    "Swap"
                } else if t.contains("intro blend") || t == "blend" {
                    "Blend"
                } else if t.starts_with("cut") || t == "cut" {
                    "Cut"
                } else if t.is_empty() {
                    ""
                } else {
                    "Mix"
                }
                .to_string()
            };
            l.next = match am["next"].as_object() {
                Some(n) => format!(
                    "Next: {} · {} from bar {}",
                    n.get("title")
                        .and_then(|t| t.as_str())
                        .filter(|t| !t.is_empty())
                        .map(String::from)
                        .unwrap_or_else(|| n
                            .get("path")
                            .and_then(|p| p.as_str())
                            .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
                            .unwrap_or_default()),
                    n.get("style").and_then(|s| s.as_str()).unwrap_or(""),
                    n.get("start_bar")
                        .and_then(|b| b.as_f64())
                        .map(|b| (b + 1.0).to_string())
                        .unwrap_or_default()
                ),
                None => String::new(),
            };
        }
        l.plan = match v["plan"].as_object() {
            Some(p) => {
                let style = p.get("style").and_then(|s| s.as_str()).unwrap_or("");
                let beats_in = p.get("beats_in").and_then(|b| b.as_f64()).unwrap_or(0.0);
                if beats_in < 0.0 {
                    // Scheduled: counting down to its bar.
                    let bars = (-beats_in / 4.0).ceil() as i64;
                    format!(
                        "{} {} in {bars} bar{}",
                        if l.automix { "Auto-mix:" } else { "Next:" },
                        style,
                        if bars == 1 { "" } else { "s" }
                    )
                } else {
                    format!(
                        "Mixing: {style} · bar {} of {}",
                        (beats_in / 4.0).floor() as i64 + 1,
                        p.get("bars").and_then(|b| b.as_f64()).unwrap_or(0.0)
                    )
                }
            }
            None => String::new(),
        };
        if let Some(m) = v["master_db"].as_f64() {
            l.master_db = m as f32;
        }
        let set = &v["set"];
        if set.is_object() {
            l.set_on = set["on"].as_bool().unwrap_or(false);
            l.drum_swaps = set["drum_swaps"].as_bool().unwrap_or(false);
            let el = set["elapsed_s"].as_u64().unwrap_or(0);
            let mins = set["minutes"].as_f64().unwrap_or(60.0);
            l.set_text = if l.set_on {
                let steer = set["steer"]
                    .as_object()
                    .map(|s| {
                        format!(
                            " · you asked: {} ({} more)",
                            s.get("direction").and_then(|d| d.as_str()).unwrap_or(""),
                            s.get("tracks").and_then(|t| t.as_u64()).unwrap_or(0)
                        )
                    })
                    .unwrap_or_default();
                let bpm = set["bpm"]
                    .as_array()
                    .and_then(|b| {
                        Some(format!(
                            " · {:.0}–{:.0} BPM",
                            b.first()?.as_f64()?,
                            b.get(1)?.as_f64()?
                        ))
                    })
                    .unwrap_or_default();
                format!(
                    "Auto Mix set · {} · {} / {} · going {}{}{}",
                    set["section"].as_str().unwrap_or(""),
                    dj::clock(el as f64),
                    dj::clock(mins * 60.0),
                    set["direction"].as_str().unwrap_or(""),
                    bpm,
                    steer
                )
            } else {
                "No set running: Start set lets Auto Mix play a 60-minute journey from your library".into()
            };
            // Left out when unchanged since the last reply (`history_key`).
            if set.get("log").is_some() { l.set_log = set["log"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|e| {
                    let who = match (
                        e["artist"].as_str().unwrap_or(""),
                        e["title"].as_str().unwrap_or(""),
                    ) {
                        ("", t) => t.to_string(),
                        (a, t) => format!("{a} – {t}"),
                    };
                    let what = format!(
                        "{} · {:.0} BPM · {} · {}",
                        e["style"].as_str().unwrap_or(""),
                        e["bpm"].as_f64().unwrap_or(0.0),
                        e["key"].as_str().unwrap_or(""),
                        dj::clock(e["at"].as_f64().unwrap_or(0.0))
                    );
                    (who, what)
                })
                .collect(); }
        }
    }
}

#[cfg(test)]
mod mixer_decks_tests {
    use super::*;

    #[test]
    fn either_extra_load_selects_four_and_manual_choice_survives_polling() {
        for slot in 0..2 {
            let mut mode = MixerDecks::default();
            let mut decks = [dj::DeckStatus::default(), dj::DeckStatus::default()];
            mode.observe([&decks[0], &decks[1]]);
            assert!(!mode.four);
            decks[slot].loaded = true;
            decks[slot].source = "track.flac".into();
            decks[slot].load = Some(1);
            mode.observe([&decks[0], &decks[1]]);
            assert!(mode.four);
            mode.four = false;
            mode.observe([&decks[0], &decks[1]]);
            assert!(!mode.four, "a manual two-deck choice survives a poll");
            decks[slot].load = Some(2);
            mode.observe([&decks[0], &decks[1]]);
            assert!(mode.four, "reloading the same track selects four again");
            decks[slot].loaded = false;
            mode.observe([&decks[0], &decks[1]]);
            assert!(!mode.four, "C and D both empty again: back to two decks");
            mode.four = true;
            mode.observe([&decks[0], &decks[1]]);
            assert!(mode.four, "a manual four-deck choice with C and D empty survives a poll");
            mode.four = false;
            decks[slot].loaded = true;
            mode.observe([&decks[0], &decks[1]]);
            assert!(mode.four, "loading after an eject selects four again");
        }
    }

    #[test]
    fn four_stays_while_either_extra_deck_still_has_a_track() {
        let mut mode = MixerDecks::default();
        let loaded = |src: &str| dj::DeckStatus { loaded: true, source: src.into(), load: Some(1), ..Default::default() };
        let (mut c, d) = (loaded("c.flac"), loaded("d.flac"));
        mode.observe([&c, &d]);
        assert!(mode.four);
        c = dj::DeckStatus::default();
        mode.observe([&c, &d]);
        assert!(mode.four, "D still holds a track");
        mode.observe([&c, &dj::DeckStatus::default()]);
        assert!(!mode.four);
    }

    #[test]
    fn stream_source_change_selects_four_without_native_load_counter() {
        let mut mode = MixerDecks::default();
        let mut c = dj::DeckStatus { loaded: true, source: "https://one".into(), ..Default::default() };
        let d = dj::DeckStatus::default();
        mode.observe([&c, &d]);
        mode.four = false;
        c.source = "https://two".into();
        mode.observe([&c, &d]);
        assert!(mode.four);
    }
}

#[cfg(test)]
mod mixer_window_tests {
    use super::*;
    use patch_host::session::HostedApp;
    use slint::platform::WindowEvent;

    #[test]
    fn crossfader_window_takes_the_mode_shortcut_without_a_click() {
        let _ = patch_host::shot::install();
        let app = AudioApp::new();
        let w = app.create("dj-mixer");
        w.show().unwrap();
        let _ = patch_host::shot::Shooter { scale: 1.0 }.render(&w, 686, 752);
        app.keys.opened.set(None);
        w.window().dispatch_event(WindowEvent::WindowActiveChanged(true));
        let initial = w.global::<DjState>().get_mixer_four();
        let text: SharedString = "4".into();
        w.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        w.window().dispatch_event(WindowEvent::KeyReleased { text });
        assert_ne!(w.global::<DjState>().get_mixer_four(), initial);
        assert_eq!(app.mixer_decks.borrow().four, !initial);
    }
}
