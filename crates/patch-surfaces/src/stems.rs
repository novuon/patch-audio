//! Small shared presentation for Booth and unified DJ. All I/O is off the UI thread.
use crate::ui::{PatchWindow, StemDeck, StemItem, StemState};
use serde_json::{Value, json};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{cell::Cell, rc::Rc};
const PARAM_KEYS: [&str; 24] = [
    "a-v-g", "a-v-m", "a-v-s", "a-d-g", "a-d-m", "a-d-s", "a-b-g", "a-b-m", "a-b-s", "a-o-g",
    "a-o-m", "a-o-s", "b-v-g", "b-v-m", "b-v-s", "b-d-g", "b-d-m", "b-d-s", "b-b-g", "b-b-m",
    "b-b-s", "b-o-g", "b-o-m", "b-o-s",
];
const EXTRA_PARAM_KEYS: [&str; 96] = [
    "a-0-g", "a-0-m", "a-0-s", "a-1-g", "a-1-m", "a-1-s", "a-2-g", "a-2-m",
    "a-2-s", "a-3-g", "a-3-m", "a-3-s", "a-4-g", "a-4-m", "a-4-s", "a-5-g",
    "a-5-m", "a-5-s", "a-6-g", "a-6-m", "a-6-s", "a-7-g", "a-7-m", "a-7-s",
    "b-0-g", "b-0-m", "b-0-s", "b-1-g", "b-1-m", "b-1-s", "b-2-g", "b-2-m",
    "b-2-s", "b-3-g", "b-3-m", "b-3-s", "b-4-g", "b-4-m", "b-4-s", "b-5-g",
    "b-5-m", "b-5-s", "b-6-g", "b-6-m", "b-6-s", "b-7-g", "b-7-m", "b-7-s",
    "c-0-g", "c-0-m", "c-0-s", "c-1-g", "c-1-m", "c-1-s", "c-2-g", "c-2-m",
    "c-2-s", "c-3-g", "c-3-m", "c-3-s", "c-4-g", "c-4-m", "c-4-s", "c-5-g",
    "c-5-m", "c-5-s", "c-6-g", "c-6-m", "c-6-s", "c-7-g", "c-7-m", "c-7-s",
    "d-0-g", "d-0-m", "d-0-s", "d-1-g", "d-1-m", "d-1-s", "d-2-g", "d-2-m",
    "d-2-s", "d-3-g", "d-3-m", "d-3-s", "d-4-g", "d-4-m", "d-4-s", "d-5-g",
    "d-5-m", "d-5-s", "d-6-g", "d-6-m", "d-6-s", "d-7-g", "d-7-m", "d-7-s",
];
const ACTIONS: [&str; 13] = [
    "prepare",
    "pin_prepare",
    "cancel",
    "attach",
    "cancel_batch",
    "reset",
    "auto",
    "pin",
    "unpin",
    "bridge",
    "bridge_off",
    "bridge_fx",
    "bridge_depth",
];
const ACTION_KEYS: [&str; 52] = [
    "a-prepare",
    "a-pin-prepare",
    "a-cancel",
    "a-attach",
    "a-cancel-batch",
    "a-reset",
    "a-auto",
    "a-pin",
    "a-unpin",
    "a-bridge",
    "a-bridge-off",
    "a-bridge-fx",
    "a-bridge-depth",
    "b-prepare",
    "b-pin-prepare",
    "b-cancel",
    "b-attach",
    "b-cancel-batch",
    "b-reset",
    "b-auto",
    "b-pin",
    "b-unpin",
    "b-bridge",
    "b-bridge-off",
    "b-bridge-fx",
    "b-bridge-depth",
    "c-prepare",
    "c-pin-prepare",
    "c-cancel",
    "c-attach",
    "c-cancel-batch",
    "c-reset",
    "c-auto",
    "c-pin",
    "c-unpin",
    "c-bridge",
    "c-bridge-off",
    "c-bridge-fx",
    "c-bridge-depth",
    "d-prepare",
    "d-pin-prepare",
    "d-cancel",
    "d-attach",
    "d-cancel-batch",
    "d-reset",
    "d-auto",
    "d-pin",
    "d-unpin",
    "d-bridge",
    "d-bridge-off",
    "d-bridge-fx",
    "d-bridge-depth",
];

fn deck_index(id: &str) -> usize { match id { "b" => 1, "c" => 2, "d" => 3, _ => 0 } }
fn get_deck(state: &StemState<'_>, i: usize) -> StemDeck {
    match i { 0 => state.get_a(), 1 => state.get_b(), 2 => state.get_c(), _ => state.get_d() }
}
fn set_deck(state: &StemState<'_>, i: usize, value: StemDeck) {
    match i { 0 => state.set_a(value), 1 => state.set_b(value), 2 => state.set_c(value), _ => state.set_d(value) }
}

fn action_key(action: &str, deck: usize) -> &'static str {
    ACTIONS
        .iter()
        .position(|a| *a == action)
        .map_or(ACTION_KEYS[deck * 13], |i| ACTION_KEYS[deck * 13 + i])
}
pub struct View {
    window: slint::Weak<PatchWindow>,
    polls: crate::poll::Poller,
    commands: crate::poll::Poller,
    items: [Rc<VecModel<StemItem>>; 4],
    last: Cell<f64>,
    /// The message shows a failed status read (cleared when one succeeds).
    transport_error: Cell<bool>,
    /// Gains and levels arrive every frame (shared memory): status reads
    /// then leave them alone, so a fader never steps back to a second-old
    /// value.
    live_frames: Cell<bool>,
    /// Per deck: last frame's gains, until when the AI counts as moving
    /// them, and when the DJ last moved one here.
    last_gains: Cell<[[f32; 4]; 4]>,
    moving_until: Cell<[f64; 4]>,
    touched: Cell<[f64; 4]>,
    clock: Cell<f64>,
}
impl View {
    pub fn bind(w: &PatchWindow) -> Rc<Self> {
        let view = Rc::new(Self {
            window: w.as_weak(),
            polls: Default::default(),
            commands: Default::default(),
            items: std::array::from_fn(|_| Rc::new(VecModel::default())),
            last: Cell::new(-10.0),
            transport_error: Cell::new(false),
            live_frames: Cell::new(false),
            last_gains: Cell::new([[f32::NAN; 4]; 4]),
            moving_until: Cell::new([-10.0; 4]),
            touched: Cell::new([-10.0; 4]),
            clock: Cell::new(0.0),
        });
        let weak = Rc::downgrade(&view);
        w.global::<StemState>().on_action(move |action, deck, id, value| {
            let Some(view)=weak.upgrade() else {return;};
            let Some(window)=view.window.upgrade() else {return;};
            let di=deck_index(&deck);
            if matches!(action.as_str(),"gain"|"mute"|"solo") {
                let mut t=view.touched.get();t[di]=view.clock.get();view.touched.set(t);
                // The knob shows the hand at once: engine frames are held
                // off while it moves, so without this it would not turn.
                use slint::Model;
                let items=&view.items[di];
                if let Some(row)=(0..items.row_count()).find(|r|items.row_data(*r).is_some_and(|x|x.id.as_str()==id.as_str()))
                    && let Some(mut item)=items.row_data(row)
                {
                    match action.as_str(){"gain"=>item.gain=value,"mute"=>item.muted=value!=0.0,_=>item.solo=value!=0.0}
                    items.set_row_data(row,item);
                }
            }
            let req=if matches!(action.as_str(),"gain"|"mute"|"solo") {
                let parameter=match action.as_str(){"gain"=>json!({"parameter":"gain","value":value}),"mute"=>json!({"parameter":"mute","value":value!=0.0}),_=>json!({"parameter":"solo","value":value!=0.0})};
                json!({"op":"command","origin":"user","command":{"op":"dj.stem","deck":deck.as_str(),"stem":id.as_str(),"value":parameter}})
            }else if action == "bridge" || action == "bridge_off" {
                json!({"op":"stem_bridge","source":deck.as_str(),"stem":id.as_str(),"on":action=="bridge"})
            }else if action == "bridge_fx" {
                json!({"op":"stem_bridge","fx":value!=0.0})
            }else if action == "bridge_depth" {
                json!({"op":"stem_bridge","depth":value})
            }else{json!({"op":"stems","deck":deck.as_str(),"action":action.as_str(),"id":id.as_str()})};
            let key=match action.as_str(){
                "gain"|"mute"|"solo"=>{
                    use slint::Model;
                    let items=get_deck(&window.global::<StemState>(),di).items;
                    let slot=(0..items.row_count()).find(|i|items.row_data(*i).is_some_and(|row|row.id.as_str()==id.as_str())).unwrap_or(0).min(7);
                    let param=match action.as_str(){"gain"=>0,"mute"=>1,_=>2};EXTRA_PARAM_KEYS[di*24+slot*3+param]
                }
                _=>action_key(action.as_str(),di)
            };
            if !view.commands.request(key,req){window.global::<StemState>().set_message("Stem command queue is busy; retry".into());}
            view.last.set(-10.0);
        });
        if std::env::var_os("PATCH_STEM_PREVIEW").is_some() {
            let controls=patch_core::stems::CANONICAL.iter().map(|id|json!({"id":id,"gain":1.0,"mute":false,"solo":false,"human":false,"meter":0.35})).collect::<Vec<_>>();
            w.global::<StemState>().set_a(display(&json!({"backend_ready":true,"controls":{"ready":true,"enabled":true,"controls":controls},"set":{"id":"preview"}}), &view.items[0], false));
            w.global::<StemState>().set_b(display(&json!({"backend_ready":true,"source":"preview-b","controls":{"ready":false,"controls":controls},"jobs":[{"id":"preparing","source":"preview-b","state":"preparing","progress":42}]}), &view.items[1], false));
            w.global::<StemState>().set_preview_expanded(true);
        }
        view
    }
    /// Seconds between status reads: five a second while the AI moves a
    /// deck's stems (its faders are seen moving), else one.
    fn every(&self) -> f64 {
        let Some(w) = self.window.upgrade() else {
            return 1.0;
        };
        let state = w.global::<StemState>();
        if (0..4).any(|i| get_deck(&state, i).automating) {
            0.2
        } else {
            1.0
        }
    }
        /// This frame's stem gains and levels from the engine (every frame, as
    /// the EQ knobs): the faders and meters follow them, a fader the DJ is
    /// moving here excepted; a fader the AI moves opens the panel.
    pub fn frame(&self, t: f64, gains: [[f32; 4]; 4], levels: [[f32; 4]; 4]) {
        self.clock.set(t);
        self.live_frames.set(true);
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let state = w.global::<StemState>();
        let last = self.last_gains.replace(gains);
        let mut until = self.moving_until.get();
        for d in 0..4 {
            let items = &self.items[d];
            let hand = t - self.touched.get()[d] < 0.4;
            let moved = (0..4).any(|i| {
                gains[d][i].is_finite() && last[d][i].is_finite() && (gains[d][i] - last[d][i]).abs() > 0.002
            });
            if moved && !hand {
                until[d] = t + 1.5;
            }
            for (i, id) in patch_core::stems::CANONICAL.iter().enumerate() {
                let Some(row) = (0..items.row_count()).find(|r| items.row_data(*r).is_some_and(|x| x.id.as_str() == *id)) else {
                    continue;
                };
                let Some(mut item) = items.row_data(row) else { continue };
                let gain = gains[d][i];
                let mut changed = false;
                if gain.is_finite() && !hand && (item.gain - gain).abs() > 0.0005 {
                    item.gain = gain;
                    changed = true;
                }
                if (item.meter - levels[d][i]).abs() > 0.002 {
                    item.meter = levels[d][i];
                    changed = true;
                }
                if changed {
                    items.set_row_data(row, item);
                }
            }
            let moving = until[d] > t;
            let deck = get_deck(&state, d);
            if deck.automating != moving && deck.attached {
                let mut deck = deck;
                deck.automating = moving || deck.status.as_str() == "AI · moving stems";
                set_deck(&state, d, deck)
            }
        }
        self.moving_until.set(until);
    }
        pub fn alive(&self) -> bool {
        self.window.upgrade().is_some()
    }
    pub fn tick(&self, t: f64, live: bool) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let state = w.global::<StemState>();
        if !live {
            return;
        }
        for (key, deck) in [("stem-a", "a"), ("stem-b", "b"), ("stem-c", "c"), ("stem-d", "d")] {
            if let Some(reply) = self.polls.take(key) {
                match reply.result {
                    Ok(v) => {
                        // The engine answers again: a transport error (a
                        // restart, an update) no longer applies.
                        if self.transport_error.replace(false) {
                            state.set_message("".into());
                        }
                        let items = &self.items[deck_index(deck)];
                        let mut d = display(&v, items, self.live_frames.get());
                        d.automating |= self.moving_until.get()[deck_index(deck)] > t;
                        set_deck(&state, deck_index(deck), d);
                    }
                    Err(e) => {
                        self.transport_error.set(true);
                        state.set_message(e.into());
                    }
                }
            }
            if t - self.last.get() > self.every() {
                self.polls
                    .request(key, json!({"op":"stems","deck":deck,"action":"status"}));
            }
        }
        if t - self.last.get() > self.every() {
            self.polls
                .request("stem-bridge", json!({"op":"stem_bridge"}));
            self.last.set(t);
        }
        if let Some(reply) = self.polls.take("stem-bridge")
            && let Ok(v) = reply.result
        {
            state.set_bridge_source(v["source"].as_str().unwrap_or("").into());
            state.set_bridge_stem(v["stem"].as_str().unwrap_or("").into());
            state.set_bridge_destination(v["destination"].as_str().unwrap_or("").into());
            state.set_bridge_fx(v["fx"].as_bool().unwrap_or(false));
            state.set_bridge_depth(v["depth"].as_f64().unwrap_or(0.5) as f32);
        }
        for key in PARAM_KEYS
            .iter()
            .chain(EXTRA_PARAM_KEYS.iter())
            .chain(ACTION_KEYS.iter())
            .copied()
        {
            if let Some(r) = self.commands.take(key) {
                state.set_message(match r.result {
                    Ok(v) => v
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| {
                            v["message"]
                                .as_str()
                                .unwrap_or("Stem control updated")
                                .to_owned()
                        })
                        .into(),
                    Err(e) => short(&e).into(),
                });
                self.last.set(-10.0);
            }
        }
    }
}
fn display(v: &Value, items_model: &Rc<VecModel<StemItem>>, keep_live: bool) -> StemDeck {
    let controls = &v["controls"];
    let attached = controls["ready"] == true;
    let job = v["jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|j| j["source"] == v["source"])
        .max_by_key(|j| j["started"].as_u64().unwrap_or(0));
    let cached = v["sets"].as_array().into_iter().flatten().find(|s| {
        s["resident"] == true
            && (s["set"]["track_id"] == v["track_id"]
                || s["aliases"]
                    .as_array()
                    .is_some_and(|a| a.contains(&v["track_id"])))
    });
    let key = v["set"]["id"]
        .as_str()
        .or_else(|| cached.and_then(|c| c["set"]["id"].as_str()))
        .unwrap_or("");
    let preparing =
        job.is_some_and(|j| matches!(j["state"].as_str(), Some("queued" | "preparing")));
    let complete = |c: &Value| {
        let set = &c["set"];
        set["frames"].as_u64().is_some_and(|frames| {
            patch_core::stems::CANONICAL.iter().all(|id| {
                set["stems"][*id]["start_frame"].as_u64() == Some(0)
                    && set["stems"][*id]["frames"].as_u64() == Some(frames)
            })
        })
    };
    let ready = attached || cached.is_some_and(complete);
    // The AI moving this deck's stems (a transition's stem moves armed or
    // under way): no DJ hand on them and some away from full or queued.
    let automating = attached
        && controls["controls"].as_array().is_some_and(|c| {
            c.iter().all(|s| s["human"] != true)
                && c.iter().any(|s| {
                    s["automation_armed"] == true
                        || controls["enabled"] == true
                            && (s["gain"].as_f64().unwrap_or(1.0) - 1.0).abs() > 0.01
                })
        });
    let partial = cached.is_some_and(|c| !complete(c));
    let status = if v["setup"]["state"] == "preparing" {
        format!("Installing separator {}%", v["setup"]["progress"])
    } else if v["setup"]["state"] == "failed" && v["backend_ready"] != true {
        format!(
            "Setup failed: {}",
            v["setup"]["error"].as_str().unwrap_or("retry")
        )
    } else if v["backend_ready"] != true {
        "Separator not installed · Ctrl+K → Setup".into()
    } else if let Some(error) = v["error"].as_str() {
        unavailable(error)
    } else if automating {
        "AI · moving stems".into()
    } else if attached {
        if controls["enabled"] == true {
            "Ready · stems".into()
        } else {
            "Ready · original".into()
        }
    } else if let Some(job) = job.filter(|_| preparing) {
        let action = if job["resumed"] == true { "Resuming" } else { "Preparing" };
        format!("{action} {}%", job["progress"])
    } else if let Some(j) = job.filter(|j| j["state"] == "failed") {
        unavailable(j["error"].as_str().unwrap_or(""))
    } else if partial {
        "Partial stems · complete layout required to use".into()
    } else if ready {
        "Ready".into()
    } else {
        "Original mix".into()
    };
    let items = controls["controls"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            let id = s["id"].as_str().unwrap_or("stem");
            StemItem {
                id: (*id).into(),
                name: format!("{}{}", id[..1].to_uppercase(), &id[1..]).into(),
                gain: s["gain"].as_f64().unwrap_or(1.0) as f32,
                muted: s["mute"] == true,
                solo: s["solo"] == true,
                meter: s["meter"].as_f64().unwrap_or(0.0) as f32,
                human: s["human"] == true,
            }
        })
        .collect::<Vec<_>>();
    // Keep the repeater model stable across status polls. Replacing it every
    // second cancelled pointer capture during a drag and needlessly rebuilt
    // all stem controls in every Booth/Deck window.
    // Live frames own gain and level; a status read only adds the rest.
    let items: Vec<StemItem> = items
        .into_iter()
        .map(|mut item| {
            if keep_live
                && let Some(row) = (0..items_model.row_count())
                    .filter_map(|i| items_model.row_data(i))
                    .find(|r| r.id == item.id)
            {
                item.gain = row.gain;
                item.meter = row.meter;
            }
            item
        })
        .collect();
    if items_model.row_count() != items.len() {
        items_model.set_vec(items);
    } else {
        for (i, item) in items.into_iter().enumerate() {
            if items_model.row_data(i).as_ref() != Some(&item) {
                items_model.set_row_data(i, item);
            }
        }
    }
    let batch = v["batches"].as_array().into_iter().flatten().find(|b| {
        matches!(
            b["state"].as_str(),
            Some("queued" | "preparing" | "submitted")
        )
    });
    let batch_total = batch.and_then(|b| b["total"].as_u64()).unwrap_or(0);
    let batch_done = batch.and_then(|b| b["complete"].as_u64()).unwrap_or(0);
    let batch_action = if batch.is_some_and(|b| b["resumed"] == true) {
        " · resumed"
    } else {
        ""
    };
    StemDeck {
        automating,
        batch_id: batch.map_or_else(|| "".into(), |b| b["id"].as_str().unwrap_or("").into()),
        batch_status: format!("{batch_done} / {batch_total} ready{batch_action}").into(),
        backend_ready: v["backend_ready"] == true,
        partial,
        installing: v["setup"]["state"] == "preparing",
        status: status.into(),
        key: key.into(),
        job: if preparing {
            job.unwrap()["id"].as_str().unwrap_or("").into()
        } else {
            "".into()
        },
        ready,
        attached,
        enabled: controls["enabled"] == true,
        pinned: cached.is_some_and(|c| c["pins"].as_array().is_some_and(|p| !p.is_empty())),
        items: ModelRc::from(items_model.clone()),
    }
}

/// What a deck says when its stems could not be made: a plain "not
/// available", with a reason only where one is clear. The separator's own
/// output (a Python traceback, up to 1500 characters) stays in the job's
/// `adapter.log`; on a deck it read as an error dump.
fn unavailable(error: &str) -> String {
    let e = error.to_ascii_lowercase();
    let why = if e.contains("outofmemory") || e.contains("out of memory") {
        // The GPU or RAM is taken, often by another program (a video
        // render): nothing is wrong with the track.
        " · the graphics card is busy"
    } else if e.contains("stale") {
        " · the track changed"
    } else {
        ""
    };
    format!("Stems not available{why}")
}

/// A command's error as one line: a separator dump becomes "not available".
fn short(error: &str) -> String {
    if error.contains('\n') || error.len() > 120 || error.starts_with("Separator failed") {
        unavailable(error)
    } else {
        error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_separation_reads_as_not_available() {
        let oom = "Separator failed (exit status: 1): Traceback (most recent call last):\n  File \"demucs_adapter.py\", line 22\ntorch.OutOfMemoryError: CUDA out of memory.";
        assert_eq!(unavailable(oom), "Stems not available · the graphics card is busy");
        assert_eq!(short(oom), "Stems not available · the graphics card is busy");
        assert_eq!(unavailable("Separator failed (exit status: 2): boom"), "Stems not available");
        assert_eq!(unavailable("stale stem set"), "Stems not available · the track changed");
        assert_eq!(unavailable(""), "Stems not available");
        // A short, plain message from a command stays as it is.
        assert_eq!(short("engine is not running"), "engine is not running");
    }
}
