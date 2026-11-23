//! Unified workspace presentation. All I/O runs on bounded background workers.
use crate::ui::{DjWorkspaceState, PatchWindow, WorkspaceItem};
use patch_core::dj_workspace::{PreparedSet, PreparedTrack, SessionAction};
use serde_json::{Value, json};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How long a notice stays on the banner before it clears itself.
const NOTICE: Duration = Duration::from_secs(6);

/// Windows that carry the DJ Booth's set controls: the Booth and the Mini
/// Booth (its AI DJ sheet starts, pauses and ends the same sets).
pub fn is_workspace(id: &str) -> bool {
    id == "dj" || id == "dj-mini"
}
pub struct View {
    window: slint::Weak<PatchWindow>,
    polls: crate::poll::Poller,
    reads: crate::poll::Poller,
    draft: RefCell<PreparedSet>,
    data: RefCell<Value>,
    selected: RefCell<HashSet<String>>,
    action: RefCell<String>,
    busy: Cell<bool>,
    last: Cell<f64>,
    /// When the banner's notice was shown; `tick` clears it after `NOTICE`.
    shown: Cell<Option<Instant>>,
    capture: RefCell<Option<String>>,
    undo: RefCell<Option<(usize, String)>>,
}
fn item(
    id: impl Into<slint::SharedString>,
    title: impl Into<slint::SharedString>,
    detail: impl Into<slint::SharedString>,
    selected: bool,
) -> WorkspaceItem {
    WorkspaceItem {
        id: id.into(),
        title: title.into(),
        detail: detail.into(),
        selected,
    }
}
fn rows(v: Vec<WorkspaceItem>) -> ModelRc<WorkspaceItem> {
    Rc::new(VecModel::from(v)).into()
}
fn collection_summary(collections: &[Value], ids: &BTreeSet<i64>) -> String {
    let chosen: Vec<String> = collections
        .iter()
        .filter(|c| c["id"].as_i64().is_some_and(|id| ids.contains(&id)))
        .map(|c| {
            let name = field(c, "name");
            match c["count"].as_u64() {
                Some(count) => format!("{name} · {count} tracks"),
                None => name,
            }
        })
        .collect();
    match chosen.as_slice() {
        [] => "all library tracks".into(),
        [one] => one.clone(),
        [first, second] => format!("{first} + {second}"),
        [first, second, rest @ ..] => format!("{first} + {second} + {} more collections", rest.len()),
    }
}
fn title(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.into())
}
fn field(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().into()
}
/// Store `new` in `slot`; false when it already held that value.
fn replace_if_changed(slot: &mut Value, new: Value) -> bool {
    let changed = *slot != new;
    if changed {
        *slot = new;
    }
    changed
}

fn read(req: &Value) -> Result<Value, String> {
    match req["local"].as_str() {
        Some("storage") => patch_engine::workspace::storage(req),
        Some("take") => {
            let path = std::path::Path::new(req["path"].as_str().ok_or("No take selected")?);
            let sidecar = path.with_extension("dj.json");
            let v = std::fs::read(&sidecar)
                .map_err(|_| "This older take has no DJ metadata sidecar".to_owned())?;
            serde_json::from_slice(&v).map_err(|e| e.to_string())
        }
        Some("export") => export(req),
        Some("export_take") => export_take(req),
        _ => {
            let result = patch_engine::client::call(req)?;
            let action = req["command"]["request"]["action"].as_str().unwrap_or("");
            if matches!(action, "end" | "stop_now" | "pause" | "resume")
                && patch_record::daemon::status().is_some_and(|s| s["source"] == "program:mix")
            {
                if matches!(action, "end" | "stop_now") {
                    patch_record::daemon::stop_and_wait()
                        .map_err(|e| format!("Set stopped; recording finalization: {e}"))?;
                } else if !patch_record::daemon::pause(action == "pause") {
                    return Err(
                        "Set transport changed, but recorder did not acknowledge pause/resume"
                            .into(),
                    );
                }
            }
            Ok(result)
        }
    }
}
fn export(req: &Value) -> Result<Value, String> {
    let v = patch_engine::workspace::storage(&json!({"action":"session_detail","id":req["id"]}))?;
    let id = req["id"].as_str().ok_or("Select a performed set")?;
    let dir = patch_engine::session::state_dir().join("exports");
    let path = dir.join(format!("{id}.json"));
    patch_engine::workspace::atomic_json(&path, &v)?;
    let mut list =
        String::from("Performed set · archived elapsed times (not recording sample chapters)\n");
    for e in v["log"].as_array().into_iter().flatten() {
        list += &format!(
            "{} · {} — {} · {}\n",
            crate::dj::clock(e["at"].as_f64().unwrap_or(0.0)),
            field(e, "artist"),
            field(e, "title"),
            field(e, "style")
        );
    }
    std::fs::write(path.with_extension("txt"), list).map_err(|e| e.to_string())?;
    Ok(json!({"message":format!("Exported JSON and tracklist to {}",dir.display())}))
}
fn export_take(req: &Value) -> Result<Value, String> {
    let path = std::path::Path::new(req["path"].as_str().ok_or("Select a take")?);
    let bytes = std::fs::read(path.with_extension("dj.json"))
        .map_err(|_| "No sample-aligned metadata exists for this take".to_owned())?;
    let v: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let rate = v["rate"]
        .as_f64()
        .filter(|r| *r > 0.0)
        .ok_or("Missing recording sample rate")?;
    let mut chapters = vec![];
    let initial = &v["initial"];
    let d = usize::from(initial["xfader"].as_f64().unwrap_or(0.0) > 0.5);
    if let Some(p) = initial["decks"][d]["path"]
        .as_str()
        .filter(|p| !p.is_empty())
    {
        chapters.push((0u64, title(p)));
    }
    for t in v["transitions"].as_array().into_iter().flatten() {
        if let Some(at) = t["recording_handover"].as_u64() {
            chapters.push((at, title(t["to"].as_str().unwrap_or(""))));
        }
    }
    chapters.sort_by_key(|c| c.0);
    chapters.dedup_by_key(|c| c.0);
    let mut txt =
        String::from("Recording tracklist · chapters at completed crossfader handovers\n");
    let escaped = |s: &str| s.replace(['"', '\n', '\r'], " ");
    let mut cue = format!(
        "FILE \"{}\" WAVE\n",
        escaped(&path.file_name().unwrap_or_default().to_string_lossy())
    );
    for (i, (sample, name)) in chapters.iter().enumerate() {
        let at = *sample as f64 / rate;
        txt += &format!("{}  {}\n", crate::dj::clock(at), name);
        let frame = (at * 75.0).floor() as u64;
        cue += &format!(
            "  TRACK {:02} AUDIO\n    TITLE \"{}\"\n    INDEX 01 {:02}:{:02}:{:02}\n",
            i + 1,
            escaped(name),
            frame / 4500,
            (frame / 75) % 60,
            frame % 75
        );
    }
    std::fs::write(path.with_extension("txt"), txt).map_err(|e| e.to_string())?;
    std::fs::write(path.with_extension("cue"), cue).map_err(|e| e.to_string())?;
    Ok(
        json!({"message":format!("Exported sample-aligned TXT and CUE tracklists beside {}",path.display())}),
    )
}

fn describe(kind: &str, v: &Value) -> String {
    let mut lines = vec![];
    match kind {
        "session_detail" => {
            lines.push(format!(
                "{} · {} tracks",
                v["name"].as_str().unwrap_or("Performed set"),
                v["tracks"]
            ));
            for e in v["log"].as_array().into_iter().flatten() {
                lines.push(format!(
                    "{}  {} — {} · {}",
                    crate::dj::clock(e["at"].as_f64().unwrap_or(0.0)),
                    field(e, "artist"),
                    field(e, "title"),
                    field(e, "style")
                ));
            }
            lines.push(
                "Archived elapsed times; audio takes have their own precise sample timeline."
                    .into(),
            );
        }
        "report_detail" => {
            lines.push(format!(
                "{} → {}",
                title(v["transition"]["from"].as_str().unwrap_or("")),
                title(v["transition"]["to"].as_str().unwrap_or(""))
            ));
            lines.push(format!(
                "Reported: {} · {}",
                field(v, "why"),
                field(v, "notes")
            ));
            lines.push(format!(
                "Technique: {}",
                field(&v["transition"], "technique")
            ));
            lines.push("Select this report and Replay selected to inspect timing, level and overlap evidence.".into());
        }
        "job_detail" => {
            lines.push(format!("{} · {}", field(v, "status"), field(v, "message")));
            for r in v["results"].as_array().into_iter().flatten() {
                let m = &r["metrics"];
                lines.push(if r["status"]=="unreplayable" {format!("{}: cannot replay — {}",field(r,"id"),field(r,"error"))} else {format!("{}: {:.1}/100 · {} · kick timing {} ms · mids overlap {} s · level bump {} dB · dropouts {}",field(r,"id"),m["score"].as_f64().unwrap_or(0.0),if r["acceptable"]==true {"passed"} else {"needs attention"},m["flam_ms"],m["mids_clash_s"],m["level_bump_db"],m["dropouts"])});
            }
            for (label, key) in [
                ("Baseline tuning", "training"),
                ("Baseline held-out", "evaluation"),
            ] {
                if let Some(rs) = v["baseline"][key].as_array() {
                    let score = rs.iter().filter_map(|r| r["score"].as_f64()).sum::<f64>()
                        / rs.len().max(1) as f64;
                    lines.push(format!("{label}: {score:.1}/100 across {} pairs", rs.len()));
                }
            }
            for (i, c) in v["comparisons"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                lines.push(format!(
                    "Candidate {}: {}",
                    i + 1,
                    if c["passed"] == true {
                        "passed independent evaluation"
                    } else {
                        "did not pass"
                    }
                ));
                for (label, key) in [("Tuning", "training"), ("Held-out", "evaluation")] {
                    let rs: Vec<_> = c[key].as_array().into_iter().flatten().collect();
                    let score = rs.iter().filter_map(|m| m["score"].as_f64()).sum::<f64>()
                        / rs.len().max(1) as f64;
                    lines.push(format!(
                        "  {label}: {:.1}/100 across {} pairs",
                        score,
                        rs.len()
                    ));
                }
            }
            if !v["candidate"].is_null() {
                for (key, label) in [
                    ("cap_at", "Layer confidence limits"),
                    ("cut_bias", "Cut preference"),
                    ("space_mid", "Room for incoming melody"),
                    ("fade_scale", "Fade duration scale"),
                ] {
                    let before = &v["baseline"]["tuning"][key];
                    let after = &v["candidate"][key];
                    if before != after {
                        lines.push(format!("{label}: {before} → {after}"));
                    }
                }
            }
            lines.push("Offline replay is reconstructed evidence. Listen before activating; a higher score alone does not establish better sound.".into());
        }
        "take_detail" => {
            lines.push(format!(
                "{} · {} Hz · {} samples · {}",
                v["name"].as_str().unwrap_or("DJ take"),
                v["rate"],
                v["frames"],
                field(v, "format")
            ));
            lines.push(format!(
                "Session: {} · {}",
                field(v, "session_id"),
                field(v, "timing")
            ));
            for t in v["transitions"].as_array().into_iter().flatten() {
                let at = t["recording_handover"]
                    .as_f64()
                    .zip(v["rate"].as_f64())
                    .map(|(s, r)| crate::dj::clock(s / r))
                    .unwrap_or_else(|| "outside take / paused".into());
                lines.push(format!(
                    "{at}  {} → {} · {}",
                    title(t["from"].as_str().unwrap_or("")),
                    title(t["to"].as_str().unwrap_or("")),
                    field(t, "technique")
                ));
            }
            if let Some(error) = v["error"].as_str() {
                lines.push(format!("Partial take: {error}"));
            }
        }
        _ => lines.push(serde_json::to_string_pretty(v).unwrap_or_default()),
    }
    lines.join("\n")
}

impl View {
    pub fn bind(w: &PatchWindow) -> Rc<Self> {
        let view = Rc::new(Self {
            window: w.as_weak(),
            polls: Default::default(),
            reads: Default::default(),
            draft: Default::default(),
            data: Default::default(),
            selected: Default::default(),
            action: Default::default(),
            busy: Cell::new(false),
            last: Cell::new(-10.0),
            shown: Cell::new(None),
            capture: Default::default(),
            undo: Default::default(),
        });
        let a = w.global::<DjWorkspaceState>();
        let weak = Rc::downgrade(&view);
        a.on_action(move |kind, id| {
            if let Some(v) = weak.upgrade()
                && let Err(e) = v.act(kind.as_str(), id.as_str())
            {
                v.message(&e);
            }
        });
        if patch_host::shot::active() {
            if let Ok(tab) = std::env::var("PATCH_DJ_WORKSPACE_TAB")
                && ["play", "prepare", "recordings", "reports"].contains(&tab.as_str())
            {
                a.set_tab(tab.into());
            }
            view.data.borrow_mut()["style"] = patch_core::mix_style::MixStyle::default().json();
            let mut tempo = patch_core::tempo_plan::TempoPlan {
                mode: patch_core::tempo_plan::TempoMode::Hold,
                bpm: 124.0,
            }
            .json();
            tempo["playing_bpm"] = json!(125.3);
            tempo["target_bpm"] = json!(124.0);
            view.data.borrow_mut()["tempo"] = tempo;
            view.data.borrow_mut()["coverage"] = json!({"tracks": 40, "ready": 12, "avg_track_s": 195.0, "play_percent": 100, "minutes_per_track": 3.25, "avg_track_minutes": 3.25});
            view.set_draft(PreparedSet::default());
        }
        view
    }
    pub fn alive(&self) -> bool {
        self.window.upgrade().is_some()
    }
    pub fn show_message(&self, text: &str) {
        self.message(text);
    }
    fn message(&self, text: &str) {
        if let Some(w) = self.window.upgrade() {
            w.global::<DjWorkspaceState>().set_message(text.into());
            self.shown.set((!text.is_empty()).then(Instant::now));
        }
    }
    fn queue(&self, kind: &str, request: Value) -> Result<(), String> {
        if self.busy.get() {
            return Err("Wait for the current action to finish".into());
        }
        if !self.polls.request_with("action", request, read) {
            return Err("Workspace worker busy; try again".into());
        }
        self.busy.set(true);
        *self.action.borrow_mut() = kind.into();
        self.message("Working…");
        Ok(())
    }
    fn command(&self, kind: &str, request: SessionAction) -> Result<(), String> {
        self.queue(kind,json!({"op":"command","origin":"user","command":patch_core::command::Command::DjSession { request }}))
    }
    /// The collections the set draws on: the live set's while one plays,
    /// else the draft's (none: the whole library).
    fn stem_collections(&self) -> Vec<i64> {
        let data = self.data.borrow();
        let live = &data["live"]["set"];
        if live["on"] == true {
            let ids: Vec<i64> = live["collections"].as_array().into_iter().flatten().filter_map(Value::as_i64).collect();
            if !ids.is_empty() { return ids; }
        }
        self.draft.borrow().collections.clone()
    }
    fn edited(&self) -> Result<PreparedSet, String> {
        let w = self.window.upgrade().ok_or("Window closed")?;
        let a = w.global::<DjWorkspaceState>();
        let mut d = self.draft.borrow().clone();
        d.name = a.get_name().trim().into();
        d.notes = a.get_notes().into();
        d.arc = a.get_arc().into();
        d.mode = serde_json::from_value(json!(a.get_mode().as_str())).map_err(|e| e.to_string())?;
        d.minutes = if a.get_minutes().trim().is_empty() {
            None
        } else {
            Some(
                a.get_minutes()
                    .trim()
                    .parse()
                    .map_err(|_| "Enter a valid duration")?,
            )
        };
        let (lo, hi) = (a.get_bpm_low(), a.get_bpm_high());
        d.bpm = if lo.trim().is_empty() && hi.trim().is_empty() {
            None
        } else {
            Some((
                lo.trim().parse().map_err(|_| "Enter minimum BPM")?,
                hi.trim().parse().map_err(|_| "Enter maximum BPM")?,
            ))
        };
        d.validate()?;
        Ok(d)
    }
    fn set_draft(&self, d: PreparedSet) {
        if let Some(w) = self.window.upgrade() {
            let a = w.global::<DjWorkspaceState>();
            a.set_name(d.name.clone().into());
            a.set_notes(d.notes.clone().into());
            a.set_minutes(d.minutes.map(|v| v.to_string()).unwrap_or_default().into());
            a.set_bpm_low(d.bpm.map(|v| v.0.to_string()).unwrap_or_default().into());
            a.set_bpm_high(d.bpm.map(|v| v.1.to_string()).unwrap_or_default().into());
            a.set_arc(d.arc.clone().into());
            a.set_mode(
                serde_json::to_value(d.mode)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_else(|| "suggest".into())
                    .into(),
            );
            a.set_dirty(false);
        }
        *self.draft.borrow_mut() = d;
        self.push();
    }
    pub fn act(&self, kind: &str, id: &str) -> Result<(), String> {
        let w = self.window.upgrade().ok_or("Window closed")?;
        let a = w.global::<DjWorkspaceState>();
        match kind {
            "tab"|"refresh"|"filter" => { a.set_details("".into()); a.set_selected_id("".into()); self.last.set(-10.0); self.push(); Ok(()) }
            "new" => { if a.get_dirty() { return Err("Save your changes before creating a new draft".into()); } self.set_draft(PreparedSet::default()); Ok(()) }
            "save"|"save_as" => { let mut d=self.edited()?; if kind=="save_as" { d.id.clear();d.revision=0;d.deleted=false; } self.queue("save",json!({"local":"storage","action":"save","set":d})) }
            "load"|"duplicate"|"trash"|"restore" => {
                if kind=="load" && a.get_dirty() { return Err("Save your changes before loading another set".into()); }
                self.queue(kind,json!({"local":"storage","action":kind,"id":id}))
            }
            "readiness" => self.queue(kind,json!({"local":"storage","action":kind,"set":self.edited()?})),
            "start" => self.command(kind,SessionAction::Start { set:self.edited()? }),
            "fast_mode" => self.command(kind,SessionAction::FastMode { on:id=="on" }),
            "levels" => self.queue(kind,json!({"op":"levels","mode":id})),
            "pause"=>self.command(kind,SessionAction::Pause), "resume"=>self.command(kind,SessionAction::Resume),
            "end"=>self.command(kind,SessionAction::End), "stop_now"=>self.command(kind,SessionAction::StopNow),
            "direction"=>self.command(kind,SessionAction::Direction {direction:serde_json::from_value(json!(id)).map_err(|e|e.to_string())?}),
            "change_next"=>self.command(kind,SessionAction::ChangeNext),
            "prepare_stems"=>{
                let collections=self.stem_collections();
                if collections.is_empty() { return Err("Pick one or more collections under Sources first: stems are prepared per collection, not for the whole library at once".into()); }
                self.queue(kind,json!({"op":"stems","deck":"a","action":"prepare_collections","collections":collections}))
            }
            "style"=>self.queue(kind,json!({"op":"mix_style","style":id})),
            "tempo_mode"=>self.queue("tempo",json!({"op":"tempo_plan","mode":id})),
            "tempo_bpm_step"=>self.queue("tempo",json!({"op":"tempo_plan","bpm_step":id.parse::<f64>().map_err(|_|"Invalid BPM step")?})),
            "tempo_hold_now"=>{
                let now=self.data.borrow()["tempo"]["playing_bpm"].as_f64().ok_or("Nothing plays: no tempo to hold")?;
                self.queue("tempo",json!({"op":"tempo_plan","mode":"hold","bpm":(now*2.0).round()/2.0}))
            }
            "style_switch"=>{
                let on=self.data.borrow()["style"][id].as_bool().ok_or("The transition style has not loaded yet")?;
                self.queue(kind,json!({"op":"mix_style",id:!on}))
            }
            "play_percent"=>self.queue(kind,json!({"op":"mix_style","source":"the DJ Booth's PLAYS row","play_percent":id.parse::<u64>().map_err(|_|"Invalid play length")?})),
            "stem_prep"=>self.queue(kind,json!({"op":"mix_style","stem_prep":id})),
            "set_prep_stems"=>{
                let ids:Vec<i64>=self.data.borrow()["coverage"]["set_preps"].as_array().into_iter().flatten().filter_map(|p|p["id"].as_i64()).collect();
                if ids.is_empty() { return Err("No set prep among this set's sources".into()); }
                self.queue(kind,json!({"op":"set_prep_stems","collections":ids,"on":id=="on"}))
            }
            "play_step"=>{
                let now=self.data.borrow()["style"]["play_percent"].as_i64().ok_or("The transition style has not loaded yet")?;
                let step:i64=id.parse().map_err(|_|"Invalid play length step")?;
                self.queue(kind,json!({"op":"mix_style","source":"the DJ Booth's PLAYS −/+","play_percent":(now+step).clamp(20,100)}))
            }
            "sources"=>self.command(kind,SessionAction::Sources { collections:self.draft.borrow().collections.clone() }),
            "mix"=>self.queue(kind,json!({"op":"command","origin":"user","command":patch_core::command::Command::TransitionNow})),
            "queue_remove"=>{
                let index:usize=id.parse().map_err(|_|"Invalid queue row")?;
                let expected=self.data.borrow()["live"]["queue"][index].as_str().ok_or("Queue changed; refresh and retry")?.to_owned();
                self.command(kind,SessionAction::QueueRemove {index,expected:expected.clone()})?;*self.undo.borrow_mut()=Some((index,expected));Ok(())
            }
            "queue_undo"=>{
                let (at,path)=self.undo.borrow().clone().ok_or("Nothing to undo")?;
                self.queue(kind,json!({"op":"command","origin":"user","command":patch_core::command::Command::QueueAdd {path,at:Some(at)}}))
            }
            "queue_up"|"queue_down" => { let from:usize=id.parse().map_err(|_|"Invalid queue row")?; let to=if kind=="queue_up" { from.saturating_sub(1) } else { from+1 }; self.command(kind,SessionAction::QueueMove { from,to }) }
            "collection"|"all_collections"=>{
                let mut d=self.draft.borrow_mut();
                if kind=="all_collections" {d.collections.clear();} else {let id:i64=id.parse().map_err(|_|"Invalid collection")?;if let Some(i)=d.collections.iter().position(|v|*v==id) {d.collections.remove(i);} else {d.collections.push(id);}}
                a.set_dirty(true);drop(d);self.push();Ok(())
            }
            "add_path"=>{
                if id.trim().is_empty() { return Err("Enter a local audio path".into()); }
                let path=id.trim().to_owned();
                self.draft.borrow_mut().tracks.push(PreparedTrack {id:None,track_uuid:None,path,pinned:false}); a.set_track_path("".into());a.set_dirty(true);self.push();Ok(())
            }
            "use_queue"=>self.queue(kind,json!({"op":"dj"})),
            "pin"|"draft_up"|"draft_down"|"draft_remove"=>{
                let i:usize=id.parse().map_err(|_|"Invalid track row")?;let mut d=self.draft.borrow_mut();
                if i>=d.tracks.len() {return Err("Track order changed; retry".into());}
                match kind {"pin"=>d.tracks[i].pinned = !d.tracks[i].pinned,"draft_remove"=>{d.tracks.remove(i);},_=>{let to=if kind=="draft_up" {i.saturating_sub(1)} else {(i+1).min(d.tracks.len()-1)};d.tracks.swap(i,to);}}
                a.set_dirty(true);drop(d);self.push();Ok(())
            }
            "capture_previous"=> { if a.get_selected_transition().is_empty(){let id=w.global::<crate::ui::AiDjState>().get_previous_id();*self.capture.borrow_mut()=(!id.is_empty()).then(||id.into());} Ok(()) }
            "transition_older"|"transition_newer"=>{
                let data=self.data.borrow();let ts=data["live"]["set"]["transitions"].as_array().ok_or("No transition history yet")?;
                if ts.is_empty(){return Err("No transition history yet".into());}
                let current=self.capture.borrow().clone();let at=ts.iter().position(|t|t["id"].as_str()==current.as_deref()).unwrap_or(ts.len()-1);
                let next=if kind=="transition_older"{at.saturating_sub(1)}else{(at+1).min(ts.len()-1)};
                let id=field(&ts[next],"id");drop(data);self.act("transition",&id)
            }
            "transition"=>{
                let data=self.data.borrow();let t=data["live"]["set"]["transitions"].as_array().into_iter().flatten().find(|t|t["id"]==id).ok_or("Transition unavailable")?;
                *self.capture.borrow_mut()=Some(id.into());a.set_selected_transition(format!("{} → {} · {}",title(t["from"].as_str().unwrap_or("")),title(t["to"].as_str().unwrap_or("")),field(t,"technique")).into());a.set_details(format!("Executed transition · {}",field(t,"technique")).into());Ok(())
            }
            "report"=>{
                let capture=self.capture.borrow().clone().ok_or("Wait for transition history to load, then open Report again")?;
                self.queue(kind,json!({"op":"report","why":id,"notes":a.get_report_notes().as_str(),"transition_id":capture}))
            }
            "select_report"=>{let mut selected=self.selected.borrow_mut();if !selected.remove(id){selected.insert(id.into());}drop(selected);self.push();Ok(())}
            "select_reports"=>{let ids:Vec<String>=self.data.borrow()["reports"].as_array().into_iter().flatten().filter_map(|v|v["file_id"].as_str().map(String::from)).take(24).collect();*self.selected.borrow_mut()=ids.into_iter().collect();self.push();Ok(())}
            "process"=>{let ids:Vec<_>=self.selected.borrow().iter().cloned().collect();self.queue(kind,json!({"op":"workspace","action":"process","ids":ids,"improve":id=="improve"}))}
            "retry"=>{let data=self.data.borrow();let j=data["jobs"].as_array().into_iter().flatten().find(|v|v["id"]==id).ok_or("Job not found")?;self.queue("process",json!({"op":"workspace","action":"process","ids":j["reports"],"improve":j["improve"]}))}
            "cancel_job"|"activate"|"rollback"=>self.queue(kind,json!({"op":"workspace","action":kind,"id":id})),
            "session_detail"|"report_detail"|"job_detail"=>{a.set_selected_id(if kind=="session_detail"{id.into()}else{"".into()});a.set_candidate_ready(false);a.set_selected_job(if kind=="job_detail"{id.into()}else{"".into()});self.queue(kind,json!({"local":"storage","action":kind,"id":id}))}
            "export_take"=>self.queue(kind,json!({"local":"export_take","path":id})),
            "export_session"=>self.queue(kind,json!({"local":"export","id":id})),
            "take_detail"=>{a.set_selected_id("".into());self.queue(kind,json!({"local":"take","path":id}))}
            _=>Err("Unknown workspace action".into()),
        }
    }
    pub fn tick(&self, t: f64, live: bool) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let a = w.global::<DjWorkspaceState>();
        a.set_live(live);
        a.set_busy(self.busy.get());
        // A notice fades once read; "Working…" stays until the reply.
        if !self.busy.get() && self.shown.get().is_some_and(|at| at.elapsed() >= NOTICE) {
            self.message("");
        }
        if let Some(reply) = self.polls.take("action") {
            self.busy.set(false);
            a.set_busy(false);
            self.last.set(-10.0);
            let kind = self.action.borrow().clone();
            match reply.result {
                Err(e) => {
                    if kind == "queue_remove" {
                        self.undo.borrow_mut().take();
                        a.set_undo_ready(false);
                    }
                    self.message(&e);
                }
                Ok(v) => {
                    if kind == "queue_remove" {
                        a.set_undo_ready(true);
                    } else if kind == "queue_undo" {
                        self.undo.borrow_mut().take();
                        a.set_undo_ready(false);
                    }
                    if matches!(kind.as_str(), "style" | "style_switch" | "play_percent" | "play_step") {
                        self.message(&format!("Transitions: {}", field(&v, "summary")));
                        self.data.borrow_mut()["style"] = v;
                        self.push();
                    } else if kind == "prepare_stems" {
                        self.message(v["message"].as_str().unwrap_or("Stem preparation queued for this set"));
                    } else if kind == "save" {
                        if let Ok(d) = serde_json::from_value(v.clone()) {
                            self.set_draft(d);
                        }
                        self.message("Set saved");
                    } else if matches!(kind.as_str(), "load" | "duplicate") {
                        if let Ok(d) = serde_json::from_value(v["set"].clone()) {
                            self.set_draft(d);
                        }
                        self.message(&format!("Draft loaded · {}", v["readiness"]));
                    } else if kind == "use_queue" {
                        let tracks: Vec<_> = v["queue"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|p| p.as_str())
                            .map(|p| PreparedTrack {
                                id: None,
                                track_uuid: None,
                                path: p.into(),
                                pinned: false,
                            })
                            .collect();
                        self.draft.borrow_mut().tracks = tracks;
                        a.set_dirty(true);
                        self.push();
                        self.message("Draft order copied from the queue");
                    } else if matches!(
                        kind.as_str(),
                        "session_detail" | "report_detail" | "job_detail" | "take_detail"
                    ) {
                        a.set_details(describe(&kind, &v).into());
                        a.set_candidate_ready(
                            kind == "job_detail" && v["status"] == "evaluation_passed",
                        );
                        let path = v["results"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find_map(|r| r["replay"].as_str())
                            .unwrap_or("");
                        a.set_replay_path(path.into());
                        let mut auditions: Vec<_> = v["results"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|r| {
                                r["replay"].as_str().map(|p| {
                                    item(
                                        p,
                                        format!("Report {}", field(r, "id")),
                                        "Offline replay",
                                        false,
                                    )
                                })
                            })
                            .collect();
                        auditions.extend(
                            v["auditions"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|r| {
                                    r["path"].as_str().map(|p| {
                                        item(p, field(r, "title"), "Compare this pair", false)
                                    })
                                }),
                        );
                        a.set_auditions(rows(auditions));
                        a.set_linked_takes(rows(
                            v["takes"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|t| {
                                    t["path"].as_str().map(|p| {
                                        item(
                                            p,
                                            title(p),
                                            if std::path::Path::new(p).is_file() {
                                                "Audio available"
                                            } else {
                                                "Missing audio"
                                            },
                                            false,
                                        )
                                    })
                                })
                                .collect(),
                        ));
                    } else if kind == "levels" {
                        a.set_levels(v["mode"].as_str().unwrap_or("").into());
                        let mut name = v["mode"].as_str().unwrap_or("").to_owned();
                        if let Some(c) = name.get_mut(..1) {
                            c.make_ascii_uppercase();
                        }
                        let lufs = v["lufs"].as_f64().map(|l| format!(" · −{:.0} LUFS", l.abs())).unwrap_or_default();
                        self.message(&format!("Levels: {name}{lufs}"));
                    } else if kind == "tempo" {
                        self.message(&format!("Tempo: {}", field(&v, "summary")));
                        self.data.borrow_mut()["tempo"] = v;
                        self.push();
                    } else if kind == "readiness" {
                        self.message(&format!(
                            "{} eligible · {} beat-grid ready · verifying {} tracks · missing: {}",
                            v["eligible"], v["analysed"], v["checking"], v["missing"]
                        ));
                    } else {
                        if kind == "report" {
                            self.capture.borrow_mut().take();
                            a.set_selected_transition("".into());
                        }
                        self.message(v["message"].as_str().unwrap_or(match kind.as_str() {
                            "start" => "Set started in the selected assistance mode",
                            "fast_mode" => "Fast mode change requested for the current set",
                            "pause" => "Set paused; program recording paused too",
                            "resume" => "Set resumed",
                            "report" => "Transition report saved",
                            "end" | "stop_now" => "Set ended",
                            "sources" => "Source collections updated; queue retained",
                            _ => "Done",
                        }));
                    }
                }
            }
        }
        if t - self.last.get() >= 2.0 {
            self.last.set(t);
            self.reads
                .request_with("list", json!({"local":"storage","action":"list"}), read);
            if live {
                // The set history only when it changed (see `op_dj`).
                let history = self.data.borrow()["live"]["set"]["history_key"].as_str().unwrap_or("").to_string();
                self.reads.request("live", json!({"op":"dj","history_key":history}));
                self.reads.request("levels", json!({"op":"levels"}));
                self.reads.request("style", json!({"op":"mix_style"}));
                self.reads.request("tempo", json!({"op":"tempo_plan"}));
                self.reads.request("coverage", json!({"op":"stem_coverage","collections":self.stem_collections()}));
            }
        }
        let mut changed = false;
        if let Some(reply) = self.reads.take("list") {
            match reply.result {
                // Polled every 2 s: rebuild the lists only when they changed.
                // The live state, style and coverage come from their own reads.
                Ok(mut v) if v.is_object() => {
                    let mut data = self.data.borrow_mut();
                    let same = v
                        .as_object()
                        .is_some_and(|m| m.iter().all(|(k, x)| data[k.as_str()] == *x));
                    if !same {
                        for key in ["live", "style", "tempo", "coverage"] {
                            if let Some(x) = data.get_mut(key) {
                                v[key] = x.take();
                            }
                        }
                        *data = v;
                        changed = true;
                    }
                }
                Ok(_) => {}
                Err(e) => self.message(&e),
            }
        }
        if let Some(reply) = self.reads.take("levels")
            && let Ok(v) = reply.result
        {
            a.set_levels(v["mode"].as_str().unwrap_or("").into());
        }
        for key in ["style", "tempo", "coverage"] {
            if let Some(reply) = self.reads.take(key)
                && let Ok(v) = reply.result
            {
                changed |= replace_if_changed(&mut self.data.borrow_mut()[key], v);
            }
        }
        if let Some(reply) = self.reads.take("live")
            && let Ok(mut v) = reply.result
        {
            // An unchanged history was left out: keep the one we have.
            // (Copied, so an unchanged state compares equal: no repaint.)
            if let Some(set) = v["set"].as_object_mut()
                && set.remove("history_same").is_some()
            {
                let data = self.data.borrow();
                for k in ["log", "transitions"] {
                    set.insert(k.into(), data["live"]["set"][k].clone());
                }
            }
            changed |= replace_if_changed(&mut self.data.borrow_mut()["live"], v);
        }
        if changed {
            self.push();
        }
    }
    /// The transition style: the preset, each switch with its one-line
    /// explanation, the status line, the stems the set has and how much of
    /// each track plays.
    fn push_style(&self, a: &DjWorkspaceState<'_>, data: &Value) {
        let st = &data["style"];
        let cov = &data["coverage"];
        a.set_mix_style(field(st, "style").into());
        let about = field(
            st["styles"].as_array().into_iter().flatten().find(|s| s["id"] == st["style"]).unwrap_or(&Value::Null),
            "about",
        );
        a.set_style_about(
            if st["custom"] == true { format!("{about} · customised: a switch below differs from the preset") } else { about }.into(),
        );
        let mut status = field(st, "summary");
        if let Some(next) = st["next_technique"].as_str().filter(|n| !n.is_empty()) {
            status += &format!(" · planned now: {next}");
        }
        a.set_style_status(status.into());
        a.set_style_switches(rows(
            st["controls"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| item(field(c, "id"), field(c, "label"), field(c, "about"), st[field(c, "id")] == true))
                .collect(),
        ));
        let tempo = &data["tempo"];
        a.set_tempo_mode(field(tempo, "mode").into());
        a.set_tempo_bpm(format!("{:.1}", tempo["bpm"].as_f64().unwrap_or(124.0)).trim_end_matches(".0").into());
        let now = match (tempo["playing_bpm"].as_f64(), tempo["target_bpm"].as_f64()) {
            (Some(n), Some(t)) => format!(" · playing {n:.1} BPM, gliding to {t:.1}"),
            (Some(n), None) => format!(" · playing {n:.1} BPM"),
            _ => String::new(),
        };
        a.set_tempo_status(
            if tempo.is_null() {
                String::new()
            } else {
                let mut summary = field(tempo, "summary");
                if let Some(c) = summary.get_mut(..1) {
                    c.make_ascii_uppercase();
                }
                summary + &now
            }
            .into(),
        );
        let percent = st["play_percent"].as_i64().unwrap_or(100) as i32;
        a.set_play_percent(percent);
        a.set_stem_prep(field(st, "stem_prep").into());
        let about = field(
            st["stem_preps"].as_array().into_iter().flatten().find(|p| p["id"] == st["stem_prep"]).unwrap_or(&Value::Null),
            "about",
        );
        let speed = st["stem_seconds"].as_f64().map(|s| format!(" (about {s:.0} s a track here)")).unwrap_or_default();
        a.set_stem_prep_note(
            match (st["stem_prep"].as_str(), st["stem_prep_now_label"].as_str()) {
                (Some("auto"), Some(now)) => format!("Auto: {now} on this machine{speed}. Which stems a mix uses is the DJ's call, per transition."),
                (Some(_), _) if !about.is_empty() => format!("{about}. Which stems a mix uses is the DJ's call, per transition."),
                _ => String::new(),
            }
            .into(),
        );
        let preps = cov["set_preps"].as_array();
        a.set_set_prep_stems(match preps {
            Some(p) if !p.is_empty() => if p.iter().all(|p| p["stems"] == true) { "on" } else { "off" },
            _ => "",
        }.into());
        let (ready, tracks) = (cov["ready"].as_u64().unwrap_or(0), cov["tracks"].as_u64().unwrap_or(0));
        a.set_stems_title(if cov.is_null() {
            "Stems: counting…".into()
        } else {
            format!("Stems: {ready} of {tracks} tracks in this set are prepared")
        }.into());
        a.set_stems_hint(
            match (cov.is_null(), ready, tracks) {
                (true, ..) => "",
                (_, 0, 0) => "Pick a collection under Sources to count its stems.",
                (_, 0, _) => "No track has stems yet, so every blend uses the original mixes. Prepare them to let blends lead with drums and bass.",
                (_, r, t) if r < t => "Only prepared tracks can lead with stems; the rest blend on the original mix. Preparing runs in the background.",
                _ => "Every track is ready: stem-led blends can play anywhere in this set.",
            }
            .into(),
        );
        let batch = &cov["batch"];
        let complete = batch["complete"].as_u64().unwrap_or(0);
        let total = batch["total"].as_u64().unwrap_or(0);
        let failed = batch["failed"].as_u64().unwrap_or(0);
        let batch_status = match batch["state"].as_str() {
            Some("queued" | "preparing" | "submitted") => {
                let action = if batch["resumed"] == true {
                    "Preparation resumed"
                } else {
                    "Preparing stems"
                };
                format!("{action} · {complete} of {total} ready")
            }
            Some("failed") => {
                let reason = batch["error"].as_str().and_then(|error| {
                    let line = error.lines().next()?;
                    (!line.starts_with("Separator failed") && line.chars().count() <= 120)
                        .then_some(line)
                }).unwrap_or("Press Prepare stems to retry.");
                format!("Stem preparation failed · {complete} of {total} ready · {failed} failed. {reason}")
            }
            Some("cancelled") => format!("Stem preparation cancelled · {complete} of {total} ready"),
            Some("ready") if batch["resumed"] == true => {
                format!("Resumed preparation complete · {complete} of {total} ready")
            }
            _ => String::new(),
        };
        a.set_stems_batch_status(batch_status.into());
        let avg = cov["avg_track_minutes"].as_f64().filter(|m| *m > 0.0);
        a.set_play_status(
            match (percent, avg) {
                (100, Some(avg)) => format!("Each track plays in full: about {avg:.1} min on average."),
                (100, None) => "Each track plays in full.".into(),
                (p, Some(avg)) => format!(
                    "Each track plays about {p}% of its length, transitions included: about {:.1} of {avg:.1} min, cut on phrases (never under {:.0} s).",
                    cov["minutes_per_track"].as_f64().unwrap_or(avg * f64::from(p) / 100.0),
                    patch_core::mix_style::MIN_PLAY_SECONDS
                ),
                (p, None) => format!("Each track plays about {p}% of its length, transitions included, cut on phrases."),
            }
            .into(),
        );
    }
    fn push(&self) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let a = w.global::<DjWorkspaceState>();
        let data = self.data.borrow();
        let draft = self.draft.borrow();
        let cs: Vec<_> = data["collections"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| {
                item(
                    c["id"].to_string(),
                    field(c, "name"),
                    format!("{} tracks", c["count"]),
                    draft.collections.contains(&c["id"].as_i64().unwrap_or(0)),
                )
            })
            .collect::<Vec<_>>();
        a.set_selected_collections(cs.iter().filter(|c| c.selected).count() as i32);
        a.set_collections(rows(cs));
        let active = data["live"]["set"]["collections"].as_array();
        let live_on = data["live"]["set"]["on"] == true;
        let active_ids: BTreeSet<i64> = active
            .into_iter()
            .flatten()
            .filter_map(Value::as_i64)
            .collect();
        let draft_ids: BTreeSet<i64> = draft.collections.iter().copied().collect();
        let collections = data["collections"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let selected_summary = collection_summary(collections, &draft_ids);
        let current_summary = collection_summary(collections, &active_ids);
        let same_sources = active_ids == draft_ids;
        a.set_source_change_needed(live_on && !same_sources);
        let source_change_label = match draft_ids.len() {
            0 => "Change Auto Mix to all library tracks",
            1 => "Change Auto Mix to this collection",
            _ => "Change Auto Mix to these collections",
        };
        a.set_source_change_label(source_change_label.into());
        let collection_display = if live_on {
            if same_sources {
                format!("Playing from {current_summary}")
            } else {
                format!("Playing from {current_summary} · selected for next picks: {selected_summary}")
            }
        } else {
            format!("Next set: {selected_summary}")
        };
        a.set_collection_display(collection_display.into());
        self.push_style(&a, &data);
        let test_label = data["live"]["set"]["test_bars"].as_u64()
            .map(|_| " · FAST MODE".to_owned()).unwrap_or_default();
        // A set that is on while no deck plays (after an engine restart, or
        // paused while silent) shows Resume, which plays its current deck:
        // it read "Pause" and stayed silent (the user, 2026-11-16).
        let set_on = data["live"]["set"]["on"] == true;
        let paused = data["live"]["set"]["paused"] == true;
        let silent = set_on
            && !paused
            && data["live"]["decks"].as_array().is_some_and(|d| {
                d.iter().take(2).all(|d| d["playing"] != true)
            });
        let state = if !set_on {
            ""
        } else if paused {
            " · paused"
        } else if silent {
            " · silent: press Resume"
        } else {
            " · playing"
        };
        a.set_session_name(
            (data["live"]["set"]["name"]
                .as_str()
                .filter(|v| !v.is_empty() && set_on)
                .unwrap_or(if set_on { "Auto Mix set" } else { "No set running" })
                .to_owned()
                + state
                + &test_label)
                .into(),
        );
        a.set_paused(paused || silent);
        let requested = data["live"]["set"]["fast_mode_requested"].as_bool();
        a.set_fast_mode_pending(requested.is_some());
        a.set_fast_mode(data["live"]["set"]["on"] == true && requested.unwrap_or(data["live"]["set"]["test_bars"].as_u64().is_some()));
        a.set_live_queue(rows(
            data["live"]["queue"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p.as_str().map(|p| item(p, title(p), "Queued", false)))
                .collect(),
        ));
        a.set_draft_tracks(rows(
            draft
                .tracks
                .iter()
                .map(|t| {
                    item(
                        t.path.clone(),
                        title(&t.path),
                        if std::path::Path::new(&t.path).is_file() {
                            "Ready"
                        } else {
                            "Missing file"
                        },
                        t.pinned,
                    )
                })
                .collect(),
        ));
        a.set_saved_sets(rows(
            data["sets"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|s| a.get_show_trash() || s["data"]["deleted"] != true)
                .map(|s| {
                    item(
                        field(s, "id"),
                        field(s, "name"),
                        format!(
                            "revision {} · {} tracks",
                            s["data"]["revision"],
                            s["data"]["tracks"].as_array().map_or(0, Vec::len)
                        ),
                        s["data"]["deleted"] == true,
                    )
                })
                .collect(),
        ));
        a.set_sessions(rows(
            data["sessions"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|s| {
                    item(
                        field(s, "file_id"),
                        s["name"].as_str().unwrap_or("Archived DJ set"),
                        format!(
                            "{} · {} tracks · {}",
                            s["started"],
                            s["tracks"],
                            if s["ended"].is_null() {
                                "partial / active"
                            } else {
                                "completed"
                            }
                        ),
                        false,
                    )
                })
                .collect(),
        ));
        let selected = self.selected.borrow();
        let filter = a.get_filter().to_lowercase();
        a.set_reports(rows(
            data["reports"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|r| filter.is_empty() || r.to_string().to_lowercase().contains(&filter))
                .map(|r| {
                    item(
                        field(r, "file_id"),
                        format!(
                            "{} → {}",
                            title(r["transition"]["from"].as_str().unwrap_or("")),
                            title(r["transition"]["to"].as_str().unwrap_or(""))
                        ),
                        format!("{} · {}", field(r, "why"), r["reported"]),
                        selected.contains(r["file_id"].as_str().unwrap_or("")),
                    )
                })
                .collect(),
        ));
        a.set_jobs(rows(
            data["jobs"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|j| {
                    item(
                        field(j, "id"),
                        field(j, "status"),
                        format!("{}% · {}", j["progress"], field(j, "message")),
                        j["status"] == "running",
                    )
                })
                .collect(),
        ));
        a.set_transitions(rows(
            data["live"]["set"]["transitions"]
                .as_array()
                .into_iter()
                .flatten()
                .rev()
                .map(|t| {
                    item(
                        field(t, "id"),
                        format!(
                            "{} → {} · {}",
                            title(t["from"].as_str().unwrap_or("")),
                            title(t["to"].as_str().unwrap_or("")),
                            field(t, "technique")
                        ),
                        "Executed",
                        false,
                    )
                })
                .collect(),
        ));
    }
}
