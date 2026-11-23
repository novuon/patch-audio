//! Demand-driven preparation. Never called from discovery or broad analysis.
use crate::{
    atomic_json,
    cache::Cache,
    digest, hash_file, now,
    separator::{Backend, StemSeparator},
};
use patch_core::stems::{StemArtifact, StemSet};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::{Path, PathBuf},
    process::Command,
    sync::{Condvar, Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const PIPELINE_VERSION: u32 = 4;
/// How new stems are stored: 16-bit WavPack, 6 dB under their level.
pub const STORED_CODEC: &str = "wavpack-s16-h6";

/// The gain that restores a stored stem to its separated level
/// (`wavpack-f32` sets from before pipeline 3 are stored as they are).
pub fn stored_gain(codec: &str) -> f32 {
    match codec {
        "wavpack-s16-h6" => 10f32.powf(6.0 / 20.0),
        _ => 1.0,
    }
}
pub const ACTIVITY_HZ: u32 = 4;

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

/// `rms_activity` below 120 Hz (two one-pole low-passes on the mono sum).
fn low_activity(samples: &[f32], rate: u32, activity_hz: u32) -> Vec<f32> {
    let k = (-std::f32::consts::TAU * 120.0 / rate.max(1) as f32).exp();
    let (mut a, mut b) = (0.0f32, 0.0f32);
    let low: Vec<f32> = samples
        .chunks_exact(2)
        .flat_map(|f| {
            a = (1.0 - k) * (f[0] + f[1]) * 0.5 + k * a;
            b = (1.0 - k) * a + k * b;
            [b, b]
        })
        .collect();
    rms_activity(&low, rate, activity_hz)
}

fn rms_activity(samples: &[f32], rate: u32, activity_hz: u32) -> Vec<f32> {
    samples
        .chunks((rate as usize / activity_hz.max(1) as usize).max(1) * 2)
        .map(|chunk| {
            (chunk
                .iter()
                .map(|sample| f64::from(*sample).powi(2))
                .sum::<f64>()
                / chunk.len().max(1) as f64)
                .sqrt() as f32
        })
        .collect()
}
const QUEUE_FULL: &str = "Stem preparation queue is full";

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Request {
    pub source: PathBuf,
    pub track: String,
    pub pin: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) batch: Option<String>,
}

static JOBS: OnceLock<Mutex<HashMap<PathBuf, Request>>> = OnceLock::new();
fn running() -> &'static Mutex<HashMap<PathBuf, Request>> {
    JOBS.get_or_init(Default::default)
}
static QUEUE: OnceLock<(Mutex<VecDeque<PathBuf>>, Condvar)> = OnceLock::new();
fn queue() -> &'static (Mutex<VecDeque<PathBuf>>, Condvar) {
    QUEUE.get_or_init(Default::default)
}

pub(crate) fn new_id() -> String {
    format!(
        "{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id(),
    )
}

pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 100 && id.bytes().all(|b| b.is_ascii_digit() || b == b'-')
}

pub(crate) fn path(cache: &Cache, id: &str) -> PathBuf {
    cache.metadata.join(format!("job-{id}.json"))
}

pub(crate) fn is_running(cache: &Cache, id: &str) -> bool {
    running().lock().unwrap().contains_key(&path(cache, id))
}

pub(crate) fn read_record(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn marker(cache: &Cache, action: &str, id: &str) -> PathBuf {
    cache.metadata.join("locks").join(format!("{action}-{id}"))
}

pub(crate) fn pending(record: &Value) -> bool {
    matches!(record["state"].as_str(), Some("queued" | "preparing"))
}

pub fn cancel(cache: &Cache, id: &str) -> Result<(), String> {
    if !cancel_pending(cache, id)? {
        return Err("Job no longer running".into());
    }
    Ok(())
}

pub(crate) fn cancel_pending(cache: &Cache, id: &str) -> Result<bool, String> {
    if !valid_id(id) || !read_record(&path(cache, id)).is_some_and(|r| pending(&r)) {
        return Ok(false);
    }
    atomic_json(&marker(cache, "cancel", id), &json!(true))?;
    queue().1.notify_all();
    Ok(true)
}

fn cancelled(cache: &Cache, id: &str, batch: Option<&str>) -> bool {
    marker(cache, "cancel", id).is_file()
        || batch.is_some_and(|batch| crate::batches::cancelled(cache, batch))
}

fn pinned(cache: &Cache, id: &str) -> bool {
    marker(cache, "pin", id).is_file() || cache.metadata.join(format!("pin-{id}.json")).is_file()
}

pub fn key(source_hash: &str, separator: &dyn StemSeparator, backend: &Backend) -> String {
    digest(
        &serde_json::to_vec(
            &json!({"source":source_hash,"model_id":separator.model().id,"model_version":separator.model().version,"weights_digest":separator.model().weights_digest,"layout":separator.model().layout,"profile":separator.model().profile,"backend":backend,"pipeline":PIPELINE_VERSION}),
        )
        .unwrap(),
    )
}
pub fn request(
    cache: Cache,
    source: PathBuf,
    track: String,
    separator: Box<dyn StemSeparator>,
    backend: Backend,
    pin: bool,
) -> Result<Value, String> {
    if !source.is_file() {
        return Err("Source audio unavailable".into());
    }
    let mut jobs = running().lock().unwrap();
    for (path, request) in jobs.iter() {
        if path.parent() != Some(cache.metadata.as_path())
            || request.source != source
            || request.batch.is_some()
        {
            continue;
        }
        let Some(record) = read_record(path).filter(pending) else {
            continue;
        };
        let Some(id) = record["id"].as_str() else {
            continue;
        };
        if cancelled(&cache, id, None) {
            continue;
        }
        if pin {
            atomic_json(&marker(&cache, "pin", id), &json!(true))?;
            if let Some(key) = record["key"].as_str() {
                cache.pin(key, &track, true)?;
            }
        }
        return Ok(json!({"id":id,"state":record["state"]}));
    }
    let id = new_id();
    let request = Request {
        source,
        track,
        pin,
        batch: None,
    };
    launch(
        &cache,
        &id,
        request,
        None,
        Ok((separator, backend)),
        &mut jobs,
    )
    .map_err(|e| e.to_string())?;
    Ok(json!({"id":id,"state":"queued"}))
}

#[derive(Debug)]
pub(crate) enum LaunchError {
    Full,
    Other(String),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full => f.write_str(QUEUE_FULL),
            Self::Other(error) => f.write_str(error),
        }
    }
}

impl From<String> for LaunchError {
    fn from(error: String) -> Self {
        Self::Other(error)
    }
}

pub type Separator = (Box<dyn StemSeparator>, Backend);

/// Submit a batch's current item or adopt its existing job record. The record
/// is created under the same process lock used by a successor to claim it.
pub(crate) fn ensure(
    cache: &Cache,
    id: &str,
    request: Request,
    record: Option<Value>,
    separator: Result<Separator, String>,
) -> Result<bool, LaunchError> {
    launch(
        cache,
        id,
        request,
        record,
        separator,
        &mut running().lock().unwrap(),
    )
}

fn launch(
    cache: &Cache,
    id: &str,
    request: Request,
    previous: Option<Value>,
    separator: Result<Separator, String>,
    jobs: &mut HashMap<PathBuf, Request>,
) -> Result<bool, LaunchError> {
    let path = path(cache, id);
    if jobs.contains_key(&path) {
        return Ok(false);
    }
    let Some(owner) = cache.work_lock(&format!("job-{id}"))? else {
        return Ok(false);
    };
    // Re-read after taking ownership. A previous process may have committed
    // the result between discovery and our lock acquisition.
    let existing = read_record(&path);
    if existing.as_ref().is_some_and(|r| !pending(r)) {
        return Ok(false);
    }
    if existing.is_none() && previous.is_some() {
        return Err("Preparation record disappeared".to_owned().into());
    }
    let mut record = if let Some(record) = existing {
        record
    } else {
        let mut record = serde_json::to_value(&request).map_err(|e| e.to_string())?;
        record["id"] = json!(id);
        record["started"] = json!(now());
        record
    };
    if cancelled(cache, id, request.batch.as_deref()) {
        finish(&path, &mut record, "cancelled", Some("Cancelled"))?;
        return Ok(true);
    }
    if jobs.len() >= 16 {
        return Err(LaunchError::Full);
    }
    if previous.is_some() {
        record["resumed"] = json!(true);
    }
    record["state"] = json!("queued");
    record["progress"] = json!(0);
    atomic_json(&path, &record)?;
    jobs.insert(path.clone(), request.clone());
    queue().0.lock().unwrap().push_back(path.clone());
    let worker_path = path.clone();
    let cache = cache.clone();
    let id = id.to_owned();
    if let Err(e) = std::thread::Builder::new()
        .name("patch-stem-preparation".into())
        .spawn(move || {
            // SAFETY: Linux per-thread niceness; preparation cannot compete with deck workers.
            unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 10) };
            let is_cancelled = || cancelled(&cache, &id, request.batch.as_deref());
            let (gate, wake) = queue();
            let mut waiting = gate.lock().unwrap();
            while waiting.front() != Some(&worker_path) && !is_cancelled() {
                waiting = wake
                    .wait_timeout(waiting, Duration::from_millis(250))
                    .unwrap()
                    .0;
            }
            drop(waiting);
            record["state"] = json!("preparing");
            record["progress"] = json!(1);
            let result = catch_worker_panic(std::panic::AssertUnwindSafe(|| {
                if is_cancelled() {
                    return Err("Cancelled".into());
                }
                if !request.source.is_file() {
                    return Err("Source audio unavailable".into());
                }
                atomic_json(&worker_path, &record)?;
                // The previous process may have published before it could
                // mark the job ready. Its result is usable even if the
                // separator is no longer configured.
                if let Some(key) = record["key"].as_str()
                    && let Ok(lease) = cache.acquire(key)
                    && hash_file(&request.source)? == lease.set.source_hash
                {
                    return reuse(&cache, &request, &id, lease);
                }
                let (separator, backend) = separator?;
                prepare(
                    &cache,
                    &request,
                    separator.as_ref(),
                    &backend,
                    &is_cancelled,
                    &worker_path,
                    &mut record,
                )
            }));
            let mut jobs = running().lock().unwrap();
            let written = match result {
                Ok(Ok(set)) => {
                    if pinned(&cache, &id) {
                        let _ = cache.pin(&set.id, &request.track, true);
                    }
                    record["progress"] = json!(100);
                    record["set"] = json!(set);
                    finish(&worker_path, &mut record, "ready", None)
                }
                Ok(Err(error)) if is_cancelled() => {
                    finish(&worker_path, &mut record, "cancelled", Some(&error))
                }
                Ok(Err(error)) => finish(&worker_path, &mut record, "failed", Some(&error)),
                Err(_) => finish(
                    &worker_path,
                    &mut record,
                    "failed",
                    Some("Preparation worker failed"),
                ),
            };
            if let Err(error) = written {
                eprintln!("patch stems: could not save job {id}: {error}");
            }
            drop(owner);
            jobs.remove(&worker_path);
            gate.lock().unwrap().retain(|p| p != &worker_path);
            wake.notify_all();
        })
    {
        jobs.remove(&path);
        queue().0.lock().unwrap().retain(|p| p != &path);
        queue().1.notify_all();
        return Err(e.to_string().into());
    }
    Ok(true)
}

fn finish(path: &Path, record: &mut Value, state: &str, error: Option<&str>) -> Result<(), String> {
    record["state"] = json!(state);
    record["error"] = json!(error);
    record["finished"] = json!(now());
    atomic_json(path, record)
}

/// Recover standalone work once the previous engine has handed over.
/// Batch children are recovered by their parent and retain its cancellation.
pub fn recover(
    cache: &Cache,
    memo: &mut crate::cache::Memo<Value>,
    configured: &impl Fn() -> Result<Separator, String>,
    mut on_recovered: impl FnMut(&str, &Path),
) -> Result<bool, String> {
    let mut pending_jobs = memo
        .list(&cache.metadata, is_job_file, |r| {
            pending(r).then(|| r.clone())
        })
        .into_iter()
        .filter(|record| {
            record["batch"].is_null()
                || record["batch"]
                    .as_str()
                    .is_some_and(|id| crate::batches::cancelled(cache, id))
        })
        .collect::<Vec<_>>();
    pending_jobs.sort_by_key(|r| r["started"].as_u64().unwrap_or(0));
    for record in pending_jobs {
        let Some(id) = record["id"].as_str().filter(|id| valid_id(id)) else {
            continue;
        };
        if running().lock().unwrap().contains_key(&path(cache, id)) {
            continue;
        }
        let mut request: Request = match serde_json::from_value(record.clone()) {
            Ok(request) => request,
            Err(_) => {
                if let Some(_owner) = cache.work_lock(&format!("job-{id}"))?
                    && let Some(mut current) = read_record(&path(cache, id)).filter(pending)
                {
                    finish(
                        &path(cache, id),
                        &mut current,
                        "failed",
                        Some("Saved preparation job is incomplete"),
                    )?;
                }
                continue;
            }
        };
        request.pin |= pinned(cache, id);
        let standalone = request.batch.is_none();
        let source = request.source.clone();
        match ensure(cache, id, request, Some(record.clone()), configured()) {
            Ok(true) if standalone => on_recovered(id, &source),
            Ok(_) => {}
            Err(LaunchError::Full) => return Ok(true),
            Err(LaunchError::Other(error)) => return Err(error),
        }
        if !running().lock().unwrap().contains_key(&path(cache, id))
            && read_record(&path(cache, id)).is_some_and(|r| pending(&r))
        {
            // Another process still owns this job.
            return Ok(true);
        }
    }
    Ok(false)
}

fn reuse(
    cache: &Cache,
    request: &Request,
    job: &str,
    lease: crate::cache::Lease,
) -> Result<StemSet, String> {
    cache.associate(&lease.set.id, &request.track)?;
    if request.pin || pinned(cache, job) {
        cache.pin(&lease.set.id, &request.track, true)?;
    }
    Ok(lease.set.clone())
}

fn prepare(
    cache: &Cache,
    request: &Request,
    separator: &dyn StemSeparator,
    backend: &Backend,
    cancel: &dyn Fn() -> bool,
    path: &Path,
    record: &mut Value,
) -> Result<StemSet, String> {
    let source = &request.source;
    let track = &request.track;
    let job = record["id"].as_str().unwrap().to_owned();
    let source_hash = hash_file(source)?;
    let id = key(&source_hash, separator, backend);
    record["key"] = json!(id);
    atomic_json(path, record)?;
    let _job = loop {
        if cancel() {
            return Err("Cancelled".into());
        }
        if let Some(guard) = cache.work_lock(&format!("{id}.job"))? {
            break guard;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // A crash between the object rename and catalogue update leaves a valid
    // manifest in the audio directory. Repair it before deciding to separate.
    if let Ok(lease) = cache.acquire(&id) {
        return reuse(cache, request, &job, lease);
    }
    cache.cleanup()?;
    let source_bytes = std::fs::metadata(source).map_err(|e| e.to_string())?.len();
    if cache.free_bytes()? < source_bytes.saturating_add(256 * 1024 * 1024) {
        return Err("Not enough free space to prepare stems safely".into());
    }
    let staging = cache.audio.join(format!(".staging-{job}.{}", new_id()));
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let result = (|| {
        let snapshot = staging.join("source.input");
        std::fs::copy(source, &snapshot).map_err(|e| e.to_string())?;
        if hash_file(&snapshot)? != source_hash {
            return Err("Source changed while preparing".into());
        }
        separator.separate(&snapshot, &staging, backend, cancel, &mut |p| {
            if record["progress"].as_u64().unwrap_or(0) != p as u64 {
                record["progress"] = json!(p);
                let _ = atomic_json(path, record);
            }
        })?;
        record["progress"] = json!(85);
        atomic_json(path, record)?;
        let description: Value = serde_json::from_slice(
            &std::fs::read(staging.join("output.json")).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let rate = description["sample_rate"]
            .as_u64()
            .filter(|r| (8000..=192000).contains(r))
            .ok_or("Invalid separator sample rate")? as u32;
        let frames = description["frames"]
            .as_u64()
            .filter(|f| *f > 0)
            .ok_or("Invalid separator duration")?;
        // Separator outputs must match the authoritative input at their common rate.
        let mut probe = Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-i"])
            .arg(&snapshot)
            .args(["-f", "f32le", "-ac", "2", "-ar"])
            .arg(rate.to_string())
            .arg("pipe:1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let decoded = std::io::copy(
            &mut probe.stdout.take().ok_or("Missing source decoder output")?,
            &mut std::io::sink(),
        )
        .map_err(|e| e.to_string());
        let status = probe.wait().map_err(|e| e.to_string())?;
        if !status.success() || decoded? != frames.saturating_mul(8) {
            return Err("Separator duration does not match the original audio".into());
        }
        let mut stems = BTreeMap::new();
        for stem in &separator.model().layout {
            if cancel() {
                return Err("Cancelled".into());
            }
            if description["available"]
                .as_array()
                .is_none_or(|a| !a.iter().any(|s| s == stem))
            {
                continue;
            }
            let wav = staging.join(format!("{stem}.wav"));
            let reference = format!("{stem}.wv");
            let target = staging.join(&reference);
            let output = Command::new("ffmpeg")
                .args(["-v", "error", "-nostdin", "-i"])
                .arg(&wav)
                // 16-bit with 6 dB of headroom (separated stems peak up to
                // ~+4 dBFS) and TPDF dither: a third of float32's size, its
                // noise floor far under the separation's own residual.
                .args(["-af", "volume=-6dB,aresample=dither_method=triangular"])
                .args([
                    "-c:a",
                    "wavpack",
                    "-sample_fmt",
                    "s16p",
                    "-compression_level",
                    "1",
                    "-y",
                ])
                .arg(&target)
                .output()
                .map_err(|e| e.to_string())?;
            if !output.status.success() {
                return Err(format!(
                    "Stem encoding: {}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            let raw = Command::new("ffmpeg")
                .args(["-v", "error", "-nostdin", "-i"])
                .arg(&target)
                .args(["-f", "f32le", "-ac", "2", "pipe:1"])
                .output()
                .map_err(|e| e.to_string())?;
            if !raw.status.success() || raw.stdout.len() as u64 != frames * 8 {
                return Err("Stem frame alignment mismatch".into());
            }
            let gain = stored_gain(STORED_CODEC);
            let samples: Vec<f32> = raw
                .stdout
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap()) * gain)
                .collect();
            if samples.iter().any(|s| !s.is_finite()) {
                return Err("Nonfinite separated audio".into());
            }
            let activity = rms_activity(&samples, rate, ACTIVITY_HZ);
            let low_activity = low_activity(&samples, rate, ACTIVITY_HZ);
            let waveform = samples
                .chunks((rate as usize / 20).max(1) * 2)
                .map(|c| c.iter().fold(0.0f32, |m, s| m.max(s.abs())))
                .collect();
            stems.insert(
                stem.clone(),
                StemArtifact {
                    chunks: vec![],
                    id: stem.clone(),
                    cache_ref: reference,
                    codec: STORED_CODEC.into(),
                    checksum: hash_file(&target)?,
                    bytes: std::fs::metadata(&target).map_err(|e| e.to_string())?.len(),
                    frames,
                    start_frame: 0,
                    peak: samples.iter().fold(0.0f32, |m, s| m.max(s.abs())),
                    activity_hz: ACTIVITY_HZ,
                    activity,
                    low_activity,
                    waveform,
                },
            );
            std::fs::remove_file(wav).map_err(|e| e.to_string())?;
        }
        if stems.is_empty() {
            return Err("Separator produced no requested stems".into());
        }
        for file in [
            "source.input",
            "output.json",
            "adapter.log",
            "progress.json",
        ] {
            let _ = std::fs::remove_file(staging.join(file));
        }
        let m = separator.model();
        let set = StemSet {
            version: 1,
            id: id.clone(),
            source_hash,
            track_id: track.into(),
            model_id: m.id.clone(),
            model_version: m.version.clone(),
            weights_digest: m.weights_digest.clone(),
            profile: m.profile.clone(),
            pipeline_version: PIPELINE_VERSION,
            backend: format!("{}:{}:{}", backend.id, backend.device, backend.precision),
            sample_rate: rate,
            frames,
            duration: frames as f64 / rate as f64,
            layout: m.layout.clone(),
            stems,
            created: now(),
        };
        let pin = request.pin || pinned(cache, &job);
        let bytes = set.stems.values().map(|s| s.bytes).sum::<u64>();
        if bytes > cache.budget && !pin {
            return Err(
                "Stem audio exceeds the configured cache budget; increase it or pin this track"
                    .into(),
            );
        }
        if cancel() {
            return Err("Cancelled".into());
        }
        if pin {
            cache.pin(&id, track, true)?;
        }
        cache.publish(&set, &staging)?;
        cache.associate(&id, track)?;
        if pin {
            cache.pin(&id, track, true)?;
        }
        let _lease = cache.acquire(&id)?;
        cache.cleanup()?;
        Ok(set)
    })();
    if staging.exists() {
        let _ = std::fs::remove_dir_all(staging);
    }
    result
}

#[cfg(test)]
mod activity_tests {
    use super::*;

    #[test]
    fn stem_rms_activity_has_declared_quarter_second_resolution() {
        let audio = [0.0, 0.0, 0.4, 0.4, 0.2, 0.2, 0.0, 0.0];
        let activity = rms_activity(&audio, 4, ACTIVITY_HZ);
        assert_eq!(activity.len(), 4);
        assert_eq!(activity[0], 0.0);
        assert!((activity[1] - 0.4).abs() < 1e-6);
        assert!((activity[2] - 0.2).abs() < 1e-6);
        assert_eq!(activity[3], 0.0);
    }
}
/// A job's record file (`job-*.json`).
pub fn is_job_file(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with("job-"))
        && path.extension().is_some_and(|x| x == "json")
}

/// Read compact records without re-parsing their audio metadata on each poll.
pub fn list_memo(
    cache: &Cache,
    memo: &mut crate::cache::Memo<Value>,
    f: impl Fn(&Value) -> Value,
) -> Vec<Value> {
    memo.list(&cache.metadata, is_job_file, |v| Some(f(v)))
}

pub fn list(cache: &Cache) -> Vec<Value> {
    std::fs::read_dir(&cache.metadata)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| is_job_file(&e.path()))
        .filter_map(|e| read_record(&e.path()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::separator::{InferenceBackend, Model};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    struct Fixture {
        model: Model,
        release: Arc<AtomicBool>,
        calls: Arc<AtomicUsize>,
    }
    impl StemSeparator for Fixture {
        fn model(&self) -> &Model {
            &self.model
        }
        fn separate(
            &self,
            source: &Path,
            out: &Path,
            _: &dyn InferenceBackend,
            cancel: &dyn Fn() -> bool,
            progress: &mut dyn FnMut(u8),
        ) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            while !self.release.load(Ordering::Acquire) {
                if cancel() {
                    return Err("Cancelled".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            for id in patch_core::stems::CANONICAL {
                let result = Command::new("ffmpeg")
                    .args(["-v", "error", "-i"])
                    .arg(source)
                    .args(["-af", "volume=0.25", "-c:a", "pcm_f32le", "-y"])
                    .arg(out.join(format!("{id}.wav")))
                    .status()
                    .map_err(|e| e.to_string())?;
                if !result.success() {
                    return Err("Fixture decode failed".into());
                }
            }
            progress(80);
            atomic_json(
                &out.join("output.json"),
                &json!({"sample_rate":48000,"frames":4800,"available":patch_core::stems::CANONICAL}),
            )
        }
    }
    #[test]
    fn bounded_serial_queue_deduplicates_cancels_and_reuses_renamed_audio() {
        let root =
            std::env::temp_dir().join(format!("patch-stem-jobs-{}-{}", std::process::id(), now()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.wav");
        assert!(
            Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:sample_rate=48000:duration=0.1",
                    "-ac",
                    "2",
                    "-c:a",
                    "pcm_f32le",
                    "-y"
                ])
                .arg(&source)
                .status()
                .unwrap()
                .success()
        );
        let cache = Cache::new(root.join("audio"), root.join("state"), 1024 * 1024).unwrap();
        let release = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let model = Model {
            id: "fixture".into(),
            version: "1".into(),
            weights_digest: "a".repeat(64),
            layout: patch_core::stems::CANONICAL
                .iter()
                .map(|s| s.to_string())
                .collect(),
            profile: "test".into(),
            adapter: "unused".into(),
            python: "unused".into(),
            weights: None,
        };
        let backend = Backend {
            id: "fixture".into(),
            device: "cpu".into(),
            precision: "float32".into(),
            threads: 1,
        };
        let submit = |path: PathBuf, pin| {
            request(
                cache.clone(),
                path.clone(),
                path.to_string_lossy().into_owned(),
                Box::new(Fixture {
                    model: model.clone(),
                    release: release.clone(),
                    calls: calls.clone(),
                }),
                backend.clone(),
                pin,
            )
        };
        let first = submit(source.clone(), false).unwrap();
        let duplicate = submit(source.clone(), true).unwrap();
        assert_eq!(first["id"], duplicate["id"]);
        let mut requests = vec![first];
        for i in 1..16 {
            let renamed = root.join(format!("renamed-{i}.wav"));
            std::fs::copy(&source, &renamed).unwrap();
            requests.push(submit(renamed, false).unwrap());
        }
        let overflow = root.join("overflow.wav");
        std::fs::copy(&source, &overflow).unwrap();
        assert!(
            submit(overflow, false)
                .unwrap_err()
                .contains("queue is full")
        );
        cancel(&cache, requests[15]["id"].as_str().unwrap()).unwrap();
        release.store(true, Ordering::Release);
        let timeout = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            let rows = list(&cache);
            if rows.len() == 16
                && rows
                    .iter()
                    .all(|r| matches!(r["state"].as_str(), Some("ready" | "cancelled")))
            {
                break;
            }
            assert!(
                std::time::Instant::now() < timeout,
                "jobs did not finish: {rows:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            calls.load(Ordering::Acquire),
            1,
            "renamed identical sources must reuse prepared audio"
        );
        let records = cache.list();
        assert_eq!(records.len(), 1);
        assert!(!records[0]["pins"].as_array().unwrap().is_empty());
        assert_eq!(records[0]["aliases"].as_array().unwrap().len(), 15);
        let key = records[0]["set"]["id"].as_str().unwrap();
        let lease = cache.acquire(key).unwrap();
        assert!(lease.set.complete());
        assert_eq!(lease.set.pipeline_version, PIPELINE_VERSION);
        assert!(
            lease
                .set
                .stems
                .values()
                .all(|stem| stem.activity_hz == ACTIVITY_HZ)
        );
        drop(lease);
        std::fs::remove_dir_all(root).unwrap();
    }
}
