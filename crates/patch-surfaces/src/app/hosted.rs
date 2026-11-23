//! AudioApp as a hosted Patch app: what the session host asks of it.

use super::*;

impl HostedApp for AudioApp {
    type Window = PatchWindow;

    fn id() -> AppId {
        AppId::Patch
    }

    fn spec() -> AppSpec {
        spec::spec()
    }

    fn provenance(&self) -> String {
        if self.is_live() {
            String::new()
        } else {
            "SIMULATED".into()
        }
    }

    fn command(&self, cmd: Command, origin: Origin) -> Result<(), String> {
        // Loading a track yourself overrules the AI DJ's choice.
        if origin == Origin::User
            && matches!(
                cmd,
                Command::LoadTrack { .. } | Command::DeckLoadPath { .. }
            )
        {
            self.ai_touched();
        }
        // A human touching the decks or mixer during a plan takes over:
        // the rest of the plan is cancelled (automation never fights the DJ).
        if origin == Origin::User && self.plan.borrow().is_some() {
            let dj = matches!(
                cmd,
                Command::Crossfader { .. }
                    | Command::CrossfaderStep { .. }
                    | Command::DeckFader { .. }
                    | Command::DeckEqKnob { .. }
                    | Command::DeckEq { .. }
                    | Command::DeckFilter { .. }
                    | Command::DeckFilterStep { .. }
                    | Command::DeckTransport { .. }
                    | Command::DeckTempo { .. }
                    | Command::DeckTempoStep { .. }
                    | Command::DeckBend { .. }
                    | Command::DeckJump { .. }
                    | Command::DeckLoop { .. }
                    | Command::DeckReloop { .. }
                    | Command::DeckLoopSet { .. }
                    | Command::DeckLoopAdjust { .. }
                    | Command::DeckCensor { .. }
                    | Command::DeckMaster { .. }
                    | Command::DeckCueRecall { .. }
                    | Command::DeckLoopToggle { .. }
                    | Command::DeckLoopIn { .. }
                    | Command::DeckLoopOut { .. }
                    | Command::DeckReverse { .. }
            );
            if dj {
                self.ai_touched();
                self.plan.replace(None);
                let _ = patch_engine::client::call(&serde_json::json!({"op": "plan_cancel"}));
                *self.dj_status.borrow_mut() = "Transition handed back to you".into();
            }
        }
        let strip = |name: &str| self.resolve_strip(name);
        let ctx = CommandContext {
            current_gain_db: match &cmd {
                // The master isn't a mixer strip: the engine's master gain.
                Command::SetGain { strip: s, .. } if s.eq_ignore_ascii_case("master") => {
                    Some(self.light.borrow().master_db)
                }
                Command::SetGain { strip: s, .. } => strip(s).map(|(_, g)| g),
                _ => None,
            },
            crossfader: Some(self.xfader.get()),
            recording: !self.rec_snapshot.borrow().is_null(),
        };
        let cmd = command::validate(cmd, origin, &ctx).map_err(|r| r.0)?;
        let reply = match cmd {
            Command::SetGain { strip: s, db } if s.eq_ignore_ascii_case("master") => {
                if self.is_live() {
                    patch_engine::client::call(&serde_json::json!({"op": "master", "db": db}))
                        .map_err(|e| format!("master: {e}"))?;
                    self.dj_synced.set(None);
                    self.dj_changed.set(Some(Instant::now()));
                } else {
                    self.light.borrow_mut().master_db = db.clamp(-120.0, 12.0);
                }
                self.poll_dj();
                Ok(())
            }
            Command::SetGain { strip: s, db } => match strip(&s) {
                Some((id, _)) if self.set_deck_strip_gain(id, db) => Ok(()),
                Some((id, _)) => {
                    self.provider.borrow_mut().set_gain(id, db);
                    self.rebuild_strips();
                    self.rebuild_selection();
                    Ok(())
                }
                None => Err(format!("no mixer strip matching '{s}'")),
            },
            Command::SetMute { strip: s, muted } => match strip(&s) {
                Some((id, _)) => {
                    self.provider.borrow_mut().set_mute(id, muted);
                    self.rebuild_graph();
                    self.rebuild_strips();
                    self.rebuild_selection();
                    Ok(())
                }
                None => Err(format!("no mixer strip matching '{s}'")),
            },
            Command::Route { stream, through } => {
                let node = self.provider.borrow().find_stream(&stream);
                match node {
                    None => Err(format!("no playback stream matching '{stream}'")),
                    Some(id) => {
                        let r = self.provider.borrow_mut().route_through_patch(id, through);
                        if r.is_ok() {
                            *self.selected.borrow_mut() = id.to_string();
                        }
                        r
                    }
                }
            }
            Command::Restore => self.provider.borrow_mut().restore_routing(),
            Command::Connect { from, to } => {
                let p = self.provider.borrow();
                match (p.find_stream(&from), p.find_stream(&to)) {
                    (Some(f), Some(t)) => {
                        drop(p);
                        self.provider.borrow_mut().connect(f, t)
                    }
                    (None, _) => Err(format!("nothing called '{from}'")),
                    (_, None) => Err(format!("no output called '{to}'")),
                }
            }
            Command::RecordStart { source, format } => {
                if self.rec_busy.get() {
                    return Err("recorder is busy".into());
                }
                let default = if self
                    .provider
                    .borrow()
                    .recordable()
                    .iter()
                    .any(|(id, _, _)| id == "program")
                {
                    "program"
                } else {
                    "bus"
                };
                let src = source.as_deref().unwrap_or(default);
                self.toggle_record_mode(Some((src, format.map(|f| f == RecordFormat::F32))))
            }
            Command::RecordStop => {
                if self.rec_busy.get() {
                    return Err("recorder is busy".into());
                }
                self.queue_record(serde_json::json!({"op":"stop"}));
                Ok(())
            }
            Command::LoadTrack { deck, query } => {
                // The matching rule is shared with the engine, so a controller's
                // "load <query>" and a click here cannot land on different tracks
                // within whichever list each is looking at.
                match self
                    .visible_library("")
                    .into_iter()
                    .find(|item| patch_library::title_matches(&item.title, &query))
                {
                    Some(item) => {
                        self.load_deck(deck.id(), &item)
                    }
                    None => Err(format!("nothing in the library matching '{query}'")),
                }
            }
            Command::DeckTransport { deck, action } => {
                let d = self.deck(deck.id());
                match action {
                    Transport::Cue => d.cue(),
                    Transport::CueRelease => d.cue_release(),
                    Transport::Toggle => d.toggle(),
                    Transport::Play | Transport::Pause => {
                        let playing = self.is_live() && !d.status().paused;
                        if (action == Transport::Play) != playing {
                            d.toggle();
                        }
                    }
                }
                Ok(())
            }
            Command::DeckEject { deck } => {
                // Unload the track and stop playback. For a native deck this
                // is the engine's eject; for an mpv stream it also quits the
                // player, so the deck is empty either way.
                self.deck(deck.id()).quit();
                Ok(())
            }
            Command::DeckTempo { deck, percent } => {
                self.with_pitch(deck.id(), |p| {
                    let t = percent as f64 / 100.0;
                    while t.abs() > p.range && p.range < dj::RANGES[dj::RANGES.len() - 1] {
                        p.range = p.next_range();
                    }
                    p.tempo = t.clamp(-p.range, p.range);
                });
                Ok(())
            }
            Command::DeckKeylock { deck, on } => {
                self.with_pitch(deck.id(), |p| p.keylock = on);
                Ok(())
            }
            Command::DeckKey { deck, semitones } => {
                self.with_pitch(deck.id(), |p| p.key_shift = semitones);
                Ok(())
            }
            Command::DeckEq { deck, band, db } => {
                let v = match db {
                    None => -1.0,
                    Some(d) if d < 0.0 => d / 26.0,
                    Some(d) => d / 6.0,
                };
                let slot = match band {
                    patch_core::command::Band::High => 0,
                    patch_core::command::Band::Mid => 1,
                    patch_core::command::Band::Low => 2,
                };
                self.set_eq_knob(Self::deck_index(deck.id()), slot, v);
                Ok(())
            }
            // The DJ session's queue and auto-mix live in the service.
            c @ (Command::StemControl { .. }
            | Command::StemRamp { .. }
            | Command::QueueAdd { .. }
            | Command::QueueRemove { .. }
            | Command::QueueClear
            | Command::AutoMix { .. }
            | Command::TransitionNow
            | Command::DjSession { .. }
            | Command::DjSet { .. }
            | Command::DjSay { .. }
            | Command::AiMode { .. }
            | Command::AiPick { .. }
            | Command::AiDrop { .. }
            | Command::AiApprove
            | Command::AiCancel
            | Command::EqBand { .. }
            | Command::EqBypass { .. }
            | Command::EqGain { .. }
            | Command::EqReset { .. }
            | Command::EqPreset { .. }
            | Command::EqSave { .. }
            | Command::EqApp { .. }) => {
                if !self.is_live() {
                    return Err("the DJ session runs on the live engine".into());
                }
                let req = serde_json::json!({"op": "command", "command": serde_json::from_str::<serde_json::Value>(&c.to_json()).unwrap_or_default(), "origin": origin});
                patch_engine::client::call(&req).map(|_| ())?;
                self.dj_synced.set(None);
                self.dj_changed.set(Some(Instant::now()));
                Ok(())
            }
            Command::DeckFx {
                deck,
                kind,
                on,
                beats,
                depth,
            } => self.set_fx(Self::deck_index(deck.id()), kind, on, beats, depth),
            Command::DeckFxChain {
                deck,
                kinds,
                on,
                beats,
                depth,
            } => self.set_fx_chain(Self::deck_index(deck.id()), &kinds, on, beats, depth),
            Command::DeckFilter { deck, amount } => {
                self.set_eq_knob(Self::deck_index(deck.id()), 3, amount);
                Ok(())
            }
            Command::DeckFilterStep { deck, amount } => {
                // An endless knob sends movement, so the new value is measured
                // from the one the booth is showing — the knob lands where the
                // finger expects rather than snapping to the raw delta.
                let d = Self::deck_index(deck.id());
                let now = self.eq.get()[d][3];
                self.set_eq_knob(d, 3, now + amount);
                Ok(())
            }
            // The live engine owns loop history and manual takeover. In
            // particular its active AI plan may not be this window's plan.
            c @ (Command::DeckLoop { .. }
            | Command::DeckReloop { .. }
            | Command::DeckLoopToggle { .. }
            | Command::DeckLoopScale { .. }
            | Command::DeckLoopIn { .. }
            | Command::DeckLoopOut { .. })
                if self.is_live() =>
            {
                let request = serde_json::json!({"op": "command", "command":
                    serde_json::from_str::<serde_json::Value>(&c.to_json()).unwrap_or_default(), "origin": origin});
                patch_engine::client::call(&request).map(|_| ())?;
                self.dj_synced.set(None);
                self.dj_changed.set(Some(Instant::now()));
                Ok(())
            }
            Command::DeckLoop { deck, beats } => match beats {
                Some(b) => self
                    .deck(deck.id())
                    .deck_action("loop", serde_json::json!({"beats": b})),
                None => self
                    .deck(deck.id())
                    .deck_action("loop_exit", serde_json::json!({})),
            },
            c @ (Command::DeckLoopSet { .. }
            | Command::DeckLoopAdjust { .. }
            | Command::DeckCensor { .. }
            | Command::DeckMaster { .. }
            | Command::DeckCueRecall { .. }) => {
                if !self.is_live() {
                    return Err("This control needs a live deck".into());
                }
                patch_engine::client::call(&serde_json::json!({"op":"command","origin":origin,"command":serde_json::from_str::<serde_json::Value>(&c.to_json()).unwrap_or_default()})).map(|_| ())
            }
            Command::DeckReloop { deck } => self
                .deck(deck.id())
                .deck_action("reloop", serde_json::json!({})),
            Command::DeckJump { deck, beats } => self
                .deck(deck.id())
                .deck_action("jump", serde_json::json!({"beats": beats})),
            Command::DeckSlip { deck, on } => self
                .deck(deck.id())
                .deck_action("slip", serde_json::json!({"on": on})),
            // The sample maker's panel is this window's; a save goes to the
            // engine with the panel's choices.
            Command::DeckSample { deck, action } => {
                match action {
                    patch_core::command::SampleAction::Panel => self.sample_toggle(deck.id()),
                    patch_core::command::SampleAction::Save => self.sample_save_on(deck.id()),
                }
                Ok(())
            }
            Command::DeckQuantize { deck, on } => self
                .deck(deck.id())
                .deck_action("quantize", serde_json::json!({"on": on})),
            // A manual loop and reverse are the engine's own transport, so the
            // booth passes them through exactly as it does slip and quantize: one
            // place decides what they mean, whoever asks.
            Command::DeckLoopIn { deck } => self
                .deck(deck.id())
                .deck_action("loop_in", serde_json::json!({})),
            Command::DeckLoopOut { deck } => self
                .deck(deck.id())
                .deck_action("loop_out", serde_json::json!({})),
            Command::DeckReverse { deck, on } => self
                .deck(deck.id())
                .deck_action("reverse", serde_json::json!({"on": on})),
            Command::DjPlan { plan, level } => self.start_plan(*plan, level, origin),
            Command::Ai { action, direction } => self.ai_command(action, direction, origin),
            Command::DeckLoadPath { deck, path } => {
                let item = self
                    .library
                    .borrow()
                    .iter()
                    .find(|i| i.url == path)
                    .cloned();
                match item {
                    Some(item) => {
                        self.load_deck(deck.id(), &item)
                    }
                    None => Err("only library tracks can be loaded this way".into()),
                }
            }
            Command::DeckSeek { deck, seconds } => {
                self.deck(deck.id()).seek_abs(seconds);
                Ok(())
            }
            // Scratching stays the engine's business: it owns the transport,
            // and it is the one place that can measure a jog's movement against
            // the playhead. The command goes back as a canonical one so a
            // controller, this UI, the AI DJ and the remote all take the same
            // path (`patch-engine/src/daemon/commands.rs`).
            Command::DeckScratch {
                deck,
                seconds,
                delta,
                velocity,
            } => {
                let mut command = serde_json::json!({
                    "op": "dj.scratch",
                    "deck": deck.id(),
                    "velocity": velocity,
                });
                match (seconds, delta) {
                    (Some(seconds), _) => command["seconds"] = serde_json::json!(seconds),
                    (None, Some(delta)) => command["delta"] = serde_json::json!(delta),
                    (None, None) => return Err("scratch needs seconds or delta".into()),
                }
                patch_engine::client::call(&serde_json::json!({
                    "op": "command",
                    "command": command,
                    "origin": origin,
                }))
                .map(|_| ())
                .map_err(|e| format!("scratch: {e}"))
            }
            Command::DeckScratchEnd { deck } => patch_engine::client::call(&serde_json::json!({
                "op": "command",
                "command": {"op": "dj.scratch_end", "deck": deck.id()},
                "origin": origin,
            }))
            .map(|_| ())
            .map_err(|e| format!("scratch: {e}")),
            Command::DjPlanCancel => {
                self.plan.replace(None);
                if self.is_live() {
                    let _ = patch_engine::client::call(&serde_json::json!({"op": "plan_cancel"}));
                }
                *self.dj_status.borrow_mut() = "Transition cancelled".into();
                Ok(())
            }
            Command::DeckTempoStep { deck, percent } => {
                self.with_pitch(deck.id(), |p| {
                    p.tempo = (p.tempo + percent as f64 / 100.0).clamp(-p.range, p.range)
                });
                Ok(())
            }
            Command::DeckRange { deck } => {
                self.with_pitch(deck.id(), |p| {
                    p.range = p.next_range();
                    p.tempo = p.tempo.clamp(-p.range, p.range);
                });
                Ok(())
            }
            Command::DeckToggle { deck, what } => {
                let id = deck.id();
                match what {
                    patch_core::command::Feature::Keylock => {
                        self.with_pitch(id, |p| p.keylock = !p.keylock);
                        Ok(())
                    }
                    patch_core::command::Feature::Slip => {
                        let on = !self.deck(id).status().slip;
                        self.deck(id)
                            .deck_action("slip", serde_json::json!({"on": on}))
                    }
                    patch_core::command::Feature::Quantize => {
                        let on = !self.deck(id).status().quantize;
                        self.deck(id)
                            .deck_action("quantize", serde_json::json!({"on": on}))
                    }
                    patch_core::command::Feature::Pfl => {
                        let mut p = self.pfl.get();
                        let i = Self::deck_index(id);
                        p[i] = !p[i];
                        self.pfl.set(p);
                        self.send_cue(serde_json::json!({
                            "op": "pfl", "channel": (["deck-a", "deck-b", "deck-c", "deck-d"][i]), "on": p[i]
                        }));
                        Ok(())
                    }
                }
            }
            Command::DeckKeyStep { deck, semitones } => {
                self.with_pitch(deck.id(), |p| {
                    p.key_shift = (p.key_shift + semitones).clamp(-12, 12)
                });
                Ok(())
            }
            Command::DeckLoopToggle { deck, beats } => self.loop_toggle(deck.id(), beats as f64),
            Command::DeckLoopScale { deck, factor } => self
                .deck(deck.id())
                .deck_action("loop_scale", serde_json::json!({"factor": factor})),
            Command::DeckBend { deck, amount } => {
                if self.is_live() {
                    // A jog changes bend alone. Re-sending keylock and key for
                    // every motion used to re-prime the stretcher repeatedly.
                    self.pitch.borrow_mut()[Self::deck_index(deck.id())].bend = amount as f64;
                    self.deck(deck.id()).deck_action("bend", serde_json::json!({"value": amount}))?;
                    self.poll_dj();
                } else { self.with_pitch(deck.id(), |p| p.bend = amount as f64); }
                Ok(())
            }
            Command::CrossfaderStep { delta } => {
                self.xfader.set((self.xfader.get() + delta).clamp(0.0, 1.0));
                self.apply_dj_gains();
                Ok(())
            }
            Command::DeckFader { deck, position } => {
                let mut g = self.deck_gain.get();
                g[Self::deck_index(deck.id())] = position;
                self.deck_gain.set(g);
                self.apply_dj_gains();
                self.rebuild_strips();
                Ok(())
            }
            Command::DeckEqKnob { deck, band, value } => {
                let slot = match band {
                    patch_core::command::Band::High => 0,
                    patch_core::command::Band::Mid => 1,
                    patch_core::command::Band::Low => 2,
                };
                self.set_eq_knob(Self::deck_index(deck.id()), slot, value);
                Ok(())
            }
            Command::Pfl { deck, on } => {
                let mut p = self.pfl.get();
                p[Self::deck_index(deck.id())] = on;
                self.pfl.set(p);
                self.send_cue(serde_json::json!({
                    "op": "pfl", "channel": (["deck-a", "deck-b", "deck-c", "deck-d"][deck.index()]), "on": on
                }));
                Ok(())
            }
            Command::HotCue { deck, n, clear } => {
                self.hot_cue(Self::deck_index(deck.id()), (n - 1) as usize, clear);
                Ok(())
            }
            Command::DeckSync { deck } => self.sync_deck(deck.id()).map(|m| {
                *self.dj_status.borrow_mut() = m;
            }),
            Command::DjStart => self.start_dj(),
            Command::DjStop { force } => {
                let live = self.live_decks();
                if !live.is_empty() && !force {
                    Err(format!(
                        "{} still playing: the DJ Booth keeps running. Stop playback first, or `dj stop --force`",
                        decks_text(&live)
                    ))
                } else {
                    self.stop_dj();
                    Ok(())
                }
            }
            Command::Crossfader { position } => {
                self.xfader.set(position);
                self.apply_dj_gains();
                Ok(())
            }
        };
        self.dj_changed.set(Some(Instant::now()));
        self.after_routing();
        reply
    }

    /// Closing a window never stops its Unit. When the last window of a Unit
    /// that still carries audio goes away, say that it keeps running.
    fn query(&self, what: &str) -> Option<String> {
        match what {
            "ai" => serde_json::to_string(&self.ai_view()).ok(),
            _ => None,
        }
    }

    /// The controller's switches (engine settings).
    fn settings(&self) -> Vec<patch_host::SettingRow> {
        use patch_core::setup::SWITCHES;
        let setup = patch_core::setup::load();
        let mut rows: Vec<patch_host::SettingRow> = module_rows();
        rows.extend(super::module_keys::rows());
        rows.extend(SWITCHES
            .iter()
            // The modules (and the themes) are the Modules rows above.
            .filter(|(id, _, _)| *id != "themes" && !patch_core::setup::MODULES.iter().any(|m| m.0 == *id))
            .filter(|(id, _, _)| {
                !matches!(
                    *id,
                    "hq_decks" | "show_track_path" | "track_map" | "mix_spots" | "online_lookups" | "preview_aux_notice"
                ) || setup.dj
            })
            .map(|(id, label, detail)| patch_host::SettingRow {
                id: (*id).into(),
                label: (*label).into(),
                detail: (*detail).into(),
                group: match *id {
                    "hq_decks" | "show_track_path" | "track_map" | "mix_spots" | "online_lookups" | "preview_aux_notice" => "DJ Booth",
                    "focus_border" => "Windows",
                    "dev_mode" => "Developer",
                    id if patch_core::setup::MODULES.iter().any(|m| m.0 == id) => "Modules",
                    _ => "Setup",
                }
                .into(),
                on: setup.get(id).unwrap_or(false),
                action: false,
            }));
        let on = |name: &str| {
            patch_engine::client::call(&serde_json::json!({"op": "setting", "name": name}))
                .ok()
                .and_then(|v| v["on"].as_bool())
        };
        if setup.dj
            && let Some(v) = on("notify_tracks")
        {
            rows.push(patch_host::SettingRow {
                id: "notify_tracks".into(),
                label: "Song notifications".into(),
                detail: "A desktop notification for every new song the decks play".into(),
                group: "DJ Booth".into(),
                on: v,
                action: false,
            });
        }
        // How loud the decks play: one row per mode, the one in use on.
        if setup.dj
            && let Ok(v) = patch_engine::client::call(&serde_json::json!({"op": "levels"}))
        {
            for m in v["modes"].as_array().into_iter().flatten() {
                let id = m["id"].as_str().unwrap_or("");
                let mut label = id.to_string();
                label[..1].make_ascii_uppercase();
                rows.push(patch_host::SettingRow {
                    id: format!("levels:{id}"),
                    label: format!("Levels: {label}"),
                    detail: m["about"].as_str().unwrap_or("").into(),
                    group: "DJ Booth".into(),
                    on: v["mode"] == id,
                    action: false,
                });
            }
        }
        // How meters look: one row per style, the default (saved) on.
        let default = crate::meter_style::load();
        for look in crate::meter_style::Look::ALL {
            let (label, detail) = look.label();
            rows.push(patch_host::SettingRow {
                id: format!("meters:{}", look.name()),
                label: format!("Meters: {label}"),
                detail: detail.into(),
                group: "Meters".into(),
                on: look == default,
                action: false,
            });
        }
        // Unit styles: Classic (Omarchy), Patch (the same layouts in Patch's
        // own style) or Pro gear (club-gear layouts), for all units and per
        // unit (as all units → Classic → Patch → Pro gear on each click).
        let styles = patch_ui::style::load();
        rows.push(patch_host::SettingRow {
            id: "style:logo-accent".into(),
            label: "Unit logo follows theme accent".into(),
            detail: "use Patch red by default, or the current theme accent".into(),
            group: "Style".into(),
            on: styles.logo_accent,
            action: false,
        });
        for style in patch_ui::style::Style::ALL {
            rows.push(patch_host::SettingRow {
                id: format!("style:default:{}", style.name()),
                label: format!("All units: {}", style.label()),
                detail: match style {
                    patch_ui::style::Style::Classic => "Patch's Omarchy look".into(),
                    patch_ui::style::Style::Patch => "the same layouts in Patch's own style".into(),
                    patch_ui::style::Style::Pro => "Patch style, laid out like club gear".into(),
                },
                group: "Style".into(),
                on: styles.default == style,
                action: false,
            });
        }
        for (key, label) in patch_ui::style::UNITS {
            let own = styles.unit(key);
            rows.push(patch_host::SettingRow {
                id: format!("style:unit:{key}"),
                label: format!("{label}: {}", own.map_or("as all units", |v| v.label())),
                detail: "click: as all units → Classic → Patch → Pro gear".into(),
                group: "Style".into(),
                on: own.is_some(),
                action: false,
            });
        }
        rows
    }

    fn toggle_setting(&self, id: &str) {
        // Saved; every window picks it up from the file within 1.5 s.
        if let Some(rest) = id.strip_prefix("style:") {
            let mut styles = patch_ui::style::load();
            if rest == "logo-accent" {
                styles.logo_accent = !styles.logo_accent;
            } else if let Some(v) = rest.strip_prefix("default:").and_then(patch_ui::style::Style::parse) {
                styles.default = v;
            } else if let Some(key) = rest.strip_prefix("unit:") {
                styles.cycle_unit(key);
            }
            patch_ui::style::save(&styles);
            return;
        }
        if let Some(look) = id.strip_prefix("meters:").and_then(crate::meter_style::Look::parse_name) {
            crate::meter_style::save(look);
            self.set_meter_look(look);
            return;
        }
        if let Some(mode) = id.strip_prefix("levels:") {
            let _ = patch_engine::client::call(&serde_json::json!({"op": "levels", "mode": mode}));
            return;
        }
        if let Some(module) = id.strip_prefix("module:") {
            toggle_module(module);
            return;
        }
        if let Some(action) = id.strip_prefix("licence:") {
            super::module_keys::action(action);
            return;
        }
        let mut setup = patch_core::setup::load();
        if let Some(on) = setup.get(id) {
            let before = setup;
            setup.set(id, !on);
            change_setup(before, setup);
            return;
        }
        let now = patch_engine::client::call(&serde_json::json!({"op": "setting", "name": id}))
            .ok()
            .and_then(|v| v["on"].as_bool());
        if let Some(on) = now {
            let _ = patch_engine::client::call(
                &serde_json::json!({"op": "setting", "name": id, "on": !on}),
            );
        }
    }

    fn profiles(&self) -> Vec<patch_host::ProfileRow> {
        // As chosen: a module waiting for its key keeps the profile marked.
        let now = patch_core::setup::load_saved().profile().or(patch_core::setup::load().profile());
        patch_core::setup::Profile::ALL
            .iter()
            .map(|p| patch_host::ProfileRow {
                id: p.id().into(),
                name: p.name().into(),
                about: p.about().into(),
                active: now == Some(*p),
            })
            .collect()
    }

    fn choose_profile(&self, id: &str) {
        if let Some(p) = patch_core::setup::Profile::from_id(id) {
            let now = patch_core::setup::load();
            change_setup(now, p.apply(&now));
            // What the profile downloads (themes, wallpapers, stems) comes
            // in the background.
            let c = patch_core::modules::builtin();
            if let Some(def) = c.profile(id) {
                let get: Vec<String> = c
                    .with_needs(&def.modules)
                    .into_iter()
                    .filter(|m| c.module(m).is_some_and(|m| m.download || m.step.is_some()))
                    .collect();
                if !get.is_empty() {
                    let mut args = vec!["modules", "add"];
                    args.extend(get.iter().map(String::as_str));
                    args.push("--yes");
                    patch_cli(&args);
                }
            }
        }
    }

    fn surface_hidden(&self, surface: &str) -> bool {
        // A plugin window opens from the rack, for one plugin.
        surface == "plugin" || !patch_core::setup::load().shows(surface)
    }

    fn instance_title(&self, window: &str) -> Option<String> {
        self.plugin_window_title(window)
    }

    fn layout_hidden(&self, layout: &str) -> bool {
        !spec::layout_offered(&patch_core::setup::load(), layout)
    }

    fn arrange(
        &self,
        recipe: patch_core::LayoutRecipe,
        screen: Option<(f32, f32)>,
    ) -> patch_core::LayoutRecipe {
        spec::arrange(&patch_core::setup::load(), recipe, screen)
    }

    fn window_closed(&self, surface: &str, open: &[String]) {
        if !self.is_live() {
            return;
        }
        let unit_of = |id: &str| {
            spec::spec()
                .surface(patch_core::instance::base(id))
                .map(|s| s.unit)
        };
        let Some(unit) = unit_of(surface) else { return };
        if open.iter().any(|o| unit_of(o) == Some(unit)) {
            return; // the Unit is still on screen
        }
        match unit {
            // The last Library window closed: the pre-listen ends with it.
            UnitKind::Library => {
                if !self.preview.borrow().url.is_empty() {
                    self.end_preview();
                }
            }
            UnitKind::DjBooth => {
                let live = self.live_decks();
                if !live.is_empty() {
                    notify(
                        "DJ Booth keeps playing",
                        &format!(
                            "{} still playing. Stop it from the DJ Booth when you're done.",
                            decks_text(&live)
                        ),
                    );
                }
            }
            UnitKind::Recorder if !self.rec_snapshot.borrow().is_null() => {
                notify(
                    "Recording continues",
                    "The take keeps recording. Stop it from the Recorder or with `omarchy-patch record stop`.",
                );
            }
            _ if open.is_empty()
                && patch_engine::client::live().is_ok_and(|(_, program)| program) =>
            {
                notify(
                    "Patch keeps playing",
                    "All windows are closed; the audio keeps running through Patch.",
                );
            }
            _ => {}
        }
    }

    fn badge(&self, surface: &str) -> String {
        if spec::spec()
            .surface(surface)
            .is_some_and(|s| !s.origin.is_implemented())
        {
            return "PLANNED".into();
        }
        // Windows that are a preview of a Unit say so, live or not.
        if spec::PREVIEW.contains(&surface) {
            return "PREVIEW".into();
        }
        match (self.is_live(), surface) {
            (true, _) => "LIVE".into(),
            (false, _) => "MOCK".into(),
        }
    }

    fn hints(surface: &str) -> Vec<(&'static str, &'static str)> {
        spec::hints(surface)
    }

    fn new() -> Rc<Self> {
        let t_start = Instant::now();
        let mark = |what: &str| {
            if std::env::var_os("PATCH_STARTUP_LOG").is_some() {
                eprintln!(
                    "startup: {what} at {} ms · heap {:.1} MB",
                    t_start.elapsed().as_millis(),
                    patch_host::heap::in_use_mb()
                );
            }
        };
        let frozen = patch_host::shot::active();
        // The service's DJ mixer (it owns it); defaults in screenshots.
        let dj_now = if frozen {
            patch_engine::session::Mixer::default()
        } else {
            patch_engine::client::call(&serde_json::json!({"op": "dj"}))
                .ok()
                .and_then(|v| {
                    let f = |x: &serde_json::Value| x.as_f64().map(|v| v as f32);
                    Some(patch_engine::session::Mixer {
                        faders: [f(&v["faders"][0])?, f(&v["faders"][1])?],
                        extra_faders: [f(&v["faders4"][2]).unwrap_or(0.0), f(&v["faders4"][3]).unwrap_or(0.0)],
                        extra_trims_db: [0.0; 2],
                        xfader: f(&v["xfader"])?,
                        auto_gain: v["auto_gain"].as_bool()?,
                        group_db: 0.0,
                        group_mute: false,
                        trims_db: [0.0; 2],
                        ..patch_engine::session::Mixer::default()
                    })
                })
                .unwrap_or_else(|| patch_engine::session::Session::load().mixer)
        };
        let mut mixer_decks = dj_mixer::MixerDecks::default();
        if patch_host::shot::active()
            && std::env::var("PATCH_SHOT_KEYS").is_ok_and(|keys| keys.contains('4'))
        {
            mixer_decks.four = true;
        }
        mark("dj mixer from the service");
        let library_items = if frozen && !live_shots() {
            dj::demo_library()
        } else {
            dj::library()
        };
        let shot_library = frozen && !live_shots();
        let shot_status = shot_library.then(|| dj::demo_library_status(&library_items));
        let shot_ready = if shot_library {
            dj::demo_readiness(&library_items)
        } else {
            HashMap::new()
        };
        let shot_members = if shot_library {
            dj::demo_memberships(&library_items)
        } else {
            HashMap::new()
        };
        let (lib_collections, lib_inactive) = if frozen && !live_shots() {
            dj::demo_collections()
        } else {
            (Vec::new(), dj::load_inactive_collections())
        };
        mark("library");
        let library_scan = if frozen {
            None
        } else {
            let (tx, rx) = std::sync::mpsc::channel();
            let _ = std::thread::Builder::new()
                .name("patch-dj-library-scan".into())
                .spawn(move || {
                    let _ = tx.send(dj::scan_native_library());
                });
            Some(rx)
        };
        let stored_cues = if frozen {
            std::collections::HashMap::from([(
                "~/Music/Kasper Lind/Midnight Transit.flac".to_string(),
                [
                    Some(0.12),
                    Some(62.0),
                    Some(155.0),
                    None,
                    None,
                    None,
                    None,
                    None,
                ],
            )])
        } else {
            dj::load_cues()
        };
        mark("cues");
        let provider = choose_provider(frozen);
        mark("audio provider");
        let app = Rc::new_cyclic(|me| AudioApp {
            plugins: Default::default(),
            provider: RefCell::new(provider),
            nodes: Rc::default(),
            edges: Rc::default(),
            strips: Rc::default(),
            in_channels: Rc::default(),
            out_channels: Rc::default(),
            main_dests: Rc::default(),
            console_inner: Cell::new(0.0),
            console_patch_style: Cell::new(false),
            console_w: Cell::new(0.0),
            fit_cache: Cell::new(None),
            devices: Rc::default(),
            presets: Rc::default(),
            sections: Rc::default(),
            mic_polls: Default::default(),
            mic_info: RefCell::new(
                if patch_host::shot::active() && std::env::var_os("PATCH_MOCK_REMOTE").is_some() {
                    serde_json::json!({"mics": [
                        {"channel": "mic-5", "source": "stream:mock:5", "description": "Chromium",
                         "monitor": true, "input_send": true, "loops": ["AUX 1"]},
                        {"channel": "mic-6", "source": "stream:mock:6", "description": "Call",
                         "monitor": true, "input_send": false}
                    ]})
                } else {
                    serde_json::Value::Null
                }
            ),
            mic_calibration: RefCell::new(None),
            mic_pending: RefCell::new(None),
            deck_strips: Cell::new([None; 4]),
            targets: Rc::default(),
            outputs: Rc::default(),
            buses: Rc::default(),
            bus_targets: Rc::default(),
            bus_meter: RefCell::new([[-120.0; 2]; 4]),
            spectrum: Rc::default(),
            history: Rc::default(),
            // Shots can start on another selection (`PATCH_SHOT_SELECTED=`:
            // none, the Inspector's empty state).
            selected: RefCell::new(
                std::env::var("PATCH_SHOT_SELECTED")
                    .ok()
                    .filter(|_| patch_host::shot::active())
                    .unwrap_or_else(|| "mic".into()),
            ),
            expanded: RefCell::new(HashSet::from(["scarlett"])),
            device_cursor: Cell::new(0),
            preset_cursor: Cell::new(3),
            toast: RefCell::default(),
            metering: RefCell::default(),
            sequencer: sequencer_view::View::default(),
            pattern: pattern_view::View::default(),
            tidal: tidal_view::View::default(),
            player: crate::player::Player::default(),
            previewer: crate::player::Player::preview(),
            aux_told: Cell::new(false),
            player_picker: crate::poll::Poller::default(),
            player_picking: Cell::new(false),
            carts: Default::default(),
            desk: Default::default(),
            rec_cursor: Cell::new(0),
            rec_polls: crate::poll::Poller::default(),
            rec_scans: crate::poll::Poller::default(),
            rec_actions: crate::poll::Poller::default(),
            rec_busy: Cell::new(false),
            rec_watch: Cell::new(false),
            rec_edited: Cell::new(Instant::now()),
            rec_snapshot: RefCell::new(serde_json::Value::Null),
            rec_files: RefCell::new(serde_json::Value::Null),
            rec_scan_at: Cell::new(None),
            rec_float: Cell::new(false),
            rec_message: RefCell::default(),
            bc: BcView::default(),
            net: network::NetView::default(),
            decks: [Deck::new("a", "Deck A"), Deck::new("b", "Deck B"), Deck::new("c", "Deck C"), Deck::new("d", "Deck D")],
            pitch: RefCell::new(if frozen {
                [
                    dj::Pitch {
                        tempo: 0.0161,
                        ..Default::default()
                    },
                    dj::Pitch {
                        tempo: -0.0079,
                        key_shift: 1,
                        ..Default::default()
                    },
                    Default::default(), Default::default(),
                ]
            } else {
                load_pitch().unwrap_or_default()
            }),
            analyst: (!frozen).then(dj::Analyst::start),
            analyses: RefCell::default(),
            lib_views: RefCell::default(),
            club_views: RefCell::default(),
            club_master: Cell::new(0),
            lib_stale: Cell::new(None),

            lib_status: RefCell::new(shot_status),
            lib_status_at: Cell::new(None),
            lib_ready: RefCell::new(shot_ready),
            hist_flat: Cell::new(false),
            law_drawn: Cell::new(None),
            scope_anchor: RefCell::new(vec![None, None, None, None]),
            overviews: RefCell::new(vec![None, None, None, None]),
            track_maps: RefCell::new(vec![None, None, None, None]),
            scope_style: Cell::new(crate::scope::Style::load()),
            meter_look: Cell::new(crate::meter_style::load()),
            law_glow: Cell::new(0),
            mix_anchor: Cell::new(None),
            prof_on: std::env::var_os("PATCH_TRACE_FRAMES").is_some()
                || std::env::var_os("PATCH_TRACE_HEAP").is_some(),
            heap_trace: std::env::var_os("PATCH_TRACE_HEAP").map(Into::into),
            heap_at: Cell::new(patch_host::heap::in_use_mb()),
            heap_by: RefCell::new(vec![]),
            heap_written: Cell::new(None),
            prof_at: Cell::new(None),
            prof: RefCell::default(),
            trims: Cell::new([0.0; 4]),
            input_trims: Cell::new([0.0; 4]),
            auto_gain: Cell::new(dj_now.auto_gain),
            cues: RefCell::new(stored_cues),
            cues_stamp: Cell::new(None),
            mission_cues: RefCell::default(),
            heads: Cell::new([(0.0, Instant::now(), false, 1.0); 4]),
            phones_name: RefCell::new(if frozen {
                Some("Scarlett 18i20 · Phones 1".into())
            } else {
                None
            }),
            deck_lines: RefCell::default(),
            line_sources: RefCell::default(),
            xf_assign: Cell::new(dj_now.assign),
            xf_curve: Cell::new(dj_now.curve),
            pfl: Cell::new(if frozen { [false, true, false, false] } else { [false; 4] }),
            cue_knobs: Cell::new((0.0, 0.0)),
            deck_sources: RefCell::default(),
            grid_sent: RefCell::default(),
            scope_detail: RefCell::default(),
            keymap: patch_core::input::Keymap::load(),
            fx: Cell::new(FxUnit::default()),
            fx_touched: Cell::new(None),
            fx_before: Cell::new(None),
            ai: RefCell::default(),
            ai_was_on: Cell::default(),
            ai_acting: Cell::default(),
            plan: RefCell::new(None),

            deck_loops: Cell::new([(None, false); 4]),
            diag_rows: Rc::default(),
            analyzer: Rc::default(),
            visual: Rc::default(),
            tidal_visual: Rc::new(crate::visual::Panel::backdrop()),
            broadcast_views: RefCell::default(),
            vis_levels: Cell::new([0.0; 2]),
            aidj: Default::default(),
            dj_workspaces: RefCell::default(),
            stem_views: RefCell::default(),
            master_eq: Rc::default(),
            controller_polls: Default::default(),
            sample: Default::default(),
            controller_poll: Cell::new(None),
            cpu_engine: Default::default(),
            perf: Default::default(),
            cpu_ui: Default::default(),
            frame_worst_ms: Cell::new(0.0),
            budget: crate::budget::Budget::new(
                crate::budget::load().map_or(0, |t| t.start_level()),
            ),
            frames: Cell::new(0),
            presented: RefCell::new(HashMap::new()),
            frame_no: Cell::new(0),
            frame_loop: FrameLoop::default(),
            keys: KeyGuard::default(),
            last_timing: RefCell::new(None),
            eq: Cell::new(if frozen {
                [[0.0, 0.0, -0.46, 0.0], [0.2, 0.0, -1.0, -0.35], [0.0; 4], [0.0; 4]]
            } else {
                { let session = patch_engine::session::Session::load(); [session.eq[0], session.eq[1], session.extra_eq[0], session.extra_eq[1]] }
            }),
            library_keys: RefCell::new(
                library_items
                    .iter()
                    .map(|i| {
                    (
                        i.title.to_lowercase(),
                        i.detail.to_lowercase(),
                        i.url.to_lowercase(),
                    )
                })
                    .collect(),
            ),
            lib_favourites: RefCell::new(dj::favourite_paths(&library_items)),
            library: RefCell::new(library_items),
            library_loader: Default::default(),
            library_briefs: Default::default(),
            lib_collections: RefCell::new(lib_collections),
            lib_inactive: RefCell::new(lib_inactive),
            lib_members: RefCell::new(shot_members),
            lib_sets: Default::default(),
            lib_set_order: Default::default(),
            lib_play_order: Default::default(),
            set_target: Cell::new(None),
            preview: Default::default(),
            hud_idle: Default::default(),
            preview_paused: Cell::new(None),
            library_scan: RefCell::new(library_scan),
            session_id: format!("patch-{}-{}", std::process::id(), timestamp_utc()),
            // A running engine owns the mix: adopt its faders, don't reset them.
            xfader: Cell::new(dj_now.xfader),
            mixer_decks: RefCell::new(mixer_decks),
            mixer_touched: Cell::new(None),
            dj_synced: Cell::new(None),
            dj_history: Default::default(),
            dj_polls: Default::default(),
            mixer_sends: Default::default(),
            plan_adjust_polls: Default::default(),
            plan_adjusting: Cell::new(false),
            dj_presence_at: Default::default(),
            diag_polls: Default::default(),
            diag_snapshot: Default::default(),
            dj_changed: Default::default(),
            engine_online: Cell::new(!frozen),
            light: RefCell::new(LightView {
                master_db: 0.0,
                ..Default::default()
            }),
            lib_selected: RefCell::new(None),
            reference: Cell::new(MeterReference::Output),
            stop_armed: Cell::new(None),
            deck_gain: Cell::new(dj_now.faders4()),
            dj_status: RefCell::default(),
            clip_until: RefCell::default(),
            // A full 12 s of slots from the start, so bars never balloon.
            hist_values: RefCell::new(std::iter::repeat_n(0.0, HISTORY).collect()),
            last_t: Cell::new(0.0),
            last_scope: Cell::new(0.0),
            last_hist: Cell::new(-1.0),
            last_text: Cell::new(-1.0),
            windows: RefCell::default(),
            timer: slint::Timer::default(),
            start: Instant::now(),
            frozen,
            me: me.clone(),
        });
        // Meters show what you hear (ADR-005): OUTPUT timing, every block
        // drawn at its stamp + the output latency. `o` switches to ENGINE.
        mark("app state");
        app.provider
            .borrow_mut()
            .set_meter_reference(MeterReference::Output);
        app.fix_selection();
        mark("selection");
        if let Some(d) = app.provider.borrow().devices().first()
            && !app.provider.borrow().provenance().is_simulated()
        {
            app.expanded.borrow_mut().insert(d.id);
        }
        if frozen && live_shots() {
            // Live screenshots: meter 1.5 s of real audio before rendering.
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_millis(1500) {
                std::thread::sleep(Duration::from_millis(33));
                app.tick(t0.elapsed().as_secs_f64());
            }
        } else if frozen {
            // Deterministic pre-roll for screenshots: 12.3 s at 30 fps.
            for i in 0..=369 {
                app.tick(i as f64 / 30.0);
            }
        }
        // The library is analysed by the librarian (the service's), with or
        // without windows; decks' loads still come first here.
        mark("ready");
        app
    }

    fn create(self: &Rc<Self>, surface: &str) -> PatchWindow {
        let t0 = Instant::now();
        let w = PatchWindow::new().expect("create window");
        let t_new = t0.elapsed();
        w.set_surface(surface.into());
        // The theme first: the waveform, fader law and EQ images are drawn
        // in sync_window with the Theme's colours (drawn with the built-in
        // palette, they kept it until something else redrew them).
        patch_ui::apply_theme!(&w, &patch_ui::load_palette());
        self.bind(&w);
        let t_bind = t0.elapsed();
        self.sync_window(&w);
        let t_sync = t0.elapsed();
        self.ensure_timer();
        if std::env::var_os("PATCH_STARTUP_LOG").is_some() {
            eprintln!(
                "create {surface}: new {} · bind {} · sync {} · timer {} ms",
                t_new.as_millis(),
                (t_bind - t_new).as_millis(),
                (t_sync - t_bind).as_millis(),
                (t0.elapsed() - t_sync).as_millis()
            );
        }
        w
    }
}

/// Settings › Modules: every module of the catalogue with its state. On:
/// it is here and on; clicking adds or removes it (`omarchy-patch modules`).
fn module_rows() -> Vec<patch_host::SettingRow> {
    patch_core::module_state::states()
        .into_iter()
        .map(|m| {
            let size = patch_core::modules::human_size(m.size);
            let detail = if let Some((state, done)) = &m.job {
                match state.as_str() {
                    "downloading" => format!("Downloading · {:.0} % · in the background", done * 100.0),
                    "queued" => "Waiting to download".into(),
                    "verifying" => "Checking the download".into(),
                    _ => "Installing".into(),
                }
            } else if let Some(e) = &m.failed {
                format!("Couldn't be added ({e}) · click to try again")
            } else if m.locked && m.coming_soon {
                super::module_keys::locked_detail(true)
            } else if m.locked {
                super::module_keys::locked_detail(false)
            } else if m.wanted {
                format!("Chosen, not here yet · click to add{}", if m.size > 0 { format!(" ({size})") } else { String::new() })
            } else if !m.on && m.size > 0 {
                format!("{} · {size} to download", m.about)
            } else if m.on && m.has_files {
                format!("{} · click to remove (asks first)", m.about)
            } else {
                m.about.clone()
            };
            patch_host::SettingRow {
                id: format!("module:{}", m.id),
                label: if m.preview { format!("{} (preview)", m.name) } else { m.name },
                detail,
                group: "Modules".into(),
                on: m.on || m.job.is_some(),
                action: false,
            }
        })
        .collect()
}

fn spawn_quiet(cmd: &mut std::process::Command) {
    if let Ok(mut child) = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        // `setsid -f` returns at once; collected then, so no finished
        // process is left behind the window.
        std::thread::spawn(move || child.wait());
    }
}

/// `omarchy-patch <args>` on its own (the window doesn't wait).
pub(super) fn patch_cli(args: &[&str]) {
    if let Ok(exe) = std::env::current_exe() {
        spawn_quiet(std::process::Command::new("setsid").arg("-f").arg(exe).args(args));
    }
}

/// `omarchy-patch <args>` in a terminal, for what asks something (a key, a
/// confirmation before deleting files).
pub(super) fn patch_in_terminal(args: &[&str]) {
    let Ok(exe) = std::env::current_exe() else { return };
    let line = format!(
        "{} {}; echo; read -rsn1 -p 'Press any key to close'",
        exe.display(),
        args.join(" ")
    );
    spawn_quiet(
        std::process::Command::new("setsid")
            .args(["-f", "xdg-terminal-exec", "--title=Patch", "--", "bash", "-c"])
            .arg(line),
    );
}

pub(super) fn open_url(url: &str) {
    if !url.is_empty() {
        spawn_quiet(std::process::Command::new("setsid").args(["-f", "xdg-open", url]));
    }
}

fn toggle_module(id: &str) {
    let Some(m) = patch_core::module_state::states().into_iter().find(|m| m.id == id) else { return };
    if m.job.is_some() {
        return;
    }
    if m.locked {
        let c = patch_core::modules::builtin();
        if let Some(l) = patch_core::licence::for_module(c, id) {
            open_url(l.offer_url());
        }
    } else if m.on && m.has_files {
        // Deleting files: asked first, in a terminal.
        patch_in_terminal(&["modules", "remove", id]);
    } else if m.on {
        patch_cli(&["modules", "remove", id, "--yes"]);
    } else {
        patch_cli(&["modules", "add", id, "--yes"]);
    }
}

