//! Control-plane storage and bounded offline jobs for the unified DJ workspace.
//! No function in this module is called by the audio callback.
use patch_core::dj_workspace::PreparedSet;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};

/// Isolate a worker panic when the crate unwinds. Release builds abort, so
/// the body runs directly and a panic takes the process down (the same
/// policy as the audio callback). `cargo test --release` uses the bench
/// profile, which still unwinds, and this keeps catching there.
fn catch_worker_panic<R>(
    body: impl FnOnce() -> R + std::panic::UnwindSafe,
) -> std::thread::Result<R> {
    #[cfg(panic = "unwind")]
    {
        std::panic::catch_unwind(body)
    }
    #[cfg(not(panic = "unwind"))]
    {
        Ok(body())
    }
}

pub fn unique_id() -> String {
    format!("{}-{}", crate::session::now_s(), crate::now_ns())
}
pub fn atomic_json(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension(format!("{}.tmp", unique_id()));
    let result = (|| {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::File::open(dir)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result
}
fn read_json(path: &Path) -> Result<Value, String> {
    serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
fn safe_id(value: &Value) -> Result<&str, String> {
    let id = value.as_str().ok_or("Select an item first")?;
    if id.is_empty()
        || id.len() > 160
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("Invalid item identity".into());
    }
    Ok(id)
}
pub fn records(kind: &str) -> Vec<Value> {
    records_in(&crate::session::state_dir().join(kind), |v| v)
}

/// The newest 500 records in `dir`, each passed through `shape` and tagged
/// with its `file_id`. The shaped value is kept until the file changes (size
/// or modification time), so a list that is polled every few seconds reads
/// and parses only new or rewritten files: a long DJ set's record holds every
/// transition and log line, megabytes the list never shows.
fn records_in(dir: &Path, shape: fn(Value) -> Value) -> Vec<Value> {
    // Keyed by file and shape: the same folder may be listed whole and summarised.
    type Cache = std::collections::HashMap<(PathBuf, usize), (std::time::SystemTime, u64, Value)>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
    entries.truncate(500);
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
    let kind = shape as usize;
    cache.retain(|(p, k), _| {
        *k != kind || p.parent() != Some(dir) || entries.iter().any(|e| e.path() == *p)
    });
    entries
        .into_iter()
        .filter_map(|e| {
            let path = e.path();
            let key = (path.clone(), kind);
            let stamp = e.metadata().ok().map(|m| {
                (m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len())
            });
            if let (Some((at, len)), Some((cached_at, cached_len, v))) = (stamp, cache.get(&key))
                && at == *cached_at
                && len == *cached_len
            {
                return Some(v.clone());
            }
            let v = match read_json(&path) {
                Ok(v) => {
                    let mut v = shape(v);
                    v["file_id"] = json!(path.file_stem()?.to_string_lossy());
                    v
                }
                Err(e) => return Some(json!({"status":"unreadable", "error":e})),
            };
            if let Some((at, len)) = stamp {
                cache.insert(key, (at, len, v.clone()));
            }
            Some(v)
        })
        .collect()
}

/// A set record as the list shows it: its scalar fields, without the
/// transitions, log and takes (`session_detail` reads the whole file).
fn set_summary(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.into_iter()
                .filter(|(_, x)| !x.is_array() && !x.is_object())
                .collect(),
        ),
        other => other,
    }
}

/// A transition report as the list shows and searches it: why, notes, when,
/// and the transition's scalar fields (its from and to tracks); decks, grids
/// and history stay in the file for `report_detail` and processing.
fn report_summary(mut v: Value) -> Value {
    let transition = v.get_mut("transition").map(Value::take);
    let mut out = set_summary(v);
    if let Some(t @ Value::Object(_)) = transition {
        out["transition"] = set_summary(t);
    }
    out
}
fn active_tuning() -> Value {
    patch_ai::mix::tuning_path()
        .and_then(|p| read_json(&p).ok())
        .and_then(|v| v.get("workspace_version").cloned())
        .unwrap_or_else(|| {
            read_json(&crate::session::state_dir().join("active-tuning.json"))
                .unwrap_or(Value::Null)
        })
}
fn write_tuning(tuning: Value, active: Value) -> Result<(), String> {
    let mut tuning = tuning;
    let _: patch_ai::mix::Tuning =
        serde_json::from_value(tuning.clone()).map_err(|e| e.to_string())?;
    tuning["workspace_version"] = active;
    atomic_json(
        &patch_ai::mix::tuning_path().ok_or("No configuration directory")?,
        &tuning,
    )
}
static EDITS: Mutex<()> = Mutex::new(());
fn save_draft(db: &patch_library::Library, mut draft: PreparedSet) -> Result<PreparedSet, String> {
    draft.validate()?;
    let tracks = db.tracks()?;
    for track in &mut draft.tracks {
        if let Some(t) = tracks.iter().find(|t| {
            track.track_uuid.as_deref() == Some(t.track_uuid.as_str())
                || track.id == Some(t.id)
                || t.path == track.path
        }) {
            track.id = Some(t.id);
            track.track_uuid = Some(t.track_uuid.clone());
            track.path = t.path.clone();
        }
    }
    if draft.id.is_empty() {
        draft.id = unique_id();
    }
    safe_id(&json!(draft.id))?;
    if let Some((_, old)) = db.load_set(&draft.id)? {
        if old["revision"].as_u64().unwrap_or(0) != draft.revision {
            return Err(
                "This saved set changed in another window. Reload it before saving.".into(),
            );
        }
    } else if draft.revision != 0 {
        return Err("Saved set no longer exists".into());
    }
    draft.revision += 1;
    db.save_set(&draft.id, &draft.name, &json!(draft))?;
    Ok(draft)
}
fn readiness(draft: &PreparedSet, db: &patch_library::Library) -> Value {
    let tracks = db.tracks().unwrap_or_default();
    let members: std::collections::HashSet<_> = draft
        .collections
        .iter()
        .flat_map(|id| db.collection_tracks(*id))
        .collect();
    let pool: Vec<_> = tracks
        .iter()
        .filter(|t| draft.collections.is_empty() || members.contains(&t.id))
        .collect();
    // An estimated BPM alone is not enough to promise AI transitions. Count
    // a track as ready only when its calibrated quick estimate or measured
    // kick grid supports the deterministic planner. Matching source tempo
    // tags can seed a background grid check for uncertain tracks.
    let mut checking = 0usize;
    let analysed = pool
        .iter()
        .filter(|t| {
            if t.missing {
                return false;
            }
            let Some(mut a) = patch_analysis::cached(Path::new(&t.path)) else {
                return false;
            };
            let ready = a.bpm_confidence >= 0.5 || a.use_grid();
            if !ready
                && let Some((detected, tagged)) = a.bpm.zip(a.tag_bpm)
                && tagged.is_finite()
                && (40.0..=300.0).contains(&tagged)
                && (detected - tagged).abs() / tagged <= 0.02
            {
                let _ = patch_analysis::grid_soon(Path::new(&t.path), false);
                checking += 1;
            }
            ready
        })
        .count();
    let missing: Vec<_> = draft
        .tracks
        .iter()
        .filter(|t| !Path::new(&t.path).is_file())
        .map(|t| &t.path)
        .collect();
    json!({"eligible":pool.iter().filter(|t| !t.missing).count(), "analysed":analysed, "checking":checking, "missing":missing})
}

/// Storage requests shared by CLI and UI, also usable without a live engine.
pub fn storage(req: &Value) -> Result<Value, String> {
    match req["action"].as_str().unwrap_or("list") {
        "list" => {
            let db = patch_library::open_default()?;
            let sets: Vec<_> = db
                .saved_sets()?
                .into_iter()
                .map(|(id, name, data)| json!({"id":id,"name":name,"data":data}))
                .collect();
            let collections: Vec<_> = db
                .collections()
                .into_iter()
                .map(|(id, name, count)| json!({"id":id,"name":name,"count":count}))
                .collect();
            Ok(
                json!({"sets":sets,"collections":collections,"sessions":records_in(&crate::session::state_dir().join("sets"), set_summary),"reports":records_in(&crate::session::state_dir().join("reports"), report_summary),"jobs":job_records(),"active_tuning":active_tuning()}),
            )
        }
        "save" => {
            let _guard = EDITS.lock().unwrap();
            let draft: PreparedSet =
                serde_json::from_value(req["set"].clone()).map_err(|e| e.to_string())?;
            Ok(json!(save_draft(&patch_library::open_default()?, draft)?))
        }
        "load" | "duplicate" | "trash" | "restore" => {
            let _guard = EDITS.lock().unwrap();
            let id = safe_id(&req["id"])?;
            let db = patch_library::open_default()?;
            let (name, value) = db.load_set(id)?.ok_or("Saved set not found")?;
            let mut draft: PreparedSet = serde_json::from_value(value)
                .map_err(|e| format!("Saved set format needs migration: {e}"))?;
            if draft.id.is_empty() {
                draft.id = id.into();
                draft.name = name;
            }
            let tracks = db.tracks()?;
            for track in &mut draft.tracks {
                if let Some(t) = tracks.iter().find(|t| {
                    track.track_uuid.as_deref() == Some(t.track_uuid.as_str())
                        || track.id == Some(t.id)
                }) {
                    track.path = t.path.clone();
                }
            }
            match req["action"].as_str().unwrap_or("") {
                "duplicate" => {
                    draft.id.clear();
                    draft.revision = 0;
                    draft.name += " (copy)";
                    draft.deleted = false;
                    draft = save_draft(&db, draft)?;
                }
                "trash" | "restore" => {
                    draft.deleted = req["action"] == "trash";
                    draft = save_draft(&db, draft)?;
                }
                _ => {}
            }
            Ok(json!({"set":draft,"readiness":readiness(&draft,&db)}))
        }
        "readiness" => {
            let draft: PreparedSet =
                serde_json::from_value(req["set"].clone()).map_err(|e| e.to_string())?;
            draft.validate()?;
            Ok(readiness(&draft, &patch_library::open_default()?))
        }
        "report_detail" | "session_detail" | "job_detail" => {
            let kind = match req["action"].as_str() {
                Some("report_detail") => "reports",
                Some("job_detail") => "dj-jobs",
                _ => "sets",
            };
            read_json(
                &crate::session::state_dir()
                    .join(kind)
                    .join(format!("{}.json", safe_id(&req["id"])?)),
            )
        }
        _ => Err("Unknown DJ workspace request".into()),
    }
}
struct Running {
    id: String,
    cancel: Arc<AtomicBool>,
}
fn running() -> &'static Mutex<Option<Running>> {
    static RUNNING: OnceLock<Mutex<Option<Running>>> = OnceLock::new();
    RUNNING.get_or_init(Mutex::default)
}
fn job_path(id: &str) -> PathBuf {
    crate::session::state_dir()
        .join("dj-jobs")
        .join(format!("{id}.json"))
}
fn job_records() -> Vec<Value> {
    let running_id = running().lock().unwrap().as_ref().map(|r| r.id.clone());
    records("dj-jobs")
        .into_iter()
        .map(|mut v| {
            if v["status"] == "running" && v["id"].as_str() != running_id.as_deref() {
                v["status"] = json!("interrupted");
                v["message"] = json!("Engine restarted; retry this job.");
            }
            v
        })
        .collect()
}
pub fn job_running() -> bool {
    running().lock().unwrap().is_some()
}
pub fn cancel_job(id: &str) -> Result<Value, String> {
    let guard = running().lock().unwrap();
    let job = guard
        .as_ref()
        .filter(|r| r.id == id)
        .ok_or("This job is no longer running")?;
    job.cancel.store(true, Ordering::Relaxed);
    Ok(json!({"message":"Cancellation requested; current pair will finish first."}))
}
/// One bounded job at a time. It never modifies active tuning.
pub fn start_job(ids: Vec<String>, improve: bool) -> Result<Value, String> {
    if ids.is_empty() || ids.len() > 24 {
        return Err("Select 1–24 reports per processing job".into());
    }
    for id in &ids {
        safe_id(&json!(id))?;
    }
    let mut guard = running().lock().unwrap();
    if guard.is_some() {
        return Err("A report job is already running".into());
    }
    let id = unique_id();
    let cancel = Arc::new(AtomicBool::new(false));
    let job = json!({"version":1,"id":id,"status":"running","started":crate::session::now_s(),"reports":ids,"improve":improve,"progress":0,"message":"Validating reports","engine_version":patch_core::VERSION,"judge_version":1});
    atomic_json(&job_path(&id), &job)?;
    *guard = Some(Running {
        id: id.clone(),
        cancel: cancel.clone(),
    });
    let job_id = id.clone();
    if let Err(e) = std::thread::Builder::new()
        .name("patch-report-job".into())
        .spawn(move || {
            let mut job = job;
            let result =
                catch_worker_panic(std::panic::AssertUnwindSafe(|| process(&mut job, &cancel)));
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    job["status"] = json!(if cancel.load(Ordering::Relaxed) {
                        "cancelled"
                    } else {
                        "failed"
                    });
                    job["message"] = json!(e);
                }
                Err(_) => {
                    job["status"] = json!("failed");
                    job["message"] = json!("Report worker failed; active tuning unchanged.");
                }
            }
            patch_ai::mix::set_tuning(None);
            job["finished"] = json!(crate::session::now_s());
            let _ = atomic_json(&job_path(&job_id), &job);
            *running().lock().unwrap() = None;
        })
    {
        *guard = None;
        return Err(e.to_string());
    }
    Ok(json!({"id":id,"message":"Report processing started"}))
}
#[path = "workspace_learn.rs"]
mod learn;
use learn::process;

pub fn activate(id: &str) -> Result<Value, String> {
    let _guard = EDITS.lock().unwrap();
    safe_id(&json!(id))?;
    let job = read_json(&job_path(id))?;
    if job["status"] != "evaluation_passed" {
        return Err("Only a candidate that passed evaluation can be activated".into());
    }
    let tuning: patch_ai::mix::Tuning =
        serde_json::from_value(job["candidate"].clone()).map_err(|e| e.to_string())?;
    if json!(patch_ai::mix::tuning()) != job["baseline"]["tuning"] {
        return Err("Active tuning changed since evaluation. Reprocess the reports first.".into());
    }
    atomic_json(
        &crate::session::state_dir().join("previous-tuning.json"),
        &json!({"tuning":patch_ai::mix::tuning(),"active":active_tuning()}),
    )?;
    write_tuning(
        json!(tuning),
        json!({"id":id,"reports":job["reports"],"activated":crate::session::now_s()}),
    )?;
    Ok(json!({"message":"Tuning activated for newly planned transitions"}))
}
pub fn rollback() -> Result<Value, String> {
    let _guard = EDITS.lock().unwrap();
    let path = crate::session::state_dir().join("previous-tuning.json");
    let previous = read_json(&path)?;
    let active = active_tuning();
    let tuning = patch_ai::mix::tuning();
    write_tuning(previous["tuning"].clone(), previous["active"].clone())?;
    atomic_json(&path, &json!({"tuning":tuning,"active":active}))?;
    Ok(json!({"message":"Previous tuning restored"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_tests_still_catch_panics() {
        // Cargo keeps panic=unwind for the test and bench profiles even when
        // profile.release aborts, so the harness and this worker can catch.
        assert!(std::panic::catch_unwind(|| panic!("probe")).is_err());
        let isolated =
            catch_worker_panic(std::panic::AssertUnwindSafe(|| -> Result<(), String> {
                panic!("worker");
            }));
        assert!(isolated.is_err());
    }
    #[test]
    fn saved_drafts_reject_stale_edits_and_keep_trash_recoverable() {
        let path = std::env::temp_dir().join(format!("patch-drafts-{}.sqlite", unique_id()));
        let db = patch_library::Library::open(&path).unwrap();
        let saved = save_draft(&db, PreparedSet::default()).unwrap();
        assert_eq!(saved.revision, 1);
        let mut deleted = saved.clone();
        deleted.deleted = true;
        let deleted = save_draft(&db, deleted).unwrap();
        assert!(save_draft(&db, saved).is_err());
        let mut restore = deleted;
        restore.deleted = false;
        assert!(!save_draft(&db, restore).unwrap().deleted);
        drop(db);
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn listed_records_are_summaries_reread_only_when_the_file_changes() {
        let dir = std::env::temp_dir().join(format!("patch-records-{}", unique_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("1.json");
        let set = json!({"name":"Friday","tracks":2,"log":["a"],"transitions":[{"x":1}]});
        std::fs::write(&file, set.to_string()).unwrap();
        // A whole read of the same folder does not stand in for the summary.
        assert_eq!(records_in(&dir, |v| v)[0]["log"], json!(["a"]));
        let listed = records_in(&dir, set_summary);
        assert_eq!(listed, vec![json!({"name":"Friday","tracks":2,"file_id":"1"})]);
        // Same size and time: the cached summary, not the file.
        let time = std::fs::metadata(&file).unwrap().modified().unwrap();
        std::fs::write(&file, set.to_string().replace("Friday", "Sunday")).unwrap();
        std::fs::File::options().write(true).open(&file).unwrap().set_modified(time).unwrap();
        assert_eq!(records_in(&dir, set_summary)[0]["name"], "Friday");
        std::fs::write(&file, set.to_string().replace("Friday", "Saturday")).unwrap();
        assert_eq!(records_in(&dir, set_summary)[0]["name"], "Saturday");
        let report = json!({"why":"bad timing","decks":[1],"transition":{"from":"a.flac","to":"b.flac","plan":{}}});
        assert_eq!(
            report_summary(report),
            json!({"why":"bad timing","transition":{"from":"a.flac","to":"b.flac"}})
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn identities_cannot_escape_storage_directory() {
        assert!(safe_id(&json!("../report")).is_err());
        assert!(safe_id(&json!("1791032256")).is_ok());
    }
}
