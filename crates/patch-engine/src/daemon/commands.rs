//! Performance commands (patch_core::command) applied to the decks and the mixer, and the session's mixer applied to the engine.

use super::*;

/// The DJ session's gains onto the deck channels (the RT thread ramps them,
/// or applies them on the sample a timed step is due). The session already
/// holds a timed step's value when it is posted, ahead of its sample: a
/// change from anywhere else then goes into that step, never out early.
pub(super) fn apply_mixer(s: &Shared) {
    let p = &*s.params;
    {
        let m = &s.session.lock().unwrap().mixer;
        p.set_dj_group(db_to_linear(m.group_db), m.group_mute);
    }
    let (g, trims) = {
        let session = s.session.lock().unwrap();
        (session.mixer.gains4_db(), session.input_trim_db)
    };
    for (i, ch) in ["deck-a", "deck-b", "deck-c", "deck-d"].into_iter().enumerate() {
        if let Some(c) = p.channel(ch) {
            p.set_trim(c, db_to_linear(trims[i]));
            let lin = db_to_linear(g[i]);
            let timed = match due_at() {
                Some(at) => p.post_glide(at, due_glide(), c, crate::TIMED_GAIN, lin),
                None => p.retarget_timed_gain(c, lin),
            };
            if !timed {
                p.set_gain(c, lin);
            }
        }
    }
}

/// Session upkeep: auto-gain trims follow what the decks load.
pub(super) fn sessioner(s: Arc<Shared>) {
    loop {
        std::thread::sleep(Duration::from_millis(250));
        if s.retired.load(Ordering::Acquire) {
            continue;
        }
        mix_now_pending(&s);
        automix_tick(&s);
        let paths: Vec<String> = s
            .params
            .decks
            .iter()
            .map(|d| {
                let st = d.status.lock().unwrap();
                if st.loaded {
                    st.path.to_string()
                } else {
                    String::new()
                }
            })
            .collect();
        let changed: Vec<usize> = {
            let t = s.trimmed.lock().unwrap();
            (0..paths.len()).filter(|&i| t[i] != paths[i]).collect()
        };
        if changed.is_empty() {
            continue;
        }
        for i in changed {
            // Analysis may land after the load: retry until it's there.
            let trim = crate::session::trim_for(&paths[i]);
            let analysed = paths[i].is_empty()
                || patch_analysis::cached(std::path::Path::new(&paths[i])).is_some();
            {
                let mut session = s.session.lock().unwrap();
                if i < 2 { session.mixer.trims_db[i] = trim; }
                else { session.mixer.extra_trims_db[i - 2] = trim; }
            }
            if analysed {
                s.trimmed.lock().unwrap()[i] = paths[i].clone();
            }
        }
        apply_mixer(&s);
    }
}

/// A deck's EQ knobs from the session onto its channel.
pub(super) fn apply_eq(s: &Shared, d: usize) {
    let k = {
        let session = s.session.lock().unwrap();
        if d < 2 { session.eq[d] } else { session.extra_eq[d - 2] }
    };
    let ch = ["deck-a", "deck-b", "deck-c", "deck-d"][d];
    let db = crate::session::knob_db;
    let _ = handle(
        s,
        &json!({"op": "eq", "channel": ch, "high_db": db(k[0]), "mid_db": db(k[1]), "low_db": db(k[2]), "filter": k[3]}),
    );
}

/// Beats since the grid's first beat on a native deck, now (engine side).
pub(super) fn deck_beat(p: &Params, d: usize) -> Option<f64> {
    let beat = f64::from_bits(p.grid_beat[d].load(Ordering::Relaxed));
    let off = f64::from_bits(p.grid_offset[d].load(Ordering::Relaxed));
    let (pos, _) = p.decks.get(d)?.playhead();
    if beat <= 0.0 || !pos.is_finite() {
        return None;
    }
    // TransitionPlan::start_bar is measured from the first downbeat, while
    // the transport grid offset marks the first beat (which may be beat 2–4
    // of the first bar). Keep the scheduler's bar zero on that downbeat too.
    let downbeat = p.decks[d]
        .status
        .lock()
        .unwrap()
        .grid
        .and_then(|g| g.downbeat)
        .unwrap_or(0) as f64;
    Some((pos - off) / beat - downbeat)
}

/// Execute a validated DJ command here, in the service, through the same
/// requests a frontend would send. `Err` for commands that aren't the
/// service's (routing, recording: the UI's for now).
pub(super) fn exec(s: &Shared, cmd: patch_core::command::Command) -> Result<(), String> {
    use patch_core::command::{Band, Command as C, Transport};
    let id = |d: patch_core::command::Deck| d.id().to_string();
    let other = |d: patch_core::command::Deck| match d {
        patch_core::command::Deck::A => "b",
        patch_core::command::Deck::B => "a",
        patch_core::command::Deck::C | patch_core::command::Deck::D => "a",
    };
    let run = |v: Value| handle(s, &v).map(|_| ());
    match cmd {
        C::StemControl { deck, stem, value } => {
            let i = deck.index();
            let sh = s
                .params
                .decks
                .get(i)
                .ok_or("Stem control deck is unavailable")?;
            let slot = sh.stems.index(&stem).ok_or("Unavailable stem")?;
            sh.stems.set(slot, value, true)?;
            s.eval.lock().unwrap().human_at = Some(std::time::Instant::now());
            if let Some(plan) = s.plan.lock().unwrap().as_mut() {
                plan.timeline.retain(|t|!matches!(&t.command,C::StemRamp {deck:d,stem:s,..} if *d==deck && *s==stem));
                plan.spec.steps.retain(|t|!matches!(&t.action,patch_core::intent::Action::Stem {deck:d,stem:s,..} if *d==deck && *s==stem));
            }
            Ok(())
        }
        C::StemRamp {
            deck,
            stem,
            to,
            beats,
        } => {
            let i = deck.index();
            let sh = s
                .params
                .decks
                .get(i)
                .ok_or("Stem automation deck is unavailable")?;
            let slot = sh.stems.index(&stem).ok_or("Unavailable stem")?;
            let status = deck_status(&s.params, i);
            let bpm = status["grid"]["bpm"]
                .as_f64()
                .ok_or("Stem automation requires a beatgrid")?
                * (1.0 + status["tempo"].as_f64().unwrap_or(0.0));
            let rate = s.params.last_rate.load(Ordering::Acquire).max(1);
            let duration = (beats * 60.0 / bpm * rate as f64).round() as u64;
            let at = due_at().unwrap_or_else(|| {
                s.history
                    .lock()
                    .unwrap()
                    .back()
                    .map_or(0, |(_, b)| b.stamp.sample_pos)
            });
            sh.stems.ramp(slot, at, duration, to)
        }
        C::DeckTransport { deck, action } => {
            let what = match action {
                Transport::Play => "play",
                Transport::Pause => "pause",
                Transport::Toggle => "toggle",
                Transport::Cue => "cue",
                Transport::CueRelease => "cue_release",
            };
            run(json!({"op": "deck", "deck": id(deck), "do": what}))
        }
        C::DeckKey { deck, semitones } => {
            run(json!({"op": "deck", "deck": id(deck), "do": "key", "semitones": semitones}))
        }
        C::DeckEject { deck } => {
            run(json!({"op": "deck", "deck": id(deck), "do": "eject"}))
        }
        C::DeckSync { deck } => {
            if s.params.decks.get(deck.index()).is_none() {
                return Err("Sync deck is unavailable".into());
            }
            run(json!({"op": "sync", "deck": id(deck), "on": true, "master": other(deck)}))
        }
        C::DeckLoop {
            deck,
            beats: Some(b),
        } => run(json!({"op": "deck", "deck": id(deck), "do": "loop", "beats": b})),
        C::DeckLoop { deck, beats: None } => {
            run(json!({"op": "deck", "deck": id(deck), "do": "loop_exit"}))
        }
        C::DeckLoopSet {
            deck,
            from,
            to,
            play,
        } => run(
            json!({"op":"deck","deck":id(deck),"do":"loop_set","from":from,"to":to,"play":play}),
        ),
        C::DeckLoopAdjust { deck, out, seconds } => {
            run(json!({"op":"deck","deck":id(deck),"do":"loop_adjust","out":out,"seconds":seconds}))
        }
        C::DeckCensor { deck, on } => {
            run(json!({"op":"deck","deck":id(deck),"do":"censor","on":on}))
        }
        C::DeckCueRecall { deck, seconds } => {
            run(json!({"op":"deck","deck":id(deck),"do":"cue_recall","seconds":seconds}))
        }
        C::DeckMaster { deck } => run(json!({"op":"sync_master","deck":id(deck)})),
        C::DeckReloop { deck } => run(json!({"op": "deck", "deck": id(deck), "do": "reloop"})),
        C::DeckJump { deck, beats } => {
            run(json!({"op": "deck", "deck": id(deck), "do": "jump", "beats": beats}))
        }
        C::DeckSeek { deck, seconds } => {
            run(json!({"op": "deck", "deck": id(deck), "do": "seek", "seconds": seconds}))
        }
        // The deck-side commands the DJ Booth used to own alone. Every one of
        // these is engine state — the transport carries keylock, key, quantize,
        // slip and the loop — so a controller, the CLI, the AI and a remote page
        // can now run them too, with the same semantics the booth's own arms
        // use: `slip` and `quantize` flip what the engine reports, `keylock`
        // takes the value the command carries, and `loop_toggle` exits only when
        // the loop already runs at that length.
        C::DeckKeylock { deck, on } => {
            run(json!({"op": "deck", "deck": id(deck), "do": "keylock", "on": on}))
        }
        C::DeckKeyStep { deck, semitones } => {
            let now = deck_status(&s.params, deck.index())["key"]
                .as_i64()
                .unwrap_or(0) as i32;
            let wanted = key_step(now, semitones);
            run(json!({"op": "deck", "deck": id(deck), "do": "key", "semitones": wanted}))
        }
        C::DeckQuantize { deck, on } => {
            run(json!({"op": "deck", "deck": id(deck), "do": "quantize", "on": on}))
        }
        C::DeckSlip { deck, on } => {
            run(json!({"op": "deck", "deck": id(deck), "do": "slip", "on": on}))
        }
        // A controller's sample button saves with the panel's last
        // settings; the panel itself is the deck window's.
        C::DeckSample { deck, action } => match action {
            patch_core::command::SampleAction::Save => {
                run(json!({"op": "deck_sample", "deck": id(deck), "action": "save"}))
            }
            patch_core::command::SampleAction::Panel => Ok(()),
        },
        C::DeckLoopScale { deck, factor } => {
            run(json!({"op": "deck", "deck": id(deck), "do": "loop_scale", "factor": factor}))
        }
        // A manual loop, in the engine's own two steps: the in-point waits until
        // an out-point arrives, and only then does the loop engage. Reverse is the
        // transport's own flag.
        C::DeckLoopIn { deck } => run(json!({"op": "deck", "deck": id(deck), "do": "loop_in"})),
        C::DeckLoopOut { deck } => run(json!({"op": "deck", "deck": id(deck), "do": "loop_out"})),
        C::DeckReverse { deck, on } => run(json!({
            "op": "deck",
            "deck": id(deck),
            "do": "reverse",
            "on": on,
        })),
        C::DeckLoopToggle { deck, beats } => {
            let st = deck_status(&s.params, deck.index());
            let running = st["loop_on"].as_bool().unwrap_or(false);
            let current = loop_beats(&st);
            if should_exit_loop(running, current, beats as f64) {
                run(json!({"op": "deck", "deck": id(deck), "do": "loop_exit"}))
            } else {
                run(json!({"op": "deck", "deck": id(deck), "do": "loop", "beats": beats}))
            }
        }
        C::DeckToggle { deck, what } => match what {
            // Three of the four features are the engine's own flags, so the
            // flip is read from the engine and written back to it.
            patch_core::command::Feature::Keylock => {
                let on = !deck_status(&s.params, deck.index())["keylock"]
                    .as_bool()
                    .unwrap_or(false);
                run(json!({"op": "deck", "deck": id(deck), "do": "keylock", "on": on}))
            }
            patch_core::command::Feature::Slip => {
                let on = !deck_status(&s.params, deck.index())["slip"]
                    .as_bool()
                    .unwrap_or(false);
                run(json!({"op": "deck", "deck": id(deck), "do": "slip", "on": on}))
            }
            patch_core::command::Feature::Quantize => {
                let on = !deck_status(&s.params, deck.index())["quantize"]
                    .as_bool()
                    .unwrap_or(false);
                run(json!({"op": "deck", "deck": id(deck), "do": "quantize", "on": on}))
            }
            // The deck's headphone cue, flipped the same way as the three above.
            // This arm used to refuse, on the grounds that the engine applies the
            // cue mix without keeping a queryable flag — but the flag is there
            // (`Params::pfl_of`), so the refusal left every PFL toggle dead: the
            // controller's button and Patch's own keyboard key alike.
            patch_core::command::Feature::Pfl => {
                let name = format!("deck-{}", deck.id());
                let c = s
                    .params
                    .channel(&name)
                    .ok_or_else(|| format!("no channel '{name}'"))?;
                let on = !s.params.pfl_of(c);
                run(json!({"op": "pfl", "channel": name, "on": on}))
            }
        },
        C::DeckScratch {
            deck,
            seconds,
            delta,
            velocity,
        } => {
            // A jog wheel can only describe movement, so the target is measured
            // from where the deck is now. Validation already refused the case
            // where neither or both of `seconds` and `delta` were given.
            let position = match (seconds, delta) {
                (Some(seconds), _) => seconds,
                (None, Some(delta)) => {
                    let now = deck_now(&s.params, deck.index()).3;
                    (now + delta).max(0.0)
                }
                (None, None) => return Err("scratch needs seconds or delta".into()),
            };
            run(json!({
                "op": "deck",
                "deck": id(deck),
                "do": "scratch",
                "seconds": position,
                "velocity": velocity,
            }))
        }
        C::HotCue { deck, n, clear } => {
            let (loaded, _, path, position, _, _) = deck_now(&s.params, deck.index());
            if !loaded || path.is_empty() {
                return Err(format!(
                    "Deck {} has nothing loaded, so it has no hot cues",
                    deck.id().to_uppercase()
                ));
            }
            // The cue positions live with the track, in the library, which is the
            // one store this path and the DJ Booth both write. Nothing is cached
            // here: a second copy is what makes two entry points disagree.
            let db = patch_library::open_quick().map_err(|e| format!("library: {e}"))?;
            let Some(track) = db.track_by_path(&path) else {
                return Err(format!(
                    "{path} is not in the library, so it has no hot cues to set or jump to"
                ));
            };
            let mut perf = db.performance(track);
            let slot = (n as usize).saturating_sub(1);
            perf.hot_cues
                .resize(perf.hot_cues.len().max(slot + 1), None);
            match hot_cue_action(clear, perf.hot_cues[slot], snap_to_grid(&path, position)) {
                HotCueAction::Clear => {
                    perf.hot_cues[slot] = None;
                    db.set_performance(track, &perf)
                        .map_err(|e| format!("library: {e}"))?;
                    Ok(())
                }
                // Set: stored where the DJ Booth reads it, and not sought —
                // the hand that pressed the pad is already at that position, and
                // the booth does not move the deck on a set either.
                HotCueAction::Set(at) => {
                    perf.hot_cues[slot] = Some(at);
                    db.set_performance(track, &perf)
                        .map_err(|e| format!("library: {e}"))?;
                    Ok(())
                }
                HotCueAction::Seek(at) => {
                    run(json!({"op": "deck", "deck": id(deck), "do": "seek", "seconds": at}))
                }
            }
        }
        C::DeckScratchEnd { deck } => {
            run(json!({"op": "deck", "deck": id(deck), "do": "scratch_end"}))
        }
        C::DeckLoadPath { deck, path } => {
            let deck_index = deck.index();
            if deck_index >= s.tempo_maps.lock().unwrap().len() {
                return Err("Load deck is unavailable".into());
            }
            // A playing deck is never replaced under the music (load lock):
            // pause it first. Transitions load the idle deck.
            if deck_now(&s.params, deck_index).1 {
                return Err(format!(
                    "Deck {} is playing: pause it to load another track",
                    deck.id().to_uppercase()
                ));
            }
            s.tempo_maps.lock().unwrap()[deck_index].clear();
            run(json!({"op": "deck", "deck": id(deck), "do": "load", "path": path}))?;
            // The grid from its analysis, so sync and FX lock at once.
            if let Some(mut a) = patch_analysis::cached(std::path::Path::new(&path)) {
                crate::track_analysis::apply_saved_manual_grid(&path, &mut a);
                let mut tempo_map = Vec::new();
                if a.use_grid() {
                    run(
                        json!({"op": "deck", "deck": id(deck), "do": "grid", "bpm": a.bpm, "offset": a.beat_offset, "downbeat": a.downbeat}),
                    )?;
                    tempo_map = sync_tempo_map(&a);
                } else if let Some(bpm) = a.bpm.filter(|_| a.bpm_confidence >= 0.5) {
                    // No measured grid yet: the kick refinement now, and the
                    // grid measured in the background for next time.
                    let _ = patch_analysis::grid_soon(std::path::Path::new(&path), false);
                    let fit = patch_analysis::kick_grid_for(std::path::Path::new(&path), bpm);
                    let offset = fit.map_or(a.beat_offset, |fit| fit.offset);
                    let bpm = fit.map_or(bpm, |fit| fit.bpm);
                    let beat = 60.0 / bpm;
                    let shift = (offset - a.beat_offset + beat / 2.0).rem_euclid(beat) - beat / 2.0;
                    let downbeat = if shift.abs() > beat * 0.1 {
                        None
                    } else {
                        a.downbeat
                    };
                    run(
                        json!({"op": "deck", "deck": id(deck), "do": "grid", "bpm": bpm, "offset": offset, "downbeat": downbeat}),
                    )?;
                    tempo_map = sync_tempo_map(&a);
                }
                s.tempo_maps.lock().unwrap()[deck_index] = tempo_map;
            }
            Ok(())
        }
        C::Crossfader { position } => run(json!({"op": "dj", "xfader": position})),
        C::DeckFader { deck, position } => run(json!({"op": "dj", format!("fader_{}", deck.id()): position})),
        C::DeckEqKnob { deck, band, value } => {
            let slot = match band { Band::High => 0, Band::Mid => 1, Band::Low => 2 };
            set_knob(s, deck.index(), slot, value);
            Ok(())
        }
        C::DeckFilterStep { deck, amount } => {
            let d = deck.index();
            let now = {
                let session = s.session.lock().unwrap();
                if d < 2 { session.eq[d][3] } else { session.extra_eq[d - 2][3] }
            };
            set_knob(s, d, 3, now + amount);
            Ok(())
        }
        C::DeckFilter { deck, amount } => {
            set_knob(s, deck.index(), 3, amount);
            Ok(())
        }
        C::DeckFx {
            deck,
            kind,
            on,
            beats,
            depth,
        } => {
            let mut v = json!({"op": "fx", "channel": format!("deck-{}", deck.id())});
            if let Some(k) = kind {
                v["kind"] = json!(k.name());
            }
            if let Some(o) = on {
                v["on"] = json!(o);
            }
            if let Some(b) = beats {
                v["beats"] = json!(b);
            }
            if let Some(d) = depth {
                v["depth"] = json!(d);
            }
            run(v)
        }
        C::DeckFxChain {
            deck,
            kinds,
            on,
            beats,
            depth,
        } => {
            let mut v = json!({"op": "fx", "channel": format!("deck-{}", deck.id()), "on": on});
            v["chain"] = json!(kinds.iter().map(|k| k.name()).collect::<Vec<_>>());
            if let Some(b) = beats {
                v["beats"] = json!(b);
            }
            if let Some(d) = depth {
                v["depth"] = json!(d);
            }
            run(v)
        }
        C::QueueAdd { path, at } => {
            let mut ses = s.session.lock().unwrap();
            let i = at.unwrap_or(ses.queue.len()).min(ses.queue.len());
            ses.queue.insert(i, path);
            ses.save();
            Ok(())
        }
        C::QueueRemove { index } => run(json!({"op": "queue", "remove": index})),
        C::QueueClear => run(json!({"op": "queue", "clear": true})),
        C::AutoMix { on, direction } => {
            run(json!({"op": "automix", "on": on, "direction": direction}))
        }
        C::TransitionNow => run(json!({"op": "transition_now"})),
        C::DjSet {
            on: true,
            minutes,
            arc,
            bpm,
            crate_dir,
            collection,
            opener,
        } => run(
            json!({"op": "set_start", "minutes": minutes, "arc": arc, "bpm": bpm.map(|(a, b)| vec![a, b]), "crate": crate_dir, "collection": collection, "opener": opener}),
        ),
        C::DjSet { on: false, .. } => run(json!({"op": "set_stop"})),
        C::DjSay { text } => run(json!({"op": "say", "text": text})),
        C::DjSession { request } => workspace_session(s, request).map(|_| ()),
        C::AiMode { mode } => run(json!({"op": "ai_mode", "mode": mode})),
        c @ (C::AiPick { .. } | C::AiDrop { .. }) => {
            let (id, rank, op) = match c {
                C::AiPick { id, rank } => (id, rank, "ai_select"),
                C::AiDrop { id, rank } => (id, rank, "ai_reject"),
                _ => unreachable!(),
            };
            let id = match (id, rank) {
                (Some(id), _) => id,
                (None, Some(r)) => s
                    .eval
                    .lock()
                    .unwrap()
                    .candidates
                    .get(r.saturating_sub(1))
                    .map(|c| c.path.clone())
                    .ok_or(format!("no candidate #{r}"))?,
                _ => return Err("which candidate?".into()),
            };
            run(json!({"op": op, "id": id}))
        }
        C::AiApprove => run(json!({"op": "ai_approve"})),
        C::AiCancel => run(json!({"op": "ai_cancel"})),
        C::EqBand {
            target,
            band,
            on,
            kind,
            freq,
            gain_db,
            q,
        } => {
            let t = eq_target(target.as_deref())?;
            let t = t.as_str();
            let mut st = s.eq.lock().unwrap().get(t).cloned().unwrap_or_default();
            st.edited = !st.preset.is_empty();
            if let Some(b) = st.bands.get_mut(band as usize) {
                if let Some(v) = on {
                    b.on = v;
                }
                if let Some(v) = kind {
                    b.kind = v;
                }
                if let Some(v) = freq {
                    b.freq = v;
                }
                if let Some(v) = gain_db {
                    b.gain_db = v;
                }
                if let Some(v) = q {
                    b.q = v;
                }
            }
            set_eq_state(s, t, st);
            Ok(())
        }
        C::EqBypass { target, on } => {
            let t = eq_target(target.as_deref())?;
            let t = t.as_str();
            let mut st = s.eq.lock().unwrap().get(t).cloned().unwrap_or_default();
            st.bypass = on;
            set_eq_state(s, t, st);
            Ok(())
        }
        C::EqGain { target, db } => {
            let t = eq_target(target.as_deref())?;
            let t = t.as_str();
            let mut st = s.eq.lock().unwrap().get(t).cloned().unwrap_or_default();
            st.edited = !st.preset.is_empty();
            st.gain_db = db;
            set_eq_state(s, t, st);
            Ok(())
        }
        C::EqReset { target } => {
            let t = eq_target(target.as_deref())?;
            let t = t.as_str();
            set_eq_state(s, t, patch_core::eq::EqState::default());
            Ok(())
        }
        C::EqApp { app, on } => set_app_eq(s, &app, on).map(|_| ()),
        C::EqPreset { target, name } => {
            let t = eq_target(target.as_deref())?;
            let t = t.as_str();
            let mut st = find_preset(&name)?.to_state()?;
            // The bypass is where you left it: loading doesn't flip it.
            st.bypass = s.eq.lock().unwrap().get(t).is_some_and(|x| x.bypass);
            set_eq_state(s, t, st);
            Ok(())
        }
        C::EqSave { target, name } => {
            let t = eq_target(target.as_deref())?;
            let t = t.as_str();
            let mut st = s.eq.lock().unwrap().get(t).cloned().unwrap_or_default();
            let slug = preset_slug(&name);
            let preset =
                patch_core::eq::Preset::from_state(&format!("user.{slug}"), name.trim(), &st);
            let dir = preset_dir();
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("can't create {}: {e}", dir.display()))?;
            let path = dir.join(format!("{slug}.json"));
            let tmp = path.with_extension("json.tmp");
            std::fs::write(
                &tmp,
                serde_json::to_string_pretty(&preset).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
            st.preset = name.trim().to_string();
            st.edited = false;
            set_eq_state(s, t, st);
            Ok(())
        }
        // Commands the engine has no path for. They are named one by one, with
        // the state each would need, instead of falling into a catch-all: the
        // match is then exhaustive, so adding a Command variant to `patch-core`
        // does not compile until someone decides here whether the engine can run
        // it. That is what keeps a controller, the CLI, the AI and a remote page
        // from being refused with "not a service command" while the DJ Booth's own
        // keys work — a difference that went unnoticed for several commands.
        // A relative crossfader move: the session's own xfader is read here and
        // written back through the same op an absolute move uses, so a controller
        // that only sends movement lands where the UI would.
        C::CrossfaderStep { delta } => {
            let now = s.session.lock().unwrap().mixer.xfader;
            let wanted = (now + delta).clamp(0.0, 1.0);
            run(json!({"op": "dj", "xfader": wanted}))
        }
        C::DjPlanCancel => Err(surface_only(
            "dj.plan_cancel",
            "a plan is built, held and ticked by the DJ Booth, so there is nothing \
             in the engine to cancel",
        )),
        // A relative tempo nudge, and a moment of pitch bend. The step is clamped
        // to the widest range a tempo fader offers (±50 %), because the fader's
        // own ±8/±16 choice is the DJ Booth's model and the engine does not hold
        // it; the bend is a rate the transport applies until it is re-aimed.
        C::DeckTempoStep { deck, percent } => {
            let now = deck_status(&s.params, deck.index())["tempo"]
                .as_f64()
                .unwrap_or(0.0);
            let wanted = (now + percent as f64 / 100.0).clamp(-0.5, 0.5);
            run(json!({"op": "deck", "deck": id(deck), "do": "tempo", "value": wanted}))
        }
        C::DeckBend { deck, amount } => {
            run(json!({"op": "deck", "deck": id(deck), "do": "bend", "value": amount}))
        }
        // The AI DJ's suggest / prepare / hand-over cycle is orchestrated by the
        // DJ Booth, which holds the plan and the selection; the engine only ever
        // sees the mode and the commands that cycle emits.
        C::Ai { .. } => Err(surface_only(
            "dj.ai",
            "Auto Mix's suggest and hand-over cycle is orchestrated by the DJ Booth, \
             which holds the plan and the selection; the engine holds only the mode \
             and the commands that cycle emits",
        )),
        // The tempo fader, the EQ knobs and the headphone cue: the engine holds
        // all three (the transport's tempo, the channel's EQ, the monitor mix), so
        // a controller's fader and knobs now reach them. `dj.tempo` speaks percent
        // and the transport speaks a fraction, so the conversion happens here.
        C::DeckTempo { deck, percent } => run(json!({
            "op": "deck",
            "deck": id(deck),
            "do": "tempo",
            "value": percent as f64 / 100.0,
        })),
        C::DeckEq { deck, band, db } => {
            let mut eq = json!({"op": "eq", "channel": format!("deck-{}", deck.id())});
            eq[format!("{}_db", band.key())] = json!(db);
            run(eq)
        }
        C::Pfl { deck, on } => run(json!({
            "op": "pfl",
            "channel": format!("deck-{}", deck.id()),
            "on": on,
        })),
        // The mixer's own channels — master and the engine's channel names — are
        // reachable; a strip that belongs to the host's stream graph is not, and
        // says so. Before this, a fader bound to `mixer.set_gain` did nothing at
        // all from hardware.
        C::SetGain { strip, db } => {
            if strip.eq_ignore_ascii_case("master") {
                run(json!({"op": "master", "db": db}))
            } else {
                run(json!({"op": "gain", "channel": strip, "db": db}))
            }
        }
        C::Route { .. } => Err(surface_only(
            "mixer.route",
            "routing a playback stream through Patch is the host's routing graph, \
             which the engine does not hold",
        )),
        C::Restore => Err(surface_only(
            "mixer.restore",
            "routing a playback stream through Patch is the host's routing graph, \
             which the engine does not hold",
        )),
        C::Connect { .. } => Err(surface_only(
            "mixer.connect",
            "the audio graph belongs to the host's routing provider, not to the \
             engine's session",
        )),
        C::DeckRange { .. } => Err(surface_only(
            "dj.tempo_range",
            "the tempo fader's range is the DJ Booth's own fader model",
        )),
        C::DjPlan { .. } => Err(surface_only(
            "dj.plan",
            "a plan is built, held and ticked by the DJ Booth; the engine only ever \
             sees the commands a plan emits",
        )),
        C::DjStart => Err(surface_only(
            "dj.start",
            "Auto Mix's session is started and ticked by the DJ Booth; the engine \
             only ever sees the commands it emits",
        )),
        C::DjStop { .. } => Err(surface_only(
            "dj.stop",
            "a plan is built, held and ticked by the DJ Booth; the engine only ever \
             sees the commands a plan emits",
        )),
        // The load button. The *rule* for what a query means is shared with the DJ
        // Booth (`patch_library::title_matches`), so a pad and a click agree; the
        // candidates are the whole library here, because the engine has no view,
        // where the booth searches the list it is showing. Loading then goes
        // through the same path the UI uses, with its load lock and its grid.
        C::LoadTrack { deck, query } => {
            let db = patch_library::open_quick().map_err(|e| format!("library: {e}"))?;
            let tracks = db.tracks().map_err(|e| format!("library: {e}"))?;
            let Some(track) = tracks
                .iter()
                .find(|track| patch_library::title_matches(&track.title, &query))
            else {
                return Err(format!("nothing in the library matching '{query}'"));
            };
            let path = track.path.clone();
            exec(s, C::DeckLoadPath { deck, path })
        }
        C::RecordStop => Err(surface_only(
            "record.stop",
            "the recorder is its own process with its own socket, and the surface \
             stops it",
        )),
        C::RecordStart { .. } => Err(surface_only(
            "record.start",
            "the recorder is its own process with its own socket, and the surface \
             starts it",
        )),
        C::SetMute { .. } => Err(surface_only(
            "mixer.set_mute",
            "the command names a mixer strip, and resolving a strip name to an \
             engine channel is the host's strip graph",
        )),
    }
}

/// The answer for a command the engine cannot run, naming the command and the
/// state it would need. A refusal that says why is worth more than a silence.
fn surface_only(command: &str, why: &str) -> String {
    format!("{command} is not reachable from the engine yet: {why}")
}

/// A key nudge, clamped to the twelve semitones the booth's own control clamps
/// to, so a pad and a key press cannot disagree about where the range ends.
fn key_step(current: i32, semitones: i32) -> i32 {
    (current + semitones).clamp(-12, 12)
}

/// The loop's length in beats, from the engine's own grid and loop points.
/// `None` when there is no loop, no grid, or the points do not describe one.
fn loop_beats(status: &serde_json::Value) -> Option<f64> {
    let bpm = status["grid"]["bpm"]
        .as_f64()
        .filter(|bpm| *bpm > 0.0)?;
    let from = status["loop_in"].as_f64()?;
    let to = status["loop_out"].as_f64()?;
    if to <= from {
        return None;
    }
    Some((to - from) / (60.0 / bpm))
}

/// A loop toggle exits only when the loop already runs at this length — the same
/// rule the DJ Booth's toggle uses — and otherwise sets that loop.
fn should_exit_loop(running: bool, current: Option<f64>, wanted: f64) -> bool {
    running && current.is_some_and(|beats| (beats - wanted).abs() < 0.01)
}

/// What a hot-cue command does, given what is already stored for that slot.
///
/// Split from the library call around it because this is the decision that can be
/// wrong, and it can be pinned by a test with no library and no engine.
#[derive(Debug, PartialEq)]
enum HotCueAction {
    Clear,
    /// Jump to where the cue is.
    Seek(f64),
    /// Store a new cue at this position.
    Set(f64),
}

fn hot_cue_action(clear: bool, existing: Option<f64>, snapped: f64) -> HotCueAction {
    if clear {
        return HotCueAction::Clear;
    }
    match existing {
        Some(at) => HotCueAction::Seek(at),
        None => HotCueAction::Set(snapped),
    }
}

/// The nearest beat, by the same rule the DJ Booth uses — a bpm believed with
/// confidence ≥ 0.5, rounded to the beat offset — so a cue set from a pad lands
/// exactly where a cue set from the booth would. With no analysis, or one
/// without a trustworthy tempo, the position is left alone rather than guessed
/// at.
fn snap_to_grid(path: &str, position: f64) -> f64 {
    let Some(mut a) = patch_analysis::cached(std::path::Path::new(path)) else {
        return position;
    };
    crate::track_analysis::apply_saved_manual_grid(path, &mut a);
    match a.bpm.filter(|_| a.bpm_confidence >= 0.5) {
        Some(bpm) => {
            let per = 60.0 / bpm;
            let off = a.beat_offset;
            (off + ((position - off) / per).round() * per).max(0.0)
        }
        None => position,
    }
}

pub(super) fn set_knob(s: &Shared, d: usize, slot: usize, v: f32) {
    // By hand (not a plan's timed step): the knob shows this, not the
    // end of an earlier move.
    if d < 2 && due_at().is_none() {
        s.visual_moves.lock().unwrap()[eq_lane(d, slot)].clear();
    }
    {
        let mut ses = s.session.lock().unwrap();
        if d < 2 { ses.eq[d][slot] = v.clamp(-1.0, 1.0); }
        else { ses.extra_eq[d - 2][slot] = v.clamp(-1.0, 1.0); }
        ses.save();
    }
    // Just this knob onto the channel.
    let k = {
        let session = s.session.lock().unwrap();
        if d < 2 { session.eq[d] } else { session.extra_eq[d - 2] }
    };
    let ch = ["deck-a", "deck-b", "deck-c", "deck-d"][d];
    let mut req = json!({"op": "eq", "channel": ch});
    match slot {
        3 => req["filter"] = json!(k[3]),
        _ => req[["high_db", "mid_db", "low_db"][slot]] = json!(crate::session::knob_db(k[slot])),
    }
    let _ = handle(s, &req);
}

/// Commands a human uses to play: one during a plan takes over (the rest
/// of the plan is cancelled: automation never fights the DJ).
pub(super) fn is_touch(cmd: &patch_core::command::Command) -> bool {
    use patch_core::command::Command as C;
    matches!(
        cmd,
        C::Crossfader { .. }
            | C::CrossfaderStep { .. }
            | C::DeckFader { .. }
            | C::DeckEqKnob { .. }
            | C::DeckEq { .. }
            | C::DeckFilter { .. }
            | C::DeckTransport { .. }
            | C::DeckTempo { .. }
            | C::DeckTempoStep { .. }
            | C::DeckBend { .. }
            | C::DeckJump { .. }
            | C::DeckLoop { .. }
            | C::DeckReloop { .. }
            | C::DeckLoopSet { .. }
            | C::DeckLoopAdjust { .. }
            | C::DeckCensor { .. }
            | C::DeckMaster { .. }
            | C::DeckCueRecall { .. }
            | C::DeckLoopIn { .. }
            | C::DeckLoopOut { .. }
            | C::DeckLoopScale { .. }
            | C::DeckLoopToggle { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::{HotCueAction, hot_cue_action, snap_to_grid};

    #[test]
    fn a_human_loop_edit_takes_over_a_running_plan() {
        use patch_core::command::{Command as C, Deck};
        for command in [
            C::DeckReloop { deck: Deck::A },
            C::DeckLoopIn { deck: Deck::A },
            C::DeckLoopOut { deck: Deck::A },
            C::DeckLoopScale {
                deck: Deck::A,
                factor: 0.5,
            },
            C::DeckLoopToggle {
                deck: Deck::A,
                beats: 4.0,
            },
        ] {
            assert!(super::is_touch(&command), "{command:?}");
        }
    }

    #[test]
    fn the_headphone_cue_flag_round_trips_so_a_toggle_can_flip_it() {
        // The PFL toggle was refused for a while because the engine was believed
        // not to keep this flag. It does: set it, read it back, flip it.
        let p = crate::Params::new(&["bus", "deck-a", "deck-b"]);
        let c = p.channel("deck-a").expect("deck-a is a channel");
        assert!(!p.pfl_of(c), "a deck starts un-cued");
        p.set_pfl(c, true);
        assert!(p.pfl_of(c), "what the toggle reads is what was set");
        p.set_pfl(c, false);
        assert!(!p.pfl_of(c), "and it flips back");
    }

    #[test]
    fn a_hot_cue_clears_seeks_or_sets_and_never_guesses() {
        // A clear clears, whatever is stored.
        assert_eq!(hot_cue_action(true, Some(12.5), 9.0), HotCueAction::Clear);
        assert_eq!(hot_cue_action(true, None, 9.0), HotCueAction::Clear);
        // A set cue is jumped to, and where it was stored wins over the
        // position the deck happens to be at.
        assert_eq!(
            hot_cue_action(false, Some(12.5), 9.0),
            HotCueAction::Seek(12.5)
        );
        // An empty slot takes the snapped position.
        assert_eq!(hot_cue_action(false, None, 9.0), HotCueAction::Set(9.0));
    }

    #[test]
    fn a_position_is_only_snapped_when_there_is_a_grid_to_snap_to() {
        // No analysis file for this path: the position is left exactly as it is,
        // because inventing a grid would move a cue the user placed by ear.
        let path = "/nonexistent/patch-test-track.flac";
        assert_eq!(snap_to_grid(path, 12.34), 12.34);
    }
}

#[cfg(test)]
mod deck_command_tests {
    use super::{key_step, loop_beats, should_exit_loop};
    use serde_json::json;

    #[test]
    fn a_key_nudge_stops_where_the_booth_stops() {
        assert_eq!(key_step(0, 1), 1);
        assert_eq!(key_step(11, 3), 12, "clamped at the top");
        assert_eq!(key_step(-11, -3), -12, "clamped at the bottom");
        assert_eq!(key_step(0, 0), 0);
    }

    #[test]
    fn loop_toggle_reads_the_deck_status_grid_object() {
        let params = crate::Params::new(&["bus", "deck-a"]);
        {
            let mut deck = params.decks[0].status.lock().unwrap();
            deck.grid = Some(crate::deck::transport::Grid {
                bpm: 129.0,
                offset: 0.22,
                downbeat: Some(0),
            });
            deck.loop_in = Some(118.0);
            deck.loop_out = Some(118.0 + 8.0 * 60.0 / 129.0);
            deck.loop_on = true;
        }
        let status = super::deck_status(&params, 0);
        let beats = super::loop_beats(&status);
        assert!(super::should_exit_loop(true, beats, 8.0));
        assert!(!super::should_exit_loop(true, beats, 4.0));
    }

    #[test]
    fn a_loop_toggle_exits_only_the_loop_that_is_running_at_that_length() {
        // Running at four beats, asked for four: leave the loop.
        assert!(should_exit_loop(true, Some(4.0), 4.0));
        // Running at four, asked for eight: move to eight, do not exit.
        assert!(!should_exit_loop(true, Some(4.0), 8.0));
        // Not running: set it, whatever length was asked for.
        assert!(!should_exit_loop(false, Some(4.0), 4.0));
        assert!(!should_exit_loop(false, None, 4.0));
        // Running but the length is unreadable: setting is the safe answer,
        // because exiting would leave the deck with no loop at all.
        assert!(!should_exit_loop(true, None, 4.0));
        // A fractional length still matches, as it does in the booth.
        assert!(should_exit_loop(true, Some(0.5), 0.5));
    }

    #[test]
    fn loop_length_in_beats_comes_from_the_engines_own_grid() {
        // 120 bpm: one beat is 0.5 s, so a 2 s loop is four beats.
        let status = json!({"grid": {"bpm": 120.0, "offset": 0.0, "downbeat": 0}, "loop_in": 10.0, "loop_out": 12.0});
        assert_eq!(loop_beats(&status), Some(4.0));
        // No grid, no loop points or a backwards pair: unreadable rather than
        // guessed at.
        assert_eq!(loop_beats(&json!({"loop_in": 1.0, "loop_out": 2.0})), None);
        assert_eq!(
            loop_beats(&json!({"grid": {"bpm": 120.0, "offset": 0.0, "downbeat": 0}, "loop_in": 2.0, "loop_out": 2.0})),
            None
        );
    }
}
