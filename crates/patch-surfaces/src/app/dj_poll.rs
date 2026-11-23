//! The periodic DJ poll.

use super::*;

thread_local! {
    static SETUP: std::cell::RefCell<patch_core::setup::Watched> =
        std::cell::RefCell::new(patch_core::setup::Watched::default());
}

/// Whether decks draw the file path. Read again only when setup.toml changes.
fn show_track_path() -> bool {
    SETUP.with(|setup| setup.borrow_mut().get().show_track_path)
}

/// The track map's switches (`track_map`, `mix_spots`).
pub(super) fn map_prefs() -> (bool, bool) {
    SETUP.with(|setup| {
        let s = setup.borrow_mut().get();
        (s.track_map, s.mix_spots)
    })
}

/// The Library asks whether it may look up release years online: once,
/// with the DJ Booth on, until answered here or in Settings.
pub(super) fn lookups_ask() -> bool {
    SETUP.with(|setup| {
        let s = setup.borrow_mut().get();
        s.dj && !s.online_lookups_asked
    })
}

/// Whether the Library says its pre-listen moved to AUX 1.
pub(super) fn aux_notice_on() -> bool {
    SETUP.with(|setup| setup.borrow_mut().get().preview_aux_notice)
}

impl AudioApp {
    pub(super) fn poll_dj(&self) {
        self.mark("text rate: diag, perf, dj poll");
        self.poll_library();
        self.mark("poll: library status");
        let scan_done = self
            .library_scan
            .borrow()
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        if let Some(result) = scan_done {
            self.library_scan.borrow_mut().take();
            match result {
                Ok(()) => {
                    self.request_library(true);
                    // New tracks from the scan get analysed too (cached
                    // ones return at once).
                    self.refresh_libraries();
                }
                Err(e) => {
                    *self.dj_status.borrow_mut() = format!("Library scan failed: {e}");
                }
            }
        }
        // Analyses that finished in the background.
        if let Some(an) = &self.analyst {
            let done = an.finished();
            if !done.is_empty() {
                let mut store = vec![];
                for (path, r) in done {
                    if let Ok(a) = r {
                        store.push((path.clone(), a.clone()));
                        self.analyses.borrow_mut().insert(path, a);
                    }
                }
                // Into the library database off the UI thread: one open,
                // and analyses it already holds (every cached one at start-up)
                // are skipped, so a new window never waits on disk writes.
                if !store.is_empty() {
                    let _ = std::thread::Builder::new()
                        .name("patch-library-store".into())
                        .spawn(move || store_analyses(store));
                }
                if self.lib_stale.get().is_none() {
                    self.lib_stale.set(Some(Instant::now()));
                }
            }
        }
        if self
            .lib_stale
            .get()
            .is_some_and(|t| t.elapsed() >= Duration::from_secs(2))
        {
            self.lib_stale.set(None);
            self.request_library(true);
            self.refresh_libraries();
        }
        self.mark("poll: scans, analyses");
        // The service owns the DJ mixer: follow it (twice a second).
        if self.is_live()
            && self
                .dj_synced
                .get()
                .is_none_or(|t| t.elapsed() >= Duration::from_millis(500))
        {
            self.dj_synced.set(Some(Instant::now()));
            // The set history only when it changed (the key of the one shown).
            let history = self.dj_history.borrow().clone();
            self.dj_polls
                .request("dj", serde_json::json!({"op": "dj", "history_key": history}));
            // Once with it: whether the engine is acting (an AI DJ session).
            // The app's own `ai.on` only knows about actions the app ran; the
            // engine's aivis mode is the authority on a running session.
            self.dj_polls
                .request("ai_state", serde_json::json!({"op": "ai_state"}));
            if self.plan_adjusting.get() {
                self.dj_polls.request("plan_adjust_status", serde_json::json!({"op": "plan_adjust_status"}));
            }
        }
        if self.is_live()
            && let Some(reply) = self.dj_polls.take("dj")
        {
            self.engine_online.set(reply.result.is_ok());
            // A reply requested before a local edit cannot undo it.
            if self.dj_changed.get().is_none_or(|at| at <= reply.requested)
                && let Ok(v) = reply.result
            {
                let live_mixer = self.provider.borrow().dj_mixer();
                self.mirror_dj(&v);
                // Shown now: later replies may leave this history out.
                *self.dj_history.borrow_mut() =
                    v["set"]["history_key"].as_str().unwrap_or("").to_string();
                if let Some((x, g)) = live_mixer {
                    self.xfader.set(x);
                    self.deck_gain.set(g);
                }
            }
        }
        if let Some(reply) = self.mixer_sends.take("mixer")
            && self.dj_changed.get().is_none_or(|at| at <= reply.requested)
            && let Ok(v) = reply.result
        {
            self.mirror_dj(&v);
        }
        if let Some(reply) = self.dj_polls.take("plan_adjust_status")
            && self.plan_adjusting.get()
            && let Ok(value) = reply.result
            && let Some(message) = value["message"].as_str()
        {
            for w in self.dj_windows() { w.global::<DjState>().set_plan_feedback(message.into()); }
        }
        if let Some(reply) = self.plan_adjust_polls.take("plan_adjust") {
            self.plan_adjusting.set(false);
            let message = match reply.result {
                Ok(value) => value["explanation"].as_str().unwrap_or("Transition revised").to_string(),
                Err(error) => format!("Not changed: {error}"),
            };
            for w in self.dj_windows() { w.global::<DjState>().set_plan_feedback(message.as_str().into()); }
            *self.dj_status.borrow_mut() = message.clone();
            self.light.borrow_mut().say_reply = message;
            for w in self.dj_windows() {
                w.global::<DjState>().set_plan_adjusting(false);
            }
        }
        if let Some(reply) = self.dj_polls.take("ai_state")
            && let Ok(v) = reply.result
        {
            // A session that is acting (aivis Mode::Act) is what puts the decks
            // on the AI phrase timeline.
            self.ai_acting.set(v["mode"].as_str() == Some("act"));
            // A planned (not yet started) transition is adjustable; a running
            // or absent one is not.
            let adjustable = v.get("transition").is_some_and(|transition| {
                transition["started"].as_bool() == Some(false)
                    && transition["done"].as_u64() == Some(0)
            });
            for w in self.dj_windows() {
                let d = w.global::<DjState>();
                d.set_plan_adjustable(adjustable);
                d.set_plan_adjusting(self.plan_adjusting.get());
            }
        }
        self.mark("poll: mirror dj");
        let (a, b, c, extra) = (self.deck_item(0), self.deck_item(1), self.deck_item(2), self.deck_item(3));
        self.mixer_decks.borrow_mut().observe([
            &self.decks[2].snapshot(), &self.decks[3].snapshot(),
        ]);
        self.mark("poll: deck items");
        let xt = self.xfader_text();
        if let Some(m) = FOLDER_NOTE.lock().ok().and_then(|mut n| n.take()) {
            *self.dj_status.borrow_mut() = m;
        }
        let status: SharedString = self.dj_status.borrow().as_str().into();
        let (on, engine_text) = self.engine_state();
        let (x, g) = (self.xfader.get(), self.deck_gain.get());
        let ai = self.ai_panel();
        self.mark("poll: ai panel");
        self.windows.borrow_mut().retain(|w| w.upgrade().is_some());
        // An AI DJ session starting is the decks' cue to open on the AI phrase
        // timeline instead of the performance waveform. Only at the change, so
        // a hand toggle during the session stays where it was put.
        let session = ai.on || self.ai_acting.get();
        let started = session && !self.ai_was_on.replace(true);
        if !session {
            self.ai_was_on.set(false);
        }
        let show_path = show_track_path();
        for w in self.dj_windows() {
            let d = w.global::<DjState>();
            d.set_mixer_four(self.mixer_decks.borrow().four);
            d.set_show_track_path(show_path);
            d.set_deck_a(same_lists(a.clone(), &d.get_deck_a()));
            d.set_deck_b(same_lists(b.clone(), &d.get_deck_b()));
            d.set_deck_c(same_lists(c.clone(), &d.get_deck_c()));
            d.set_deck_d(same_lists(extra.clone(), &d.get_deck_d()));
            d.set_gain_c(g[2]); d.set_gain_d(g[3]);
            d.set_pfl_c(self.pfl.get()[2]); d.set_pfl_d(self.pfl.get()[3]);
            if !self.is_live() || self.provider.borrow().dj_mixer().is_none() {
                d.set_xfader(x);
                d.set_gain_a(g[0]);
                d.set_gain_b(g[1]);
            }
            d.set_xfader_text(xt.clone());
            d.set_status(status.clone());
            d.set_engine_on(on);
            let (m, l) = self.cue_knobs.get();
            d.set_pfl_a(self.pfl.get()[0]);
            d.set_pfl_b(self.pfl.get()[1]);
            d.set_cue_mix(m);
            d.set_cue_level(l);
            d.set_cue_mix_text(cue_mix_text(m).into());
            d.set_cue_level_text(format!("{} dB", crate::num::signed(cue_level_db(l), 0)).into());
            let phones = self.phones_name.borrow().clone();
            d.set_phones_on(phones.is_some());
            {
                let lines = self.deck_lines.borrow();
                d.set_line_a(lines[0].clone().into());
                d.set_line_b(lines[1].clone().into());
                d.set_line_c(lines[2].clone().into());
                d.set_line_d(lines[3].clone().into());
                d.set_line_sources(self.line_sources.borrow().len() as i32);
            }
            let assign = self.xf_assign.get().map(|a| SharedString::from(a.id()));
            d.set_xf_assign_a(assign[0].clone());
            d.set_xf_assign_b(assign[1].clone());
            d.set_xf_assign_c(assign[2].clone());
            d.set_xf_assign_d(assign[3].clone());
            d.set_xf_curve(self.xf_curve.get().id().into());
            d.set_phones(phones.unwrap_or_else(|| "choose output".into()).into());
            d.set_engine_text(engine_text.clone());
            d.set_ai_on(ai.on);
            if started {
                // The session's start: put both decks on the AI phrase timeline.
                d.set_mission_view_a(true);
                d.set_mission_view_b(true);
            }
            d.set_ai_state(ai.state.clone());
            d.set_ai_title(ai.title.clone());
            d.set_ai_detail(ai.detail.clone());
            d.set_ai_reasons(ai.reasons.clone());
            d.set_ai_direction(ai.direction.clone());
            d.set_ai_auto(ai.auto);
            {
                let l = self.light.borrow();
                let titles = self.library.borrow();
                let items: Vec<crate::ui::QueueItem> = l
                    .queue
                    .iter()
                    .map(|p| match titles.iter().find(|it| &it.url == p) {
                        Some(it) => crate::ui::QueueItem {
                            title: it.title.clone().into(),
                            detail: it.detail.clone().into(),
                        },
                        None => crate::ui::QueueItem {
                            title: p.rsplit('/').next().unwrap_or(p).into(),
                            detail: "".into(),
                        },
                    })
                    .collect();
                d.set_now_deck(if self.now_deck_shown() == 1 { "b" } else { "a" }.into());
                if !same_rows(&d.get_queue(), &items) {
                    d.set_queue(std::rc::Rc::new(slint::VecModel::from(items)).into());
                }
                d.set_automix_on(l.automix);
                d.set_automix_dir(l.direction.clone().into());
                d.set_automix_next(l.next.clone().into());
                d.set_automix_note(l.note.clone().into());
                d.set_plan_text(l.plan.clone().into());
                d.set_mix_next(l.next_title.clone().into());
                d.set_mix_technique(l.next_style.clone().into());
                d.set_master(patch_engine::session::fader_pos(l.master_db));
                d.set_set_on(l.set_on);
                d.set_drum_swaps(l.drum_swaps);
                d.set_set_text(l.set_text.clone().into());
                d.set_say_reply(l.say_reply.clone().into());
                let log: Vec<crate::ui::QueueItem> = l
                    .set_log
                    .iter()
                    .take(6)
                    .map(|(t, d)| crate::ui::QueueItem {
                        title: t.clone().into(),
                        detail: d.clone().into(),
                    })
                    .collect();
                if !same_rows(&d.get_set_log(), &log) {
                    d.set_set_log(std::rc::Rc::new(slint::VecModel::from(log)).into());
                }
                d.set_master_text(db_text(l.master_db).into());
            }
            let fx = self.fx.get();
            d.set_fx_kind(fx.kind.name().into());
            d.set_fx_kind_2(fx.chain[0].map_or("", |k| k.name()).into());
            d.set_fx_kind_3(fx.chain[1].map_or("", |k| k.name()).into());
            d.set_fx_label(fx.label().into());
            d.set_fx_uses_beats(fx.kinds().iter().any(|k| {
                !matches!(
                    k,
                    patch_core::command::FxKind::Reverb
                        | patch_core::command::FxKind::Crush
                        | patch_core::command::FxKind::Drive
                )
            }));
            d.set_fx_beats(fx.beats);
            d.set_fx_depth(fx.depth);
            d.set_fx_deck(["a", "b", "c", "d"][fx.deck].into());
            d.set_fx_on(fx.on);
            d.set_fx_text(
                format!(
                    "{} · {} · deck {}{}",
                    fx.kinds()
                        .iter()
                        .map(|k| k.name())
                        .collect::<Vec<_>>()
                        .join(" → "),
                    fx_beats_text(fx.beats),
                    ["A", "B", "C", "D"][fx.deck],
                    if fx.on { " · ON" } else { "" }
                )
                .into(),
            );
            if !same_rows(&d.get_ai_alternates(), &ai.alternates) {
                d.set_ai_alternates(
                    std::rc::Rc::new(slint::VecModel::from(ai.alternates.clone())).into(),
                );
            }
        }
        self.poll_club_controls();
    }
}
