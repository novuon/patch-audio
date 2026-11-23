//! DJ requests: decks, sync, plans, the queue, automix, sets, the AI DJ's choices and reports.

use super::*;

pub(super) fn op_report(s: &Shared, req: &Value) -> Result<Value, String> {
        // The transition that played (the one heard), else the last
        // scheduled; marked as reported.
        let requested=req["transition_id"].as_str();
        let last = if let Some(id)=requested {
            s.session.lock().unwrap().set.transitions.iter().find(|t|t["id"].as_str()==Some(id)).cloned().ok_or("Transition history changed; select the transition again")?.into()
        } else {
            s.previous_transition.lock().unwrap().clone().or_else(||s.last_transition.lock().unwrap().clone())
        };
        let last=last.ok_or("No executed transition to report")?;
        if last.get("played").is_none() { return Err("This transition has not played yet".into()); }
        let last=Some(last);
        let decks: Vec<Value> = (0..s.params.decks.len())
            .map(|d| deck_status(&s.params, d))
            .collect();
        let grid = |path: &str| patch_analysis::grid_of(std::path::Path::new(path)).map(|g| serde_json::to_value(g).unwrap_or(Value::Null)).unwrap_or(Value::Null);
        let (from, to) = last.as_ref().map(|l| (l["from"].as_str().unwrap_or("").to_string(), l["to"].as_str().unwrap_or("").to_string())).unwrap_or_default();
        let now = crate::session::now_s();
        let report = json!({
            "version":2, "id":crate::workspace::unique_id(),
            "session_id":s.session.lock().unwrap().set.id,
            "notes":req["notes"].as_str().unwrap_or(""),
            "track_refs":patch_library::open_default().ok().and_then(|db|db.tracks().ok()).unwrap_or_default().iter().filter(|t|t.path==from || t.path==to).map(|t|json!({"id":t.id,"uuid":t.track_uuid,"path":t.path,"content_id":t.content_id})).collect::<Vec<_>>(),
            "reported": now,
            "why": req["why"].as_str().unwrap_or(""),
            "seconds_since_scheduled": last.as_ref().and_then(|l| l["scheduled"].as_f64()).map(|t| now as f64 - t),
            "transition": last,
            "grid_from": grid(&from),
            "grid_to": grid(&to),
            "decks": decks,
            "history": s.session.lock().unwrap().set.log.iter().rev().take(4).map(|e| json!({"title": e.title, "style": e.style, "path": e.path})).collect::<Vec<_>>(),
        });
        let dir = crate::session::state_dir().join("reports");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let file = dir.join(format!("{}.json",report["id"].as_str().unwrap_or_default()));
        crate::workspace::atomic_json(&file,&report)?;
        if let Some(t)=&last {
            let mut previous=s.previous_transition.lock().unwrap();
            if let Some(v)=previous.as_mut().filter(|v|v["id"]==t["id"]) {v["reported"]=report["why"].clone();}
            drop(previous);
            let mut ses=s.session.lock().unwrap();
            if let Some(v)=ses.set.transitions.iter_mut().find(|v|v["id"]==t["id"]) {v["reported"]=report["why"].clone();}
            ses.save();
            crate::session::archive_set(&ses.set,(!ses.set.on).then(crate::session::now_s));
        }
        ai_log(&format!("reported by the DJ ({}): {}", report["why"].as_str().unwrap_or(""), file.display()));
        let n = crate::session::learn_from_reports();
        ai_log(&format!("learned from {n} transition reports: the next picks weigh this one"));
        Ok(json!({"file": file.display().to_string()}))
}

pub(super) fn op_deck(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
        // Play, cue, preview: the device must be awake first.
        if matches!(req["do"].as_str(), Some("play" | "toggle" | "cue" | "preview" | "load")) {
            wake(s);
        }
        let result = deck_op(p, req)?;
        if req["do"] == "load" && let (Ok(deck),Some(path)) = (deck_index(p,req),req["path"].as_str()) { crate::stem_service::attach_cached(s.params.clone(),deck,path.into()); }
        if matches!(req["do"].as_str(),Some("scratch"|"reverse"|"scratch_end"|"censor"|"loop_set"|"loop_adjust"|"cue_recall"|"eject")) { drop_plan(s,"human transport override"); }
        Ok(result)
}

pub(super) fn op_sync(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
        let i = deck_index(p, req)?;
        let on = req["on"].as_bool().ok_or("'on' must be true or false")?;
        let master = match req["master"].as_str() {
            Some(m) => Some(deck_index(p, &json!({"deck": m}))?),
            None => None,
        };
        let mut all = s.sync.lock().unwrap();
        // A deck never follows its own follower: engaging i → m releases
        // m → i (else both correct towards each other, swap the error and
        // the deck you hear gets bent and jumped).
        if on {
            let m = master.unwrap_or(1 - i.min(1));
            if m < all.len() && m != i && all[m].on && all[m].master.is_none_or(|x| x == i) {
                all[m].on = false;
                all[m].engage = false;
                p.decks[m].send(crate::deck::transport::Cmd::Bend(0.0));
            }
        }
        all[i] = SyncState {
            on,
            master,
            phase_hold: false,
            error_ms: 0.0,
            engage: on,
            bars: false,
            last_jump: None,
            tempo_at: None,
            last_commanded_tempo: None,
        };
        if !on {
            p.decks[i].send(crate::deck::transport::Cmd::Bend(0.0));
        }
        Ok(json!("ok"))
}

pub(super) fn op_deck_status(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
        let i = deck_index(p, req)?;
        let mut v = deck_status_with(p, i, true);
        let sy = s.sync.lock().unwrap()[i];
        v["sync"] = json!({"on": sy.on, "master": sy.master.map(|m| ((b'a' + m as u8) as char).to_string()), "phase_hold": sy.phase_hold, "error_ms": sy.error_ms, "bars": sy.bars});
        Ok(v)
}

pub(super) fn op_dj(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
        let state = {
            let mut ses = s.session.lock().unwrap();
            // Only the mixer changes here: compare that (a hundred bytes),
            // not a copy of the whole session (hundreds of KB, per poll).
            let before = ses.mixer.clone();
            let m = &mut ses.mixer;
            if let Some(x) = req["xfader"].as_f64() {
                if due_at().is_none() { s.visual_moves.lock().unwrap()[0].clear(); }
                m.xfader = (x as f32).clamp(0.0, 1.0);
            }
            for (i, k) in ["fader_a", "fader_b", "fader_c", "fader_d"].into_iter().enumerate() {
                if let Some(v) = req[k].as_f64() {
                    if i < 2 && due_at().is_none() { s.visual_moves.lock().unwrap()[i + 1].clear(); }
                    if i < 2 { m.faders[i] = (v as f32).clamp(0.0, 1.0); }
                    else { m.extra_faders[i - 2] = (v as f32).clamp(0.0, 1.0); }
                }
            }
            if let Some(a) = req["auto_gain"].as_bool() {
                m.auto_gain = a;
            }
            // The assign switches and the curve. Switching where the AI DJ
            // plays its crossfader starts it from what the mixer has now.
            let was_on_faders = m.ai_on_faders();
            for (i, k) in ["assign_a", "assign_b", "assign_c", "assign_d"].into_iter().enumerate() {
                if let Some(v) = req[k].as_str() {
                    m.assign[i] = crate::session::XfAssign::parse(v).ok_or_else(|| format!("{k}: a, thru or b"))?;
                }
            }
            if let Some(v) = req["curve"].as_str() {
                m.curve = crate::session::XfCurve::parse(v).ok_or("curve: smooth, mix or cut")?;
            }
            if m.ai_on_faders() && !was_on_faders {
                m.ai = crate::session::AiControls { xfader: m.xfader, faders: m.faders };
            }
            // The AI DJ's crossfader and A/B faders (its plans, never a
            // person): on the faders when the mixer isn't set up for it.
            let ai_x = req["ai_xfader"].as_f64();
            let ai_f = [req["ai_fader_a"].as_f64(), req["ai_fader_b"].as_f64()];
            if ai_x.is_some() || ai_f.iter().any(Option::is_some) {
                let mut ai = m.ai_controls();
                if let Some(x) = ai_x {
                    ai.xfader = (x as f32).clamp(0.0, 1.0);
                }
                for (i, v) in ai_f.into_iter().enumerate() {
                    if let Some(v) = v {
                        ai.faders[i] = (v as f32).clamp(0.0, 1.0);
                    }
                }
                if due_at().is_none() {
                    let mut moves = s.visual_moves.lock().unwrap();
                    for lane in &mut moves[..3] {
                        lane.clear();
                    }
                }
                m.apply_ai(ai);
            }
            let changed = ses.mixer != before;
            if changed {
                ses.save();
            }
            let mut v = mixer_json(&ses.mixer);
            v["input_trims_db"] = json!(ses.input_trim_db);
            v["eq"] = json!(ses.eq);
            v["eq4"] = json!([ses.eq[0], ses.eq[1], ses.extra_eq[0], ses.extra_eq[1]]);
            (v, changed)
        };
        // A read (the AI DJ window at 20 Hz, the remote at 4 Hz) leaves
        // the gains alone: it once applied an AI move still due up to
        // 120 ms later, ahead of its beat.
        let (state, changed) = state;
        if changed {
            apply_mixer(s);
        }
        let mut state = state;
        state["plan"] = plan_json(s);
        state["tempo_master"] = json!(((b'a' + s.tempo_master.load(Ordering::Relaxed) as u8) as char).to_string());
        state["queue"] = json!(s.session.lock().unwrap().queue);
        // The DJ mixer's own fader: the DJ Booth on the Patch mix.
        state["master_db"] = json!(s.session.lock().unwrap().mixer.group_db);
        state["automix"] = automix_json(s);
        // Each deck's Beat FX as it runs (the AI DJ's too): the mixer
        // window shows the controls in use.
        state["fx"] = json!(["deck-a", "deck-b", "deck-c", "deck-d"].iter().filter_map(|n| p.channel(n).map(|c| fx_json(p, c))).collect::<Vec<_>>());
        state["decks"] = json!((0..p.decks.len()).map(|d|deck_status(p,d)).collect::<Vec<_>>());
        // Small monitor snapshot: a new window must not reset an existing
        // headphone mix or PFL selection when its first control is touched.
        let (cue_target, lines, line_sources) = {
            let l = s.links.lock().unwrap();
            (l.cue_target.clone(), l.deck_lines.clone(), l.line_sources.clone())
        };
        // The input selectors (LINE: deck channel → application) and the
        // applications a LINE can take now.
        state["lines"] = json!(lines);
        state["line_sources"] = json!(line_sources);
        let (mix, level) = p.cue();
        let pfl = ["deck-a", "deck-b", "deck-c", "deck-d"].map(|name| p.channel(name).is_some_and(|c| p.pfl_of(c)));
        state["cue"] = json!({
            "mix": mix, "level_db": linear_to_db(level), "output": cue_target,
            "pfl": pfl,
        });
        state["set"] = set_json(&s.session.lock().unwrap().set);
        let requested = s.fast_mode_request.lock().unwrap().clone();
        if let Some((set_id, on)) = requested
            && state["set"]["id"].as_str() == Some(set_id.as_str())
        {
            state["set"]["fast_mode_requested"] = json!(on);
        }
        // The set's history (log, transitions: ~60 KB) changes only when a
        // mix lands or a report is filed. A poller that already holds this
        // version (`history_key`) gets the rest of the state without it.
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            state["set"]["log"].to_string().hash(&mut h);
            state["set"]["transitions"].to_string().hash(&mut h);
            format!("{:016x}", h.finish())
        };
        if let Some(set) = state["set"].as_object_mut() {
            if req["history_key"].as_str() == Some(key.as_str()) {
                set.remove("log");
                set.remove("transitions");
                set.insert("history_same".into(), json!(true));
            }
            set.insert("history_key".into(), json!(key));
        }
        Ok(state)
}

pub(super) fn op_command(s: &Shared, req: &Value) -> Result<Value, String> {
        let cmd = patch_core::command::Command::from_json(&req["command"].to_string())?;
        let origin: patch_core::command::Origin = serde_json::from_value(req["origin"].clone())
            .unwrap_or(patch_core::command::Origin::Cli);
        let ctx = patch_core::command::Context {
            crossfader: Some(s.session.lock().unwrap().mixer.ai_controls().xfader),
            ..Default::default()
        };
        if origin==patch_core::command::Origin::Ai && matches!(cmd,patch_core::command::Command::StemRamp {..}) {return Err("AI stem automation requires an authorized transition plan".into());}
        let cmd = patch_core::command::validate(cmd, origin, &ctx).map_err(|e| e.0)?;
        if origin != patch_core::command::Origin::Ai && is_touch(&cmd) {
            s.eval.lock().unwrap().human_at = Some(std::time::Instant::now());
            // A human took over: the rest of the plan is dropped, and
            // the AI steps back to suggesting (it never fights the DJ).
            if drop_plan(s, "the DJ took over") {
                {
                    let mut ses = s.session.lock().unwrap();
                    if ses.automix || ses.ai_mode == patch_core::aivis::Mode::Act {
                        ses.automix = false;
                        ses.ai_mode = patch_core::aivis::Mode::Suggest;
                        ses.save();
                    }
                }
                *s.auto.lock().unwrap() = AutoState::default();
            }
        }
        if origin == patch_core::command::Origin::Ai {
            ai_exec(s, cmd)?;
        } else {
            exec(s, cmd)?;
        }
        Ok(json!("ok"))
}

pub(super) fn op_plan(s: &Shared, req: &Value) -> Result<Value, String> {
        let plan: patch_core::intent::TransitionPlan =
            serde_json::from_value(req["plan"].clone()).map_err(|e| e.to_string())?;
        let level: patch_core::intent::ControlLevel =
            serde_json::from_value(req["level"].clone())
                .unwrap_or(patch_core::intent::ControlLevel::Execute);
        let origin: patch_core::command::Origin = serde_json::from_value(req["origin"].clone())
            .unwrap_or(patch_core::command::Origin::User);
        start_plan(s, &plan, level, origin, false)
}

pub(super) fn op_queue(s: &Shared, req: &Value) -> Result<Value, String> {
        let mut ses = s.session.lock().unwrap();
        if req["clear"].as_bool() == Some(true) {
            ses.queue.clear();
        }
        match &req["add"] {
            Value::String(p) => ses.queue.push(p.clone()),
            Value::Array(a) => ses
                .queue
                .extend(a.iter().filter_map(|v| v.as_str().map(String::from))),
            _ => {}
        }
        if let Some(i) = req["remove"].as_u64()
            && (i as usize) < ses.queue.len() {
                ses.queue.remove(i as usize);
            }
        if let (Some(i), Some(j)) = (req["move"][0].as_u64(), req["move"][1].as_u64()) {
            let (i, j) = (i as usize, j as usize);
            if i < ses.queue.len() && j < ses.queue.len() {
                let t = ses.queue.remove(i);
                ses.queue.insert(j, t);
            }
        }
        ses.save();
        Ok(json!(ses.queue))
}

pub(super) fn op_automix(s: &Shared, req: &Value) -> Result<Value, String> {
        {
            let mut ses = s.session.lock().unwrap();
            let mut reset_auto = false;
            if let Some(on) = req["on"].as_bool() {
                ses.automix = on;
                ses.ai_mode = if on {
                    patch_core::aivis::Mode::Act
                } else {
                    patch_core::aivis::Mode::Suggest
                };
                reset_auto = !on;
            }
            if let Ok(d) = serde_json::from_value(req["direction"].clone()) {
                ses.direction = d;
            }
            ses.save();
            drop(ses);
            if reset_auto {
                *s.auto.lock().unwrap() = AutoState::default();
            }
        }
        Ok(automix_json(s))
}

pub(super) fn op_link_take(s: &Shared, req: &Value) -> Result<Value, String> {
        let session_id=req["session_id"].as_str().ok_or("Missing session identity")?;
        let path=req["take"]["path"].as_str().ok_or("Missing take path")?;
        if !std::path::Path::new(path).is_file(){return Err("Take audio is unavailable".into());}
        let mut ses=s.session.lock().unwrap();
        if ses.set.id!=session_id {return Err("This take belongs to another session".into());}
        if let Some(t)=ses.set.takes.iter_mut().find(|t|t["path"]==path){*t=req["take"].clone();}else{ses.set.takes.push(req["take"].clone());}
        ses.save();
        crate::session::archive_set(&ses.set,(!ses.set.on).then(crate::session::now_s));
        Ok(json!("Take linked"))
}

pub(super) fn op_workspace(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
        match req["action"].as_str().unwrap_or("list") {
            "process" => {
                if (0..2).any(|d| deck_now(p,d).1) { return Err("Pause or end live playback before running offline report processing".into()); }
                crate::workspace::start_job(serde_json::from_value(req["ids"].clone()).map_err(|e|e.to_string())?, req["improve"]==true)
            }
            "cancel_job" => crate::workspace::cancel_job(req["id"].as_str().ok_or("Select a job")?),
            "activate" | "rollback" => {
                // A scheduled plan keeps its version; only subsequently composed plans change.
                if req["action"]=="rollback" { crate::workspace::rollback() } else { crate::workspace::activate(req["id"].as_str().ok_or("Select a candidate")?) }
            }
            _ => crate::workspace::storage(req),
        }
}

pub(super) fn op_set_option(s: &Shared, req: &Value) -> Result<Value, String> {
        let mut ses = s.session.lock().unwrap();
        if let Some(on) = req["drum_swaps"].as_bool() {
            ses.set.drum_swaps = on;
        }
        ses.save();
        Ok(set_json(&ses.set))
}

/// The AI DJ's transition style and how much of each track plays:
/// `{"op":"mix_style"}` reads it; `style` (plain, balanced, adventurous),
/// each switch (`effect_exits`, `loops`, `accents`, `stem_led`,
/// `bass_replace`, `prefer_stems`) and `play_percent` (20–100) change it.
/// A change applies from the next transition: one prepared but not started
/// is planned again; one already playing finishes as planned.
pub(super) fn op_mix_style(s: &Shared, req: &Value) -> Result<Value, String> {
        let (changed, before) = {
            let mut ses = s.session.lock().unwrap();
            let before = ses.mix_style;
            let changed = ses.change_mix_style(req)?;
            patch_ai::mix::set_mix_style(ses.mix_style);
            if changed {
                ses.save();
            }
            (changed, before)
        };
        let (plays_before, prep_before) = (before.play_percent, before.stem_prep);
        let style = s.session.lock().unwrap().mix_style;
        let mut v = style.json();
        // What the stem preparation setting means on this machine.
        let (_, prep_now, seconds) = stem_prep_now(s);
        v["stem_prep_now"] = json!(prep_now.id());
        v["stem_prep_now_label"] = json!(prep_now.label());
        v["stem_seconds"] = json!(seconds);
        if style.stem_prep != prep_before {
            ai_log(&format!(
                "stem preparation: {} → {} ({} here)",
                prep_before.label(),
                style.stem_prep.label(),
                prep_now.label()
            ));
        }
        // Only stem preparation changed: what a transition plays is the
        // same, so nothing is planned again.
        let replan = changed && patch_core::mix_style::MixStyle { stem_prep: prep_before, ..style } != before;
        if style.play_percent != plays_before {
            // PLAYS sets how long blends can be: its own line, and where
            // the change came from (2026-11-12: a night at 20 % read as a
            // regression, and nothing said who set it).
            let from = req["source"].as_str().unwrap_or("a request without a source");
            ai_log(&format!(
                "PLAYS {plays_before}% → {}% of each track (from {from})",
                style.play_percent
            ));
        }
        if replan {
            request_style_replan();
            ai_log(&format!("transition style: {}", style.summary()));
        }
        v["changed"] = json!(changed);
        // What is planned already, which the setting reaches only from the
        // transition after it when it has started.
        // `planned` first: the planner holds `plan` while it takes
        // `last_transition`, so taking them the other way round (a guard
        // living through the `filter`) could deadlock every request.
        let planned = s.plan.lock().unwrap().is_some() || s.auto.lock().unwrap().pending.is_some();
        let coming = s.last_transition.lock().unwrap().clone().filter(|_| planned);
        v["next_technique"] = coming.map(|c| c["technique"].clone()).unwrap_or(Value::Null);
        Ok(v)
}

/// Where the AI DJ takes the set's tempo: `{"op":"tempo_plan"}` reads it;
/// `mode` (follow, hold, faster, slower, original), `bpm` (Hold's) and
/// `bpm_step` change it. The playing track glides there once no transition
/// is planned or running; transitions themselves never change.
pub(super) fn op_tempo_plan(s: &Shared, req: &Value) -> Result<Value, String> {
        let (plan, changed) = {
            let mut ses = s.session.lock().unwrap();
            let before = ses.tempo;
            ses.tempo.apply(req)?;
            let changed = ses.tempo != before;
            if changed {
                ses.save();
            }
            (ses.tempo, changed)
        };
        if changed {
            s.auto.lock().unwrap().tempo_settle = true;
            ai_log(&format!("tempo: {}", plan.summary()));
        }
        let mut v = plan.json();
        v["changed"] = json!(changed);
        let glide = s.auto.lock().unwrap().tempo_glide;
        v["gliding"] = json!(glide.is_some());
        // Where the set plays now: the playing deck's tempo as heard.
        let p = &*s.params;
        if let Some(d) = (0..2).find(|&d| deck_now(p, d).1) {
            let st = deck_status(p, d);
            if let Some(native) = st["grid"]["bpm"].as_f64() {
                v["playing_bpm"] = json!(native * (1.0 + st["tempo"].as_f64().unwrap_or(0.0)));
                v["target_bpm"] = json!(glide.filter(|g| g.0 == d).map(|g| native * (1.0 + g.1)));
            }
        }
        Ok(v)
}

/// How many tracks of a set have stems prepared:
/// `{"op":"stem_coverage"}` for the live set's sources, or
/// `"collections":[ids]` for others (none: the whole library). Also the
/// set's average track length and the minutes each track plays under the
/// play-length setting.
pub(super) fn op_stem_coverage(s: &Shared, req: &Value) -> Result<Value, String> {
        let collections: Vec<i64> = if req["collections"].is_array() {
            serde_json::from_value(req["collections"].clone()).map_err(|e| e.to_string())?
        } else {
            s.session.lock().unwrap().set.source_collections()
        };
        let percent = s.session.lock().unwrap().mix_style.play_percent;
        let mut v = crate::stem_service::coverage(&collections, percent)?;
        // The set preps among them, with their "prepare all stems" option.
        if let Ok(db) = patch_library::open_quick() {
            let (preps, with) = (db.set_preps(), db.set_preps_with_stems());
            v["set_preps"] = json!(collections
                .iter()
                .filter(|id| preps.contains(id))
                .map(|id| json!({"id": id, "stems": with.contains(id)}))
                .collect::<Vec<_>>());
        }
        Ok(v)
}

/// Set preps' own "prepare all stems" option (off by default):
/// `{"op":"set_prep_stems","collections":[ids],"on":true}`. Switched on
/// while its set plays, the next tick asks for them.
pub(super) fn op_set_prep_stems(_s: &Shared, req: &Value) -> Result<Value, String> {
        let ids: Vec<i64> = serde_json::from_value(req["collections"].clone())
            .map_err(|_| "which set preps: collections")?;
        let on = req["on"].as_bool().ok_or("on is true or false")?;
        let db = patch_library::open_default()?;
        for id in &ids {
            db.set_set_prep_stems(*id, on)?;
        }
        reset_set_batches();
        ai_log(&format!("set prep {ids:?}: prepare all stems {}", if on { "on" } else { "off" }));
        Ok(json!({"collections": ids, "stems": on}))
}

pub(super) fn op_set_stop(s: &Shared, _req: &Value) -> Result<Value, String> {
        *s.fast_mode_request.lock().unwrap() = None;
        drop_plan(s, "the set was stopped");
        let mut ses = s.session.lock().unwrap();
        if ses.set.on {
            crate::session::archive_set(&ses.set, Some(crate::session::now_s()));
        }
        ses.set.on = false;
        ses.automix = false;
        ses.ai_mode = patch_core::aivis::Mode::Suggest;
        ses.save();
        drop(ses);
        *s.auto.lock().unwrap() = AutoState::default();
        drop_plan(s, "the set was stopped");
        Ok(json!("stopped: the music plays on"))
}

pub(super) fn op_coming(s: &Shared, _req: &Value) -> Result<Value, String> {
        let planned = s.plan.lock().unwrap().is_some() || s.auto.lock().unwrap().pending.is_some();
        let Some(v) = s.last_transition.lock().unwrap().clone().filter(|_| planned) else { return Ok(Value::Null) };
        let next = s.auto.lock().unwrap().next.clone();
        let title = |path: &str| {
            if let Some(t) = next.as_ref().filter(|n| n["path"].as_str() == Some(path)).and_then(|n| n["title"].as_str()).filter(|t| !t.is_empty()) {
                return t.to_string();
            }
            let ses = s.session.lock().unwrap();
            ses.set.log.iter().rev().find(|e| e.path == path).map(|e| e.title.clone()).unwrap_or_else(|| std::path::Path::new(path).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
        };
        Ok(json!({
            "from": title(v["from"].as_str().unwrap_or("")),
            "to": title(v["to"].as_str().unwrap_or("")),
            "technique": v["technique"],
        }))
}

pub(super) fn op_previous(s: &Shared, _req: &Value) -> Result<Value, String> {
        let Some(v) = s.previous_transition.lock().unwrap().clone() else { return Ok(Value::Null) };
        let title = |path: &str| {
            let ses = s.session.lock().unwrap();
            ses.set.log.iter().rev().find(|e| e.path == path).map(|e| e.title.clone()).unwrap_or_else(|| std::path::Path::new(path).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
        };
        Ok(json!({
            "id":v["id"],
            "from": title(v["from"].as_str().unwrap_or("")),
            "to": title(v["to"].as_str().unwrap_or("")),
            "technique": v["technique"],
            "ago_s": v["played"].as_u64().map(|t| crate::session::now_s().saturating_sub(t)),
            "reported": v["reported"],
        }))
}

pub(super) fn op_setting(s: &Shared, req: &Value) -> Result<Value, String> {
        let name = req["name"].as_str().unwrap_or("");
        let mut ses = s.session.lock().unwrap();
        let slot = match name {
            "notify_tracks" => &mut ses.notify_tracks,
            other => return Err(format!("no setting {other:?}")),
        };
        if let Some(on) = req["on"].as_bool() {
            *slot = on;
        }
        let on = *slot;
        ses.save();
        Ok(json!({"name": name, "on": on}))
}

/// The DJ Booth's levels: `{"op":"levels"}` reads them, `"mode"` (loud,
/// balanced or streaming) sets them; the decks' trims follow at once.
pub(super) fn op_levels(s: &Shared, req: &Value) -> Result<Value, String> {
        use crate::session::Levels;
        let mode = {
            let mut ses = s.session.lock().unwrap();
            if let Some(id) = req["mode"].as_str() {
                let l = Levels::parse(id).ok_or("levels are loud, balanced or streaming")?;
                ses.levels = l;
                crate::session::set_levels(l);
                ses.save();
            }
            ses.levels
        };
        if req["mode"].is_string() {
            // The sessioner sets each deck's trim again for the new target.
            *s.trimmed.lock().unwrap() = Default::default();
        }
        Ok(json!({
            "mode": mode.id(),
            "lufs": mode.lufs(),
            "modes": Levels::ALL.iter().map(|l| json!({"id": l.id(), "lufs": l.lufs(), "about": l.about()})).collect::<Vec<_>>(),
        }))
}

pub(super) fn op_ai_mode(s: &Shared, req: &Value) -> Result<Value, String> {
        let mode: patch_core::aivis::Mode = serde_json::from_value(req["mode"].clone())
            .map_err(|_| "mode is observe, suggest, prepare or act")?;
        {
            let mut ses = s.session.lock().unwrap();
            ses.ai_mode = mode;
            ses.automix = mode == patch_core::aivis::Mode::Act;
            ses.save();
        }
        // Leaving Prepare/Act drops what isn't running yet.
        if matches!(
            mode,
            patch_core::aivis::Mode::Observe | patch_core::aivis::Mode::Suggest
        ) {
            s.auto.lock().unwrap().pending = None;
        }
        Ok(json!({"mode": mode}))
}

pub(super) fn op_ai_select_or_ai_reject(s: &Shared, req: &Value) -> Result<Value, String> {
        let id = req["id"]
            .as_str()
            .ok_or("which candidate (id)?")?
            .to_string();
        {
            let mut e = s.eval.lock().unwrap();
            if op_is(req, "ai_select") {
                e.locked = Some(id.clone());
                e.rejected.retain(|r| r != &id);
            } else {
                e.rejected.push(id.clone());
                if e.locked.as_ref() == Some(&id) {
                    e.locked = None;
                }
            }
        }
        // A different choice than what's prepared and not yet running:
        // drop it; the next tick prepares the new one.
        let started = s
            .plan
            .lock()
            .unwrap()
            .as_ref()
            .map(|pl| deck_beat(&s.params, pl.from).unwrap_or(0.0) >= pl.start_beat);
        let prepared_other = s
            .auto
            .lock()
            .unwrap()
            .prepared
            .as_ref()
            .is_some_and(|(_, p)| p != &id || op_is(req, "ai_reject"));
        if started != Some(true) && prepared_other {
            let _ = s.plan.lock().unwrap().take();
            *s.auto.lock().unwrap() = AutoState::default();
        }
        let _ = evaluate(s);
        Ok(json!("ok"))
}

pub(super) fn op_ai_approve(s: &Shared, _req: &Value) -> Result<Value, String> {
        let plan = s
            .auto
            .lock()
            .unwrap()
            .pending
            .take()
            .ok_or("nothing prepared to approve")?;
        start_plan(
            s,
            &plan,
            patch_core::intent::ControlLevel::Execute,
            patch_core::command::Origin::User,
            true,
        )
}

pub(super) fn op_ai_cancel(s: &Shared, _req: &Value) -> Result<Value, String> {
        drop_plan(s, "cancelled");
        *s.auto.lock().unwrap() = AutoState::default();
        let mut ses = s.session.lock().unwrap();
        if ses.ai_mode == patch_core::aivis::Mode::Act {
            ses.ai_mode = patch_core::aivis::Mode::Suggest;
            ses.automix = false;
            ses.save();
        }
        s.eval.lock().unwrap().human_at = Some(std::time::Instant::now());
        Ok(json!("cancelled"))
}

pub(super) fn op_dj_group(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
        let mut ses = s.session.lock().unwrap();
        if let Some(g) = req["gain_db"].as_f64() {
            ses.mixer.group_db = (g as f32).clamp(-120.0, 6.0);
        }
        if let Some(m) = req["mute"].as_bool() {
            ses.mixer.group_mute = m;
        }
        let (g, m) = (ses.mixer.group_db, ses.mixer.group_mute);
        ses.save();
        drop(ses);
        p.set_dj_group(db_to_linear(g), m);
        Ok(json!({"gain_db": g, "mute": m}))
}

pub(super) fn op_cue(s: &Shared, req: &Value) -> Result<Value, String> {
    let p = &*s.params;
        if let Some(m) = req.get("mix") {
            p.set_cue_mix(m.as_f64().ok_or("'mix' is 0 (cue) … 1 (master)")? as f32);
        }
        if let Some(g) = req.get("level_db") {
            p.set_cue_level(db_to_linear(
                g.as_f64()
                    .ok_or("'level_db' must be dB")?
                    .clamp(-60.0, 12.0) as f32,
            ));
        }
        Ok(json!("ok"))
}

pub(super) fn op_module_presence(s: &Shared, req: &Value) -> Result<Value, String> {
        let module = req["module"].as_str().ok_or("no module")?;
        if !matches!(module, "pattern" | "tidal" | "sequencer" | "dj") {
            return Err("unknown mixer module".into());
        }
        let owner = req["owner"].as_u64().and_then(|n| u32::try_from(n).ok()).ok_or("invalid module owner")?;
        let mut leases = s.module_leases.lock().unwrap();
        if req["open"].as_bool() == Some(false) {
            leases.remove(&(module.into(), owner));
        } else {
            leases.insert((module.into(), owner), Instant::now());
        }
        Ok(json!("ok"))
}

pub(super) fn op_sync_master(s: &Shared, req: &Value) -> Result<Value, String> {
    let index = deck_index(&s.params, req)?;
    drop_plan(s, "human tempo master selection");
    s.tempo_master.store(index, Ordering::Relaxed);
    let mut states = s.sync.lock().unwrap();
    for (i, state) in states.iter_mut().enumerate() {
        if i == index {
            *state = SyncState::default();
            s.params.decks[i].send(crate::deck::transport::Cmd::Bend(0.0));
        } else if state.on {
            *state = SyncState {
                on: true,
                master: Some(index),
                engage: true,
                bars: state.bars,
                ..Default::default()
            };
        }
    }
    Ok(json!("ok"))
}
