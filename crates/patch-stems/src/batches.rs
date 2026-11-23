//! Durable collection preparation. One track per batch occupies the job queue.
use crate::{
    atomic_json,
    cache::{Cache, Memo},
    jobs::{self, LaunchError, Request, Separator},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
pub struct Track {
    pub source: PathBuf,
    pub track: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct Batch {
    id: String,
    collections: Vec<i64>,
    tracks: Vec<Track>,
    pin: bool,
    state: String,
    next: usize,
    complete: usize,
    failed: usize,
    error: Option<String>,
    resumed: bool,
    #[serde(default)]
    worker_pid: Option<u32>,
}

impl Batch {
    fn current_job(&self) -> String {
        format!("{}-{}", self.id.trim_start_matches("batch-"), self.next)
    }

    fn save(&self, cache: &Cache) -> Result<(), String> {
        atomic_json(
            &path(cache, &self.id),
            &serde_json::to_value(self).map_err(|e| e.to_string())?,
        )
    }

    fn failed(&mut self, error: &str) {
        self.failed += 1;
        self.error.get_or_insert_with(|| error.to_owned());
    }
}

fn ids(collections: &[i64]) -> Vec<i64> {
    let mut ids = collections.to_vec();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn valid_id(id: &str) -> bool {
    id.strip_prefix("batch-").is_some_and(jobs::valid_id)
}

fn is_batch_file(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "json")
        && path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(valid_id)
}

fn path(cache: &Cache, id: &str) -> PathBuf {
    cache.metadata.join(format!("{id}.json"))
}

fn marker(cache: &Cache, id: &str) -> PathBuf {
    cache.metadata.join("locks").join(format!("cancel-{id}"))
}

pub(crate) fn cancelled(cache: &Cache, id: &str) -> bool {
    valid_id(id) && marker(cache, id).is_file()
}

fn pending(state: &str) -> bool {
    matches!(state, "queued" | "preparing" | "submitted")
}

fn records(
    cache: &Cache,
    memo: &mut Memo<Value>,
    compact: impl Fn(&Value) -> Option<Value>,
) -> Vec<Value> {
    memo.list(&cache.metadata, is_batch_file, compact)
}

pub fn submit(
    cache: &Cache,
    collections: &[i64],
    tracks: Vec<Track>,
    pin: bool,
    keep_refs: &[&str],
) -> Result<(String, bool), String> {
    let _catalog = cache.batch_lock()?;
    let collections = ids(collections);
    // The collection membership is a snapshot. Changes made while the batch
    // runs take effect on the next explicit preparation.
    if let Some(id) = records(cache, &mut Memo::default(), |r| {
        let id = r["id"].as_str().filter(|id| valid_id(id))?;
        (pending(r["state"].as_str()?)
            && r["collections"] == json!(collections)
            && r["pin"] == pin
            && !cancelled(cache, id))
        .then(|| json!(id))
    })
    .into_iter()
    .next()
    .and_then(|v| v.as_str().map(str::to_owned))
    {
        return Ok((id, true));
    }
    if pin {
        let keep = tracks.iter().map(|t| t.track.clone()).collect();
        cache.release_pins_except(&keep, keep_refs)?;
    }
    let batch = Batch {
        id: format!("batch-{}", jobs::new_id()),
        collections,
        tracks,
        pin,
        state: "queued".into(),
        next: 0,
        complete: 0,
        failed: 0,
        error: None,
        resumed: false,
        worker_pid: Some(std::process::id()),
    };
    batch.save(cache)?;
    Ok((batch.id, false))
}

pub fn cancel(cache: &Cache, id: &str) -> Result<(), String> {
    if !valid_id(id) {
        return Err("Invalid preparation batch".into());
    }
    let _catalog = cache.batch_lock()?;
    let path = path(cache, id);
    let mut record = jobs::read_record(&path).ok_or("Preparation batch not found")?;
    if matches!(
        record["state"].as_str(),
        Some("failed" | "ready" | "cancelled")
    ) {
        return Ok(());
    }
    // The child checks this independently. A crash after writing the
    // marker and before updating the status still cancels all remaining work.
    atomic_json(&marker(cache, id), &json!(true))?;
    cancel_legacy_children(cache, &record)?;
    record["state"] = json!("cancelled");
    atomic_json(&path, &record)
}

fn cancel_legacy_children(cache: &Cache, record: &Value) -> Result<(), String> {
    for id in legacy_children(record) {
        jobs::cancel_pending(cache, id)?;
    }
    Ok(())
}

fn legacy_children(record: &Value) -> impl Iterator<Item = &str> {
    record["children"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|id| jobs::valid_id(id))
}

/// The old records did not contain the remaining track list. Preserve jobs
/// that were admitted, and tell the DJ when the rest needs to be requested.
pub fn recover_legacy(cache: &Cache) -> Result<(), String> {
    let _catalog = cache.batch_lock()?;
    for entry in std::fs::read_dir(&cache.metadata)
        .map_err(|e| e.to_string())?
        .flatten()
    {
        let path = entry.path();
        if !is_batch_file(&path) {
            continue;
        }
        let Some(mut record) = jobs::read_record(&path).filter(|r| r["tracks"].is_null()) else {
            continue;
        };
        let id = path.file_stem().and_then(|s| s.to_str()).unwrap();
        if record["cancel"] == true || record["state"] == "cancelled" || cancelled(cache, id) {
            cancel_legacy_children(cache, &record)?;
            record["state"] = json!("cancelled");
        } else if matches!(record["state"].as_str(), Some("queued" | "preparing")) {
            record["state"] = json!("failed");
            record["error"] = json!(
                "The earlier version did not save the track list. Press Prepare stems to retry."
            );
        } else {
            continue;
        }
        record["id"] = json!(id);
        atomic_json(&path, &record)?;
    }
    Ok(())
}

#[derive(Default)]
pub struct Runner {
    pending: Memo<Value>,
}

impl Runner {
    /// A process can die between any two writes. The saved cursor always
    /// implies the same child identity, and the child's terminal result is
    /// written before the cursor can advance.
    pub fn tick(
        &mut self,
        cache: &Cache,
        configured: &impl Fn() -> Result<Separator, String>,
    ) -> Result<bool, String> {
        let mut active = records(cache, &mut self.pending, |r| {
            (r["tracks"].is_array() && pending(r["state"].as_str()?)).then(|| r["id"].clone())
        });
        active.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        for id in active
            .iter()
            .filter_map(Value::as_str)
            .filter(|id| valid_id(id))
        {
            let _catalog = cache.batch_lock()?;
            let batch_path = path(cache, id);
            let Some(mut record) = jobs::read_record(&batch_path) else {
                continue;
            };
            let mut batch: Batch = match serde_json::from_value(record.clone()) {
                Ok(batch) => batch,
                Err(_) => {
                    record["state"] = json!("failed");
                    record["error"] = json!(
                        "Saved preparation batch is incomplete. Press Prepare stems to retry."
                    );
                    atomic_json(&batch_path, &record)?;
                    continue;
                }
            };
            if !pending(&batch.state) {
                continue;
            }
            if cancelled(cache, id) {
                batch.state = "cancelled".into();
                batch.save(cache)?;
                continue;
            }
            if let Some(track) = batch.tracks.get(batch.next) {
                let job_id = batch.current_job();
                let record = jobs::read_record(&jobs::path(cache, &job_id));
                if let Some(record) = &record
                    && !jobs::pending(record)
                {
                    batch.resumed |= record["resumed"] == true;
                    if record["state"] == "ready" {
                        batch.complete += 1;
                    } else {
                        batch.failed(
                            record["error"]
                                .as_str()
                                .unwrap_or("Stem preparation failed"),
                        );
                    }
                    batch.next += 1;
                } else if !jobs::is_running(cache, &job_id) {
                    let request = Request {
                        source: track.source.clone(),
                        track: track.track.clone(),
                        pin: batch.pin,
                        batch: Some(id.into()),
                    };
                    let restarting = batch.worker_pid != Some(std::process::id())
                        || record
                            .as_ref()
                            .is_some_and(|r| r["state"] == "preparing" || r["resumed"] == true);
                    match jobs::ensure(cache, &job_id, request, record, configured()) {
                        Ok(true) => {
                            batch.resumed |= restarting;
                            batch.worker_pid = Some(std::process::id());
                            batch.state = "preparing".into();
                        }
                        Ok(false) => continue,
                        Err(LaunchError::Full) => continue,
                        Err(LaunchError::Other(error)) => return Err(error),
                    }
                } else {
                    continue;
                }
            }
            if batch.next >= batch.tracks.len() {
                batch.state = if batch.failed == 0 { "ready" } else { "failed" }.into();
            }
            batch.save(cache)?;
        }
        Ok(!active.is_empty())
    }
}

/// Compact status for the stem panels and the Booth. Legacy submitted
/// batches can still show the progress of jobs that were already admitted.
pub fn summaries(cache: &Cache, memo: &mut Memo<Value>, jobs: &[Value]) -> Vec<Value> {
    let mut summaries = records(cache, memo, |record| {
        let admitted = record["next"]
            .as_u64()
            .map(|next| next + u64::from(record["state"] == "preparing"));
        let unrecorded = if record["tracks"].is_null() && record["state"] == "failed" {
            record["total"]
                .as_u64()
                .unwrap_or_default()
                .saturating_sub(record["admitted"].as_u64().unwrap_or_default())
        } else {
            0
        };
        Some(json!({
            "id":record["id"], "collections":record["collections"], "pin":record["pin"],
            "state":record["state"], "resumed":record["resumed"], "error":record["error"],
            "total":record["tracks"].as_array().map(|a| json!(a.len())).unwrap_or_else(|| record["total"].clone()),
            "complete":record["complete"], "failed":record["failed"], "admitted":admitted,
            "children":record["children"],
            "admission_failures":unrecorded.max(record["errors"].as_array().map_or(0, |a| a.len() as u64))
        }))
    });
    for row in &mut summaries {
        if let Some(id) = row["id"].as_str()
            && cancelled(cache, id)
        {
            row["state"] = json!("cancelled");
        }
        if row["children"].is_array() {
            let mut ready = 0;
            let mut failed = 0;
            let mut pending = 0;
            for id in legacy_children(row) {
                match jobs
                    .iter()
                    .find(|r| r["id"] == id)
                    .and_then(|r| r["state"].as_str())
                {
                    Some("ready") => ready += 1,
                    Some("queued" | "preparing") => pending += 1,
                    _ => failed += 1,
                }
            }
            let admission_failures = row["admission_failures"].as_u64().unwrap_or_default();
            row["complete"] = json!(ready);
            row["failed"] = json!(failed + admission_failures);
            row["admitted"] = json!(ready + failed + pending);
            if row["state"] == "submitted" && pending == 0 {
                row["state"] = json!(if failed + admission_failures == 0 {
                    "ready"
                } else {
                    "failed"
                });
            }
        }
        row.as_object_mut().unwrap().remove("children");
        row.as_object_mut().unwrap().remove("admission_failures");
    }
    summaries.sort_by(|a, b| b["id"].as_str().cmp(&a["id"].as_str()));
    summaries
}

pub fn for_collections(batches: &[Value], collections: &[i64]) -> Value {
    let selection = ids(collections);
    let exact = json!(selection);
    batches
        .iter()
        .rev()
        .filter(|row| {
            row["collections"].as_array().is_some_and(|items| {
                !items.is_empty()
                    && items.iter().all(|id| {
                        id.as_i64()
                            .is_some_and(|id| selection.binary_search(&id).is_ok())
                    })
            })
        })
        .max_by_key(|row| {
            (
                row["state"].as_str().is_some_and(pending),
                row["collections"] == exact,
                row["pin"] == true,
            )
        })
        .or_else(|| {
            // Older batches cannot identify the collections that spawned them.
            batches.iter().find(|r| {
                r["collections"].is_null() && r["state"] == "failed" && r["error"].is_string()
            })
        })
        .cloned()
        .unwrap_or_default()
}
