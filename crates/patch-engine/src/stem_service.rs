//! Stem preparation/attachment service. All I/O and decoding are background work.
use crate::Params;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, LazyLock, Mutex, OnceLock, atomic::Ordering},
    thread::{self, Thread},
    time::{Duration, Instant},
};

struct StatusCatalogue {
    refreshed: Instant,
    sets: Vec<Value>,
    jobs: Vec<Value>,
    batches: Vec<Value>,
}

static STATUS_CATALOGUE: LazyLock<Mutex<Option<StatusCatalogue>>> =
    LazyLock::new(|| Mutex::new(None));

/// The compact set and job records by file, re-read only when a file
/// changes: the metadata holds hundreds of megabyte-sized records, and
/// parsing them all every 3 s cost the engine 10 % of a core at idle.
static SET_MEMO: LazyLock<Mutex<patch_stems::cache::Memo<Value>>> =
    LazyLock::new(Default::default);
static BATCH_MEMO: LazyLock<Mutex<patch_stems::cache::Memo<Value>>> =
    LazyLock::new(Default::default);
static JOB_MEMO: LazyLock<Mutex<patch_stems::cache::Memo<Value>>> =
    LazyLock::new(Default::default);

/// The library's track id for a deck's path, kept a while: every status
/// poll opened the library for it.
/// One entry per path: both decks poll in turn, and a single entry made
/// every other poll open the library again.
fn track_id_for(source: &str) -> String {
    type Known = Vec<(String, Instant, String)>;
    static KNOWN: Mutex<Known> = Mutex::new(Vec::new());
    let mut known = KNOWN.lock().unwrap();
    known.retain(|(_, at, _)| at.elapsed() < Duration::from_secs(10));
    if let Some((_, _, id)) = known.iter().find(|(path, ..)| path == source) {
        return id.clone();
    }
    let id = patch_library::open_quick()
        .ok()
        .and_then(|db| db.track_uuid_for_path(source).ok().flatten())
        .unwrap_or_else(|| source.to_string());
    if known.len() >= 8 {
        known.remove(0);
    }
    known.push((source.to_string(), Instant::now(), id.clone()));
    id
}

fn compact_set(record: &Value) -> Value {
    let set = &record["set"];
    let stems = set["stems"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(id, stem)| {
            (
                id.clone(),
                json!({"start_frame":stem["start_frame"],"frames":stem["frames"]}),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    json!({
        "resident":record["resident"],
        "aliases":record["aliases"],
        "pins":record["pins"],
        "set":{"id":set["id"],"track_id":set["track_id"],"frames":set["frames"],"stems":stems}
    })
}

fn compact_job(job: &Value) -> Value {
    json!({
        "id":job["id"], "source":job["source"], "state":job["state"],
        "progress":job["progress"], "error":job["error"], "started":job["started"],
        "resumed":job["resumed"]
    })
}

/// Run a small status read under the catalogue lock. Pollers share the
/// already compacted records and only copy data they will return.
fn with_status_catalogue<R>(
    cache: &patch_stems::cache::Cache,
    inspect: impl FnOnce(&StatusCatalogue) -> R,
) -> R {
    let mut cached = STATUS_CATALOGUE.lock().unwrap();
    if cached
        .as_ref()
        .is_none_or(|c| c.refreshed.elapsed() >= Duration::from_secs(3))
    {
        let sets = SET_MEMO.lock().unwrap().list(
            &cache.metadata,
            patch_stems::cache::is_set_file,
            |v| v["set"].is_object().then(|| compact_set(v)),
        );
        let jobs =
            patch_stems::jobs::list_memo(cache, &mut JOB_MEMO.lock().unwrap(), compact_job);
        let batches = patch_stems::batches::summaries(cache, &mut BATCH_MEMO.lock().unwrap(), &jobs);
        *cached = Some(StatusCatalogue {
            refreshed: Instant::now(),
            sets,
            jobs,
            batches,
        });
    }
    inspect(cached.as_ref().unwrap())
}

fn status_catalogue(
    cache: &patch_stems::cache::Cache,
    track: &str,
    source: &str,
) -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    with_status_catalogue(cache, |c| {
        let sets = c
            .sets
            .iter()
            .filter(|set| {
                set["set"]["track_id"] == track
                    || set["aliases"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|v| v == track))
            })
            .cloned()
            .collect();
        let jobs = c.jobs.iter().filter(|job| job["source"] == source).cloned().collect();
        (sets, jobs, c.batches.iter().take(8).cloned().collect())
    })
}

/// The AI's reference on a set until its transition has played; never
/// released by the stingy pin rule.
pub const STEM_HOLD: &str = "ai-transition";

fn invalidate_status_catalogue() {
    *STATUS_CATALOGUE.lock().unwrap() = None;
}
static PREPARATION_WORKER: OnceLock<Thread> = OnceLock::new();

fn wake_preparation() {
    if let Some(worker) = PREPARATION_WORKER.get() {
        worker.unpark();
    }
}

/// Start recovery off the audio and IPC threads. An older engine may have
/// active jobs lacking the new ownership locks, so wait for it to exit.
pub(crate) fn spawn_preparation(params: Arc<Params>) -> std::io::Result<()> {
    let worker = thread::Builder::new()
        .name("patch-stem-recovery".into())
        .spawn(move || {
            // SAFETY: only the priority of this background thread changes.
            unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 10) };
            if std::env::var_os("PATCH_ENGINE_SILENT").is_some() {
                let retiring = crate::daemon::retiring_socket_path();
                while std::os::unix::net::UnixStream::connect(&retiring).is_ok() {
                    thread::park_timeout(Duration::from_millis(250));
                }
            }
            let cache = match patch_stems::cache::Cache::default_cache() {
                Ok(cache) => cache,
                Err(error) => {
                    eprintln!("patch stems: could not open preparation state: {error}");
                    return;
                }
            };
            let configured = || {
                let (separator, backend) = patch_stems::separator::configured()?;
                Ok((Box::new(separator) as Box<dyn patch_stems::separator::StemSeparator>, backend))
            };
            let mut legacy = true;
            let mut recover = true;
            let mut job_records = patch_stems::cache::Memo::default();
            let mut batches = patch_stems::batches::Runner::default();
            loop {
                let result = (|| {
                    if legacy {
                        patch_stems::batches::recover_legacy(&cache)?;
                        legacy = false;
                    }
                    if recover {
                        recover = patch_stems::jobs::recover(&cache, &mut job_records, &configured, |id, source| {
                            if let Err(error) = follow_prepared(params.clone(), &cache, id, source.to_string_lossy().into_owned()) {
                                eprintln!("patch stems: could not watch recovered job {id}: {error}");
                            }
                        })?;
                    }
                    let batches_pending = batches.tick(&cache, &configured)?;
                    Ok::<_, String>(batches_pending || recover)
                })();
                match result {
                    Ok(true) => thread::park_timeout(Duration::from_millis(250)),
                    Ok(false) => thread::park(),
                    Err(error) => {
                        eprintln!("patch stems: preparation recovery: {error}");
                        thread::park_timeout(Duration::from_secs(5));
                    }
                }
            }
        })?;
    let _ = PREPARATION_WORKER.set(worker.thread().clone());
    Ok(())
}

fn follow_prepared(
    params: Arc<Params>,
    cache: &patch_stems::cache::Cache,
    id: &str,
    source: String,
) -> Result<(), String> {
    let path = cache.metadata.join(format!("job-{id}.json"));
    thread::Builder::new()
        .name("patch-stem-readiness".into())
        .spawn(move || {
            for _ in 0..7200 {
                let record = std::fs::read(&path).ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
                if let Some(record) = record {
                    match record["state"].as_str() {
                        Some("ready") => {
                            if let Some(key) = record["set"]["id"].as_str() {
                                for sh in params.decks.iter().take(2) {
                                    if sh.status.lock().unwrap().path.as_ref() != source
                                        || sh.stems.ready.load(Ordering::Acquire)
                                    {
                                        continue;
                                    }
                                    match decode(&source, key, sh.rate.load(Ordering::Acquire)) {
                                        Ok(audio) if sh.status.lock().unwrap().path.as_ref() == source => {
                                            sh.send(crate::deck::transport::Cmd::AttachStems(audio.into()))
                                        }
                                        Ok(_) => {}
                                        Err(error) => *sh.stem_error.lock().unwrap() = Some(Arc::from(error)),
                                    }
                                }
                            }
                            break;
                        }
                        Some("failed" | "cancelled" | "interrupted") => break,
                        _ => {}
                    }
                }
                thread::sleep(Duration::from_secs(1));
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn handle(params: &Arc<Params>, req: &Value) -> Result<Value, String> {
    let cache = patch_stems::cache::Cache::default_cache()?;
    let deck = match req["deck"].as_str() {
        Some("a") => 0,
        Some("b") => 1,
        _ => return Err("Choose deck a or b".into()),
    };
    let sh = &params.decks[deck];
    match req["action"].as_str().unwrap_or("status") {
        "prepare_collections" => {
            let ids: Vec<i64> =
                serde_json::from_value(req["collections"].clone()).map_err(|e| e.to_string())?;
            if ids.is_empty() {
                return Err("Select collections to prepare explicitly".into());
            }
            let db = patch_library::open_default().map_err(|e| e.to_string())?;
            let selected: std::collections::HashSet<i64> = ids
                .iter()
                .flat_map(|id| db.collection_tracks(*id))
                .collect();
            let pin = req["pin"].as_bool().unwrap_or(true);
            let tracks = db
                .tracks()
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter(|t| selected.contains(&t.id))
                .filter(|t| {
                    pin || !matches!(
                        context(&t.path).availability,
                        patch_core::stems::Availability::Ready
                            | patch_core::stems::Availability::Queued
                            | patch_core::stems::Availability::Preparing
                    )
                })
                .map(|t| patch_stems::batches::Track { source: t.path.into(), track: t.track_uuid })
                .collect();
            patch_stems::separator::configured()?;
            let (id, joined) = patch_stems::batches::submit(&cache, &ids, tracks, pin, &[STEM_HOLD])?;
            invalidate_status_catalogue();
            wake_preparation();
            let message = if joined {
                "Stem preparation is already running for these collections"
            } else {
                "Stem preparation queued for these collections"
            };
            Ok(json!({"batch":id,"message":message}))
        }
        "cancel_batch" => {
            patch_stems::batches::cancel(&cache, req["id"].as_str().ok_or("Choose a preparation batch")?)?;
            invalidate_status_catalogue();
            wake_preparation();
            Ok(json!("Selected preparation cancelled"))
        }
        "setup" => {
            let path = cache.metadata.join("setup.json");
            if std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .is_some_and(|v| v["state"] == "preparing")
            {
                return Ok(json!("Separator installation is already running"));
            }
            patch_stems::atomic_json(&path, &json!({"state":"preparing","progress":0}))?;
            let installed_prefix_script = std::env::current_exe().ok().and_then(|exe| {
                exe.parent()
                    .and_then(|bin| bin.parent())
                    .map(|prefix| prefix.join("share/omarchy-patch/stems/setup.py"))
            });
            let script = [
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tools/stems/setup.py"),
                installed_prefix_script.unwrap_or_default(),
                std::path::PathBuf::from("/usr/share/omarchy-patch/stems/setup.py"),
                std::env::var_os("XDG_DATA_HOME")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| {
                        std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                            .join(".local/share")
                    })
                    .join("omarchy-patch/stems/setup.py"),
            ]
            .into_iter()
            .find(|p| p.is_file())
            .ok_or("Separator installer is unavailable")?;
            std::thread::Builder::new()
                .name("patch-stem-runtime-setup".into())
                .spawn(move || {
                    let log = std::fs::File::create(path.with_extension("log"));
                    let result = log
                        .map_err(|e| e.to_string())
                        .and_then(|log| {
                            std::process::Command::new("nice")
                                .args(["-n", "10", "python3"])
                                .arg(script)
                                .args(["--device", "auto"])
                                .env("PATCH_STEM_SETUP_STATUS", &path)
                                .stdout(std::process::Stdio::from(
                                    log.try_clone().map_err(|e| e.to_string())?,
                                ))
                                .stderr(std::process::Stdio::from(log))
                                .status()
                                .map_err(|e| e.to_string())
                        })
                        .and_then(|s| {
                            if s.success() {
                                Ok(())
                            } else {
                                Err("Separator installation failed; inspect setup.log".into())
                            }
                        });
                    if let Err(e) = result {
                        let current = std::fs::read(&path)
                            .ok()
                            .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
                        if current.as_ref().is_none_or(|v| v["state"] != "failed") {
                            let _ = patch_stems::atomic_json(
                                &path,
                                &json!({"state":"failed","error":e}),
                            );
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
            Ok(json!("Installing optional separator"))
        }
        "prepare" | "pin_prepare" => {
            let source = req["path"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| sh.status.lock().unwrap().path.to_string());
            let (sep, backend) = patch_stems::separator::configured()?;
            let track = patch_library::open_default()
                .ok()
                .and_then(|db| db.tracks().ok())
                .and_then(|ts| ts.into_iter().find(|t| t.path == source))
                .map_or_else(|| source.clone(), |t| t.track_uuid);
            let result = patch_stems::jobs::request(
                cache.clone(),
                source.clone().into(),
                track,
                Box::new(sep),
                backend,
                req["action"] == "pin_prepare",
            )?;
            invalidate_status_catalogue();
            let id = result["id"]
                .as_str()
                .ok_or("Missing job identity")?
                .to_owned();
            follow_prepared(params.clone(), &cache, &id, source)?;
            Ok(result)
        }
        "cancel" => {
            patch_stems::jobs::cancel(&cache, req["id"].as_str().ok_or("Choose a preparation job")?)?;
            invalidate_status_catalogue();
            Ok(json!("Cancellation requested"))
        }
        "attach" => {
            let id = req["id"]
                .as_str()
                .ok_or("Choose a prepared stem set")?
                .to_owned();
            patch_stems::cache::valid_key(&id)?;
            let source = sh.status.lock().unwrap().path.to_string();
            if source.is_empty() {
                return Err("Load a track first".into());
            }
            let sh = sh.clone();
            std::thread::Builder::new()
                .name("patch-stem-decode".into())
                .spawn(move || {
                    let result: Result<(), String> = (|| {
                        let audio = decode(&source, &id, sh.rate.load(Ordering::Acquire))?;
                        if sh.status.lock().unwrap().path.as_ref() != source {
                            return Err("Deck changed while stems were decoding".into());
                        }
                        sh.send(crate::deck::transport::Cmd::AttachStems(audio.into()));
                        Ok(())
                    })();
                    if let Err(e) = result {
                        *sh.stem_error.lock().unwrap() = Some(Arc::from(e));
                    }
                })
                .map_err(|e| e.to_string())?;
            Ok(json!("Loading prepared stems"))
        }
        "pin" | "unpin" => {
            let id = req["id"].as_str().ok_or("Choose a prepared set")?;
            let source = sh.status.lock().unwrap().path.to_string();
            let track = patch_library::open_default()
                .ok()
                .and_then(|db| db.tracks().ok())
                .and_then(|ts| ts.into_iter().find(|t| t.path == source))
                .map_or(source, |t| t.track_uuid);
            cache.pin(id, &track, req["action"] == "pin")?;
            invalidate_status_catalogue();
            Ok(json!("Retention updated"))
        }
        "cleanup" => cache.cleanup(),
        "reset" => {
            sh.stems.human_reset();
            Ok(json!("Original mix"))
        }
        "auto" => {
            sh.stems.allow_auto();
            Ok(json!("Stem control returned to Auto"))
        }
        "status" => {
            let source = sh.status.lock().unwrap().path.to_string();
            // Status is polled by every visible deck/Booth window. Query only
            // the loaded path here; loading every library row for each poll
            // made stem controls contend with playback on larger libraries.
            let track = track_id_for(&source);
            let (sets, jobs, batches) = status_catalogue(&cache, &track, &source);
            let active_set = {
                let set = sh.stem_set.lock().unwrap().clone();
                set.map(|set| json!({"id": set.id, "track_id": set.track_id}))
            };
            let backend_ready = backend_ready();
            let setup_path = cache.metadata.join("setup.json");
            let mut setup = std::fs::read(&setup_path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .unwrap_or(Value::Null);
            // Recover installations completed by earlier versions, whose
            // installer wrote progress under XDG_CACHE_HOME while the UI
            // read this status from XDG_STATE_HOME.
            if backend_ready && setup["state"] != "ready" {
                setup = json!({"state":"ready","progress":100});
                let _ = patch_stems::atomic_json(&setup_path, &setup);
            }
            let stem_error = sh.stem_error.lock().unwrap().clone();
            Ok(
                json!({"backend_ready":backend_ready,"setup":setup,"source":source,"track_id":track,"sets":sets,"jobs":jobs,"batches":batches,"controls":sh.stems.snapshot(),"error":stem_error.as_deref(),"set":active_set}),
            )
        }
        _ => Err("Unknown stem preparation action".into()),
    }
}

/// Control-plane catalogue snapshot. Never called by an audio callback.
/// What [`context`]'s catalogue is made from, as a fingerprint: each set
/// record's name, size and mtime, and the library database's (WAL included).
fn context_stamp() -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut stamp = |path: &std::path::Path| {
        if let Ok(m) = std::fs::metadata(path) {
            path.hash(&mut h);
            m.len().hash(&mut h);
            m.modified().ok().hash(&mut h);
        }
    };
    if let Ok(cache) = patch_stems::cache::Cache::default_cache() {
        let mut sets: Vec<_> = std::fs::read_dir(&cache.metadata)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| patch_stems::cache::is_set_file(p))
            .collect();
        sets.sort();
        for path in &sets {
            stamp(path);
        }
    }
    let db = patch_library::default_path();
    stamp(&db);
    let mut wal = db.into_os_string();
    wal.push("-wal");
    stamp(std::path::Path::new(&wal));
    h.finish()
}

/// How many tracks of `collections` (none: the whole library) have all their
/// stems ready to play, the tracks' average length and the minutes of each
/// that play at `percent` (see `patch_core::mix_style`). Remembered for a
/// few seconds: the DJ Booth polls it.
pub fn coverage(collections: &[i64], percent: u8) -> Result<Value, String> {
    type Memo = Option<(Instant, Vec<i64>, Value)>;
    static MEMO: Mutex<Memo> = Mutex::new(None);
    let mut memo = MEMO.lock().unwrap();
    let base = match memo
        .as_ref()
        // The Booth polls this every two seconds; each miss opens the
        // library and walks every track (~3 ms). Coverage moves slowly.
        .filter(|(at, ids, _)| ids == collections && at.elapsed() < Duration::from_secs(15))
    {
        Some((_, _, v)) => v.clone(),
        None => {
            let db = patch_library::open_default().map_err(|e| e.to_string())?;
            let members: Option<std::collections::HashSet<i64>> = (!collections.is_empty())
                .then(|| collections.iter().flat_map(|id| db.collection_tracks(*id)).collect());
            let tracks: Vec<_> = db
                .tracks()
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter(|t| !t.missing && members.as_ref().is_none_or(|m| m.contains(&t.id)))
                .collect();
            let ready = tracks
                .iter()
                .filter(|t| context(&t.path).availability == patch_core::stems::Availability::Ready)
                .count();
            let lengths: Vec<f64> =
                tracks.iter().map(|t| t.duration).filter(|d| *d > 0.0).collect();
            let avg = lengths.iter().sum::<f64>() / lengths.len().max(1) as f64;
            let v = json!({"tracks": tracks.len(), "ready": ready, "avg_track_s": avg});
            *memo = Some((Instant::now(), collections.to_vec(), v.clone()));
            v
        }
    };
    let avg = base["avg_track_s"].as_f64().unwrap_or(0.0);
    let mut v = base;
    v["play_percent"] = json!(percent);
    v["minutes_per_track"] = json!(patch_core::mix_style::approx_minutes(avg, percent));
    v["avg_track_minutes"] = json!(avg / 60.0);
    drop(memo);
    if !collections.is_empty()
        && let Ok(cache) = patch_stems::cache::Cache::default_cache()
    {
        v["batch"] = with_status_catalogue(&cache, |catalogue| {
            patch_stems::batches::for_collections(&catalogue.batches, collections)
        });
    }
    Ok(v)
}

pub fn context(source: &str) -> patch_core::stems::StemContext {
    use std::{
        collections::HashMap,
        sync::{Arc, LazyLock, Mutex},
        time::{Duration, Instant},
    };
    type Rows = Arc<HashMap<String, Arc<patch_core::stems::StemContext>>>;
    type Catalogue = (Instant, u64, Rows);
    /// A set record as the catalogue keeps it: resident, track id, aliases
    /// and its context (shared with the rows, not copied).
    type Record = Arc<(bool, String, Vec<String>, Arc<patch_core::stems::StemContext>)>;
    static CATALOGUE: LazyLock<Mutex<Option<Catalogue>>> = LazyLock::new(|| Mutex::new(None));
    // Held while one caller rebuilds; the catalogue lock is not, so lookups
    // (the AI view polls several times a second) answer from the previous
    // catalogue instead of waiting seconds behind a rebuild.
    static REBUILD: LazyLock<Mutex<patch_stems::cache::Memo<Record>>> =
        LazyLock::new(Default::default);
    let lookup = |rows: &Rows| rows.get(source).map(|c| (**c).clone()).unwrap_or_default();
    let current = {
        let mut memo = CATALOGUE.lock().unwrap();
        match memo.as_mut() {
            Some((at, _, rows)) if at.elapsed() <= Duration::from_secs(3) => {
                return lookup(rows);
            }
            // Claim the check, so callers in the next 3 s don't repeat it.
            Some((at, known, rows)) => {
                *at = Instant::now();
                Some((*known, rows.clone()))
            }
            None => None,
        }
    };
    // Only the first build is waited for; afterwards a busy rebuild is
    // left to its owner and this caller reads the previous rows.
    let mut parsed = match (&current, REBUILD.try_lock()) {
        (_, Ok(m)) => m,
        (Some((_, rows)), Err(_)) => return lookup(rows),
        (None, Err(_)) => REBUILD.lock().unwrap(),
    };
    // Rebuilt only when a set record or the library changed.
    let stamp = context_stamp();
    if let Some((known, rows)) = &current
        && *known == stamp
    {
        return lookup(rows);
    }
    if current.is_none()
        && let Some((_, _, rows)) = CATALOGUE.lock().unwrap().as_ref()
    {
        // Another caller finished the first build while this one waited.
        return lookup(rows);
    }
    let mut rows = HashMap::new();
    if let (Ok(cache), Ok(db)) = (
        patch_stems::cache::Cache::default_cache(),
        patch_library::open_default(),
    ) {
        let paths: HashMap<String, String> = db
            .tracks()
            .unwrap_or_default()
            .into_iter()
            .map(|t| (t.track_uuid, t.path))
            .collect();
        let path_of = |id: &str| paths.get(id).cloned().unwrap_or_else(|| id.to_string());
        // Each manifest (megabytes: activity curves) is parsed only when
        // its file changed; a library write just maps the paths again.
        let records = parsed.list(&cache.metadata, patch_stems::cache::is_set_file, |record| {
            if !record["set"].is_object() {
                return None;
            }
            let set =
                serde_json::from_value::<patch_core::stems::StemSet>(record["set"].clone()).ok()?;
            if !set.playback_ready() {
                return None;
            }
            let aliases = record["aliases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            // Evicted audio keeps its analysis: the activity curves stay
            // evidence for planning and rehearsal (Stale: nothing plays).
            let resident = record["resident"] == true;
            let context = patch_core::stems::StemContext {
                availability: if resident {
                    patch_core::stems::Availability::Ready
                } else {
                    patch_core::stems::Availability::Stale
                },
                set_id: resident.then(|| set.id.clone()),
                available: if resident {
                    set.stems.keys().cloned().collect()
                } else {
                    vec![]
                },
                gains: set.stems.keys().map(|k| (k.clone(), 1.0)).collect(),
                // Each stem's activity, and its low band as "<stem>:low".
                activity: set
                    .stems
                    .iter()
                    .map(|(k, s)| (k.clone(), s.activity.clone()))
                    .chain(
                        set.stems
                            .iter()
                            .filter(|(_, s)| !s.low_activity.is_empty())
                            .map(|(k, s)| (format!("{k}:low"), s.low_activity.clone())),
                    )
                    .collect(),
                activity_hz: set
                    .stems
                    .iter()
                    .flat_map(|(k, s)| {
                        [
                            (k.clone(), s.activity_hz.max(1)),
                            (format!("{k}:low"), s.activity_hz.max(1)),
                        ]
                    })
                    .collect(),
                ..Default::default()
            };
            Some(Arc::new((resident, set.track_id, aliases, Arc::new(context))))
        });
        for record in records {
            let (resident, track_id, aliases, context) = &*record;
            // A resident set wins over an evicted one of the same track.
            let mut put = |path: String| {
                if *resident || !rows.contains_key(&path) {
                    rows.insert(path, context.clone());
                }
            };
            put(path_of(track_id));
            for alias in aliases {
                put(path_of(alias));
            }
        }
    }
    let rows: Rows = Arc::new(rows);
    let found = lookup(&rows);
    *CATALOGUE.lock().unwrap() = Some((Instant::now(), stamp, rows));
    found
}

/// Verified decoding shared by live playback and offline rehearsal.
pub fn decode(source: &str, key: &str, rate: u32) -> Result<crate::deck::stems::Audio, String> {
    let lease = patch_stems::cache::Cache::default_cache()?.acquire(key)?;
    decode_lease(source, lease, rate)
}

/// Decode using the exact cache lease retained by performance preflight.
/// Staging consumes that prepared identity instead of resolving through the
/// process default cache a second time.
pub fn decode_lease(
    source: &str,
    lease: patch_stems::cache::Lease,
    rate: u32,
) -> Result<crate::deck::stems::Audio, String> {
    if !lease.set.playback_ready()
        || patch_stems::hash_file(Path::new(source))? != lease.set.source_hash
    {
        return Err("Prepared stems are stale or incomplete".into());
    }
    let mut sources = Vec::new();
    for id in &lease.set.layout {
        let artifact = &lease.set.stems[id];
        let path = lease.directory.join(&artifact.cache_ref);
        if patch_stems::hash_file(&path)? != artifact.checksum {
            return Err("Stem cache checksum mismatch".into());
        }
        sources.push(
            crate::deck::pcm::Pcm::load_stem(
                &path,
                rate,
                patch_stems::jobs::stored_gain(&artifact.codec),
            )
            .map_err(|error| {
                format!(
                    "Could not decode cached stem {} in {}: {error}",
                    id, lease.set.id
                )
            })?,
        );
    }
    if sources.iter().any(|s| s.frames() != sources[0].frames()) {
        return Err("Decoded stems are misaligned".into());
    }
    Ok(crate::deck::stems::Audio {
        source_path: source.into(),
        sources,
        ids: lease.set.layout.clone(),
        set: lease.set.clone(),
        lease,
        render: None,
    })
}

pub fn attach_cached(params: Arc<Params>, deck: usize, source: String) {
    let _ = std::thread::Builder::new()
        .name("patch-stem-cache-check".into())
        .spawn(move || {
            let key = context(&source).set_id.or_else(|| {
                let cache = patch_stems::cache::Cache::default_cache().ok()?;
                let sets = cache.list();
                if sets.is_empty() {
                    return None;
                }
                let hash = patch_stems::hash_file(Path::new(&source)).ok()?;
                let key = sets
                    .iter()
                    .filter(|s| s["resident"] == true && s["set"]["source_hash"] == hash)
                    .max_by_key(|s| s["set"]["created"].as_u64().unwrap_or(0))?["set"]["id"]
                    .as_str()?
                    .to_string();
                let _ = cache.associate(&key, &source);
                Some(key)
            });
            let Some(key) = key else {
                return;
            };
            // A load is asynchronous; wait only in this preparation worker.
            for _ in 0..100 {
                if params.decks[deck].status.lock().unwrap().path.as_ref() == source {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            let sh = &params.decks[deck];
            if sh.status.lock().unwrap().path.as_ref() != source {
                return;
            }
            match decode(&source, &key, sh.rate.load(Ordering::Acquire)) {
                Ok(audio) if sh.status.lock().unwrap().path.as_ref() == source => {
                    sh.send(crate::deck::transport::Cmd::AttachStems(audio.into()))
                }
                Ok(_) => {}
                Err(e) => *sh.stem_error.lock().unwrap() = Some(Arc::from(e)),
            }
        });
}

fn backend_ready() -> bool {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let path = std::env::var_os("PATCH_STEM_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| home.join(".config"))
                .join("omarchy-patch/stems.json")
        });
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .is_some_and(|v| {
            v["model"]["python"]
                .as_str()
                .is_some_and(|s| Path::new(s).is_file())
                && v["model"]["weights"]
                    .as_str()
                    .is_some_and(|s| Path::new(s).is_file())
        })
}

pub fn live_context(params: &Params, deck: usize, context: &mut patch_core::stems::StemContext) {
    let sh = &params.decks[deck];
    let controls = sh.stems.snapshot();
    if controls["ready"] != true {
        context.availability = patch_core::stems::Availability::Unavailable;
        return;
    }
    // Copy the set out and drop the guard before cloning activity. A guard in
    // the `if let` scrutinee would stay held for this whole block, and the
    // deck worker must be able to try_lock the same mutex.
    let set = sh.stem_set.lock().unwrap().clone();
    if let Some(set) = set {
        context.set_id = Some(set.id.clone());
        context.activity = set
            .stems
            .iter()
            .map(|(k, s)| (k.clone(), s.activity.clone()))
            .collect();
        context.available = set.stems.keys().cloned().collect();
        context.availability = patch_core::stems::Availability::Ready;
    }
    context.gains.clear();
    context.muted.clear();
    context.soloed.clear();
    context.human_owned.clear();
    for slot in controls["controls"].as_array().into_iter().flatten() {
        let Some(id) = slot["id"].as_str() else {
            continue;
        };
        context
            .gains
            .insert(id.into(), slot["gain"].as_f64().unwrap_or(1.0) as f32);
        if slot["mute"] == true {
            context.muted.push(id.into());
        }
        if slot["solo"] == true {
            context.soloed.push(id.into());
        }
        if slot["human"] == true {
            context.human_owned.push(id.into());
        }
    }
}
