//! The librarian: keeps the library's sources (folders and drives) scanned
//! and every track analysed, with or without a Patch window.
//!
//! It runs as its own process (`omarchy-patch librarian`), started and kept
//! alive by the engine, at idle CPU and I/O priority: decoding a 14 GB
//! USB stick never touches the real-time engine or a playing deck. One
//! runs at a time (a lock file); a rebuilt one replaces it.
//!
//! What it knows goes to `$XDG_RUNTIME_DIR/omarchy-patch/library.json`
//! ([`Status`]): the Library window and `omarchy-patch library` read it.

use patch_library::{Counts, Library, Source};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Sources are scanned again this often (new files), and at once when a
/// drive is plugged in or `library rescan` asks.
const RESCAN: Duration = Duration::from_secs(15 * 60);
/// How often a source's folders are looked at for changes.
const FOLDERS: Duration = Duration::from_secs(30);

/// The change times of every folder under `root` (not hidden ones), as
/// one number: a file added, removed or renamed anywhere changes it.
fn folders_signature(root: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(meta) = std::fs::metadata(&dir) else {
            continue;
        };
        if let Ok(m) = meta.modified() {
            (dir.as_os_str(), m).hash(&mut h);
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let hidden = e.file_name().to_string_lossy().starts_with('.');
            if !hidden && e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(e.path());
            }
        }
    }
    h.finish()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SourceStatus {
    pub name: String,
    pub path: String,
    /// The folder is there (a USB stick is plugged in).
    pub connected: bool,
    /// On a removable drive.
    pub removable: bool,
    pub counts: Counts,
}

/// A drive plugged in that isn't in the library yet.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Offer {
    pub path: String,
    pub label: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Status {
    pub sources: Vec<SourceStatus>,
    /// The track being analysed now, and its source.
    pub analysing: Option<String>,
    pub analysing_source: Option<String>,
    /// The source being scanned for new files.
    pub scanning: Option<String>,
    /// Tracks waiting whose files are there (connected sources).
    pub queue: usize,
    /// Seconds per track, recently (for "about 20 min left").
    pub per_track_s: f64,
    pub offers: Vec<Offer>,
    /// Analysed tracks whose beat grid is still to be measured (see
    /// patch_analysis::grid), and the one being measured.
    #[serde(default)]
    pub grids_left: usize,
    #[serde(default)]
    pub measuring: Option<String>,
    /// Analysed tracks whose vocals are still to be measured (with the
    /// stem separator; 0 without one), and the one being measured.
    #[serde(default)]
    pub vocals_left: usize,
    #[serde(default)]
    pub voicing: Option<String>,
    /// Tracks whose original release year is still to be looked up online
    /// (0 while lookups are off).
    #[serde(default)]
    pub years_left: usize,
    /// Unix seconds; a status older than a minute means no librarian.
    pub updated: i64,
    pub pid: u32,
}

fn runtime() -> PathBuf {
    crate::daemon::socket_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir)
}

pub fn status_path() -> PathBuf {
    runtime().join("library.json")
}

fn lock_path() -> PathBuf {
    runtime().join("librarian.lock")
}

fn rescan_flag() -> PathBuf {
    runtime().join("library.rescan")
}

fn now_s() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// What the librarian last wrote (None: it doesn't run).
pub fn read_status() -> Option<Status> {
    let s: Status = serde_json::from_str(&std::fs::read_to_string(status_path()).ok()?).ok()?;
    (now_s() - s.updated < 60).then_some(s)
}

/// Ask for a scan of every source now (after `library add`).
pub fn request_rescan() {
    let _ = std::fs::write(rescan_flag(), b"");
}

/// The user's music folder (XDG), else ~/Music.
pub fn music_dir() -> PathBuf {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    std::process::Command::new("xdg-user-dir")
        .arg("MUSIC")
        .output()
        .ok()
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| home.join("Music"))
}

/// Mount points of removable drives (udisks mounts them here).
fn drive_roots() -> Vec<PathBuf> {
    let user = std::env::var("USER").unwrap_or_default();
    [format!("/run/media/{user}"), format!("/media/{user}")]
        .iter()
        .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// A drive source that isn't where it was, found mounted under a name one
/// digit apart: udisks mounts a drive at `LABEL2` when `LABEL` is still
/// taken (a stale mount point), and back at `LABEL` later (2026-11-13: the
/// library had `SAMSUNG_T52`, the drive came back as `SAMSUNG_T5`, and all
/// 15,000 tracks of the collection showed missing). (old root, new root)
/// when `roots` (mounted drives) has one that holds the source's folder and
/// the sampled tracks, each the same size.
fn moved_drive(source: &str, roots: &[PathBuf], sample: &[(String, u64)]) -> Option<(String, String)> {
    // The drive's own mount point: /run/media/<user>/<name> or /media/<user>/<name>.
    let depth = if source.starts_with("/run/media/") { 4 } else if source.starts_with("/media/") { 3 } else { return None };
    moved_under(source, depth, roots, sample)
}

/// `moved_drive` with the mount point `depth` components down.
fn moved_under(source: &str, base: usize, roots: &[PathBuf], sample: &[(String, u64)]) -> Option<(String, String)> {
    let path = Path::new(source);
    let parts: Vec<_> = path.components().collect();
    if parts.len() < base + 1 {
        return None;
    }
    let old_root: PathBuf = parts[..=base].iter().collect();
    if old_root.exists() && path.is_dir() {
        return None;
    }
    let rest = path.strip_prefix(&old_root).ok()?;
    let old_name = old_root.file_name()?.to_string_lossy().into_owned();
    let digit_apart = |a: &str, b: &str| {
        a.len() > b.len() && a.starts_with(b) && a[b.len()..].chars().all(|c| c.is_ascii_digit())
    };
    let old = old_root.to_string_lossy().into_owned();
    roots
        .iter()
        .filter(|r| r.parent() == old_root.parent())
        .filter(|r| {
            let n = r.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            digit_apart(&old_name, &n) || digit_apart(&n, &old_name)
        })
        .find(|r| {
            let new = r.to_string_lossy();
            r.join(rest).is_dir()
                && !sample.is_empty()
                && sample.iter().all(|(p, size)| {
                    p.strip_prefix(old.as_str())
                        .map(|tail| format!("{new}{tail}"))
                        .and_then(|q| std::fs::metadata(q).ok())
                        .is_some_and(|m| m.len() == *size)
                })
        })
        .map(|r| (old.clone(), r.to_string_lossy().into_owned()))
}

fn removable(path: &str) -> bool {
    path.starts_with("/run/media/") || path.starts_with("/media/")
}

/// Store an analysis in the library: its figures, provenance and waveform.
/// (The UI stores deck loads' analyses the same way.)
///
/// The waveform sidecar is published first (temporary file in the same
/// directory, fsync, rename). Provenance and the analysis generation then
/// commit in one transaction, so a track counted at this generation has
/// both. A matching generation is not enough on its own: a missing sidecar
/// or provenance row is written again.
pub fn store(db: &Library, track: i64, a: &patch_analysis::Analysis) -> Result<(), String> {
    let generation = a.generation();
    let version = generation.to_string();
    let wave_ok = a.waveform.is_empty() || db.waveform_sidecar_present(track);
    let prov_ok =
        db.current_analysis_result(track, "track_analysis", "org.patch.analysis", &version);
    let decision_doc_ok = db
        .track_path(track)
        .and_then(|path| db.analysis_doc(&path))
        .and_then(|doc| serde_json::from_str::<serde_json::Value>(&doc).ok())
        .is_some_and(|doc| {
            doc.get("harmony").is_some()
                && doc.get("deep_regions").is_some()
                && doc
                    .get("dj_bands")
                    .and_then(serde_json::Value::as_array)
                    .is_some()
                && doc["rhythm"].get("beats").is_some()
        });
    if db.analysis_version(track) == Some(generation) && wave_ok && prov_ok && decision_doc_ok {
        return db.clear_failure(track); // already stored (a cached analysis)
    }
    let mut value = serde_json::to_value(a).map_err(|e| e.to_string())?;
    // Decision inputs must survive loss of regenerable file caches. Keep
    // them alongside the compact figures; waveform pixels remain in their
    // independently recoverable asset sidecar.
    if let Some(doc) = value.as_object_mut() {
        doc.remove("waveform");
        let bands = db
            .track_path(track)
            .and_then(|path| patch_analysis::detail_of(Path::new(&path)))
            .map(|detail| patch_ai::bands_per_second(&detail))
            .unwrap_or_default();
        doc.insert("dj_bands".into(), serde_json::json!(bands));
    }
    // A display repair can race recovery of DJ-only arrays. Keep the richer
    // same-generation document if its caller only has the compact snapshot.
    if let Some(old) = db
        .track_path(track)
        .and_then(|path| db.analysis_doc(&path))
        .and_then(|doc| serde_json::from_str::<serde_json::Value>(&doc).ok())
        && old["version"] == value["version"]
        && old["analyzers"] == value["analyzers"]
    {
        for key in ["harmony", "deep_regions", "dj_bands"] {
            if value
                .get(key)
                .is_none_or(|v| v.is_null() || v.as_array().is_some_and(|a| a.is_empty()))
                && let Some(saved) = old.get(key)
            {
                value[key] = saved.clone();
            }
        }
        let beat_count = |v: &serde_json::Value| {
            v.as_array().map_or_else(
                || {
                    v.get("offsets")
                        .and_then(serde_json::Value::as_array)
                        .map_or(0, Vec::len)
                },
                Vec::len,
            )
        };
        if beat_count(&value["rhythm"]["beats"]) == 0
            && beat_count(&old["rhythm"]["beats"]) > 0
            && value["rhythm"].is_object()
        {
            value["rhythm"]["beats"] = old["rhythm"]["beats"].clone();
        }
    }
    let provenance = patch_library::AnalysisProvenance {
        analyzer_id: "org.patch.analysis".into(),
        analyzer_version: version,
        schema_version: 1,
        created_at: timestamp_utc(),
        confidence: Some(a.bpm_confidence as f64),
        input_fingerprint: db.content_id_of(track).unwrap_or_default(),
        parameters: serde_json::json!({"sample_rate": patch_analysis::RATE}),
    };
    let summary = serde_json::json!({
        "bpm": a.bpm,
        "bpm_confidence": a.bpm_confidence,
        "key": a.key,
        "lufs": a.lufs,
        "analyzers": a.analyzers,
    });
    // Rewrite the sidecar when this generation is not the stored one, and
    // whenever the file the asset row names is missing.
    if !a.waveform.is_empty() && (!wave_ok || db.analysis_version(track) != Some(generation)) {
        let bins = a
            .waveform
            .iter()
            .map(|[peak, rms]| {
                let p = *peak as f32 / 255.0;
                [-p, p, *rms as f32 / 255.0]
            })
            .collect();
        let sidecar = patch_library::waveform::Waveform {
            sample_rate: patch_analysis::RATE,
            channels: 2,
            levels: vec![patch_library::waveform::Level { bins }],
        };
        db.store_waveform(track, &sidecar)?;
    }
    db.store_provenance_and_analysis(
        track,
        generation,
        "track_analysis",
        &provenance,
        &summary,
        &value,
    )
}

/// ISO 8601 UTC, seconds.
pub fn timestamp_utc() -> String {
    let seconds = now_s();
    let days = seconds.div_euclid(86_400);
    let rem = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    y += i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Idle CPU and I/O priority for this whole process (and ffmpeg under it),
/// and only the background decode turns.
fn go_idle() {
    patch_analysis::set_background();
    // SAFETY: plain syscalls on this process with valid arguments.
    unsafe {
        let param = libc::sched_param { sched_priority: 0 };
        libc::sched_setscheduler(0, libc::SCHED_IDLE, &param);
        libc::setpriority(libc::PRIO_PROCESS, 0, 19);
        // ioprio_set(IOPRIO_WHO_PROCESS, 0, IOPRIO_CLASS_IDLE << 13)
        libc::syscall(libc::SYS_ioprio_set, 1, 0, 3 << 13);
    }
}

/// Hold the lock for the life of the process: false if another runs.
fn take_lock() -> bool {
    use std::os::fd::IntoRawFd;
    let Ok(f) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path())
    else {
        return false;
    };
    let fd = f.into_raw_fd();
    // SAFETY: fd is open and ours (kept open on purpose until exit).
    unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

/// Whether a librarian holds the lock.
pub fn running() -> bool {
    use std::os::fd::AsRawFd;
    let Ok(f) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path())
    else {
        return false;
    };
    // SAFETY: a valid fd; a shared try-lock that we drop at once.
    let free = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) == 0 };
    !free
}

/// Start a librarian (detached) unless one runs.
pub fn ensure(exe: &Path) {
    if running() {
        return;
    }
    let _ = std::process::Command::new("setsid")
        .arg("-f")
        .arg(exe)
        .arg("librarian")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn title_of(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// The source a path belongs to (the longest matching folder).
fn source_of<'a>(sources: &'a [Source], path: &str) -> Option<&'a Source> {
    sources
        .iter()
        .filter(|s| path.starts_with(&format!("{}/", s.path)))
        .max_by_key(|s| s.path.len())
}

/// A source's own collection (made when missing).
fn own_collection(db: &Library, s: &Source) -> Option<i64> {
    db.collection_by_name(&s.name)
        .or_else(|| db.create_collection(&s.name).ok())
}

/// Each source is a collection of its own name, holding its tracks. Only
/// tracks new since the last scan join it: one taken out by hand stays out.
fn fill_collection(db: &Library, s: &Source) {
    let Some(id) = own_collection(db, s) else {
        return;
    };
    let prefix = format!("{}/", s.path);
    let have: std::collections::HashSet<i64> = db.collection_tracks(id).into_iter().collect();
    let new: Vec<i64> = db
        .tracks()
        .unwrap_or_default()
        .iter()
        .filter(|t| !t.missing && t.path.starts_with(&prefix) && !have.contains(&t.id))
        .filter(|t| s.scanned_at.is_none_or(|at| t.added_at >= at))
        .map(|t| t.id)
        .collect();
    // One transaction: 15 000 single writes held the library for minutes.
    let _ = db.add_all_to_collection(id, &new);
}

struct Librarian {
    db: Library,
    connected: HashMap<i64, bool>,
    scanned: HashMap<i64, Instant>,
    /// Each source's folders' change times, last looked at (and when):
    /// a file added, removed or renamed changes its folder's.
    folders: HashMap<i64, (u64, Instant)>,
    folder_watches: HashMap<i64, crate::folder_watch::Watch>,
    status: Status,
    written: Option<Instant>,
    recent: std::collections::VecDeque<f64>,
    build: u64,
    /// Tracks without a measured grid, and when that list was made.
    grid_todo: std::collections::VecDeque<String>,
    grid_listed: Option<Instant>,
    /// Tracks the library has analysed whose cached analysis is gone, and
    /// when that list was made.
    lost_todo: std::collections::VecDeque<(i64, String)>,
    lost_listed: Option<Instant>,
    analysis_todo: std::collections::VecDeque<(i64, String)>,
    analysis_ids: std::collections::VecDeque<i64>,
    analysis_listed: bool,
    analysis_data_version: Option<i64>,
    analysis_exhausted: Option<Instant>,
    /// Tracks whose vocals are to be measured, newest first, when that list
    /// was made, and the meter (kept while there is work: the model loads
    /// once; dropped when done, which frees the GPU).
    vocal_todo: std::collections::VecDeque<String>,
    vocal_listed: Option<Instant>,
    meter: Option<patch_stems::vocals::VocalMeter>,
    /// The database version the published counts are current for (None: this
    /// librarian wrote since, or never counted). Each count reads every
    /// track under a source (16 000 here): idle, every 3 s, it was most of
    /// the librarian's work.
    counts_clean: Option<i64>,
    /// The status last written, without its time stamp.
    last_body: String,
}

impl Librarian {
    fn publish(&mut self, force: bool) {
        self.status.years_left = YEARS_LEFT.load(std::sync::atomic::Ordering::Relaxed);
        if !force
            && self
                .written
                .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        let sources = self.db.sources();
        let known: Vec<&str> = sources.iter().map(|s| s.path.as_str()).collect();
        // Nothing written since the last count, by anyone: the counts stand.
        let version = self.db.data_version().ok();
        let counted: HashMap<String, Counts> = if version.is_some() && self.counts_clean == version {
            self.status.sources.iter().map(|s| (s.path.clone(), s.counts)).collect()
        } else {
            HashMap::new()
        };
        self.status.sources = sources
            .iter()
            .map(|s| SourceStatus {
                name: s.name.clone(),
                path: s.path.clone(),
                connected: Path::new(&s.path).is_dir(),
                removable: removable(&s.path),
                counts: counted
                    .get(&s.path)
                    .copied()
                    .unwrap_or_else(|| self.db.counts_under(&s.path, patch_analysis::GENERATION)),
            })
            .collect();
        self.counts_clean = version;
        self.status.offers = drive_roots()
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|p| {
                !known.iter().any(|k| {
                    k == p || k.starts_with(&format!("{p}/")) || p.starts_with(&format!("{k}/"))
                })
            })
            .map(|p| Offer {
                label: Path::new(&p)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                path: p,
            })
            .collect();
        self.status.per_track_s = if self.recent.is_empty() {
            0.0
        } else {
            self.recent.iter().sum::<f64>() / self.recent.len() as f64
        };
        self.status.pid = std::process::id();
        // Unchanged: written again only to stay fresh (readers take a status
        // older than 60 s as no librarian).
        self.status.updated = 0;
        let body = serde_json::to_string(&self.status).unwrap_or_default();
        if body == self.last_body
            && self
                .written
                .is_some_and(|t| t.elapsed() < Duration::from_secs(20))
        {
            return;
        }
        self.status.updated = now_s();
        let tmp = status_path().with_extension("json.tmp");
        if std::fs::write(
            &tmp,
            serde_json::to_string(&self.status).unwrap_or_default(),
        )
        .is_ok()
        {
            let _ = std::fs::rename(tmp, status_path());
        }
        self.last_body = body;
        self.written = Some(Instant::now());
    }

    /// Scan what needs it: new sources, drives just plugged in, a rescan
    /// asked for, and every source now and then.
    fn scan_due(&mut self) {
        let asked = std::fs::remove_file(rescan_flag()).is_ok();
        for s in self.db.sources() {
            let here = Path::new(&s.path).is_dir();
            let was = self.connected.insert(s.id, here);
            if was != Some(here) {
                self.analysis_todo.clear();
                self.analysis_ids.clear();
                self.analysis_listed = false;
                self.analysis_exhausted = None;
            }
            if !here {
                self.folder_watches.remove(&s.id);
                // The same drive under another mount point: its tracks
                // follow, then the next pass scans it as plugged in.
                if removable(&s.path) {
                    let depth = if s.path.starts_with("/run/media/") { 5 } else { 4 };
                    let root = Path::new(&s.path).components().take(depth).collect::<PathBuf>();
                    let sample = self.db.sample_under(&root.to_string_lossy(), 8);
                    if let Some((from, to)) = moved_drive(&s.path, &drive_roots(), &sample)
                        && let Ok(n) = self.db.relocate(&from, &to)
                    {
                        eprintln!("librarian: {from} is mounted at {to} now: {n} tracks follow");
                        self.connected.remove(&s.id);
                        self.counts_clean = None;
                    }
                }
                continue;
            }
            // Every 30 s: have its folders changed (a file added or
            // renamed)? Then at once, not at the next quarter hour.
            if !removable(&s.path)
                && !self.folder_watches.contains_key(&s.id)
                && was != Some(true)
                && let Some(watch) = crate::folder_watch::Watch::new(Path::new(&s.path))
            {
                self.folder_watches.insert(s.id, watch);
            }
            let changed = if let Some(watch) = self.folder_watches.get(&s.id) {
                watch.changed()
            } else {
                match self.folders.get(&s.id) {
                    Some((_, at)) if at.elapsed() < FOLDERS => false,
                    seen => {
                        let now = folders_signature(Path::new(&s.path));
                        let moved = seen.is_some_and(|(was, _)| *was != now);
                        self.folders.insert(s.id, (now, Instant::now()));
                        moved
                    }
                }
            };
            let due = asked
                || changed
                || was != Some(true)
                || self.scanned.get(&s.id).is_none_or(|t| t.elapsed() > RESCAN);
            if !due {
                continue;
            }
            self.analysis_todo.clear();
            self.analysis_ids.clear();
            self.analysis_listed = false;
            self.analysis_exhausted = None;
            self.counts_clean = None;
            // Rebuild before scanning: new directories are watched while
            // enumerated, and changes during the scan trigger reconciliation.
            if let std::collections::hash_map::Entry::Occupied(mut entry) =
                self.folder_watches.entry(s.id)
            {
                if let Some(watch) = crate::folder_watch::Watch::new(Path::new(&s.path)) {
                    entry.insert(watch);
                } else {
                    entry.remove();
                }
            }
            self.status.scanning = Some(s.name.clone());
            self.publish(true);
            // The source's collection fills while it is scanned: a new drive
            // or folder shows its tracks as they come in, not minutes later.
            let own = own_collection(&self.db, &s);
            let first = s.scanned_at.is_none();
            let mut batch = Vec::new();
            let _ = self.db.scan_with(
                Path::new(&s.path),
                patch_library::probe_audio_facts,
                |id, change| {
                    if first || change == patch_library::Change::Added {
                        batch.push(id);
                    }
                    if batch.len() >= 200 {
                        if let Some(c) = own {
                            let _ = self.db.add_all_to_collection(c, &batch);
                        }
                        batch.clear();
                    }
                },
            );
            fill_collection(&self.db, &s);
            let _ = self.db.set_scanned(s.id);
            self.scanned.insert(s.id, Instant::now());
            // The scan's own writes (published counts came before them).
            self.counts_clean = None;
            self.status.scanning = None;
            if self.folder_watches.get(&s.id).is_some_and(|w| w.changed()) {
                self.scanned.remove(&s.id);
            }
        }
    }

    /// Analyse the next track whose file is there: true if one was.
    fn analyse_next(&mut self) -> bool {
        if self
            .analysis_exhausted
            .is_some_and(|at| at.elapsed() < Duration::from_secs(3))
        {
            return false;
        }
        if self.analysis_exhausted.take().is_some() {
            self.analysis_listed = false;
        }
        if let Ok(version) = self.db.data_version() {
            if self.analysis_data_version.is_some_and(|old| old != version) {
                self.analysis_todo.clear();
                self.analysis_ids.clear();
                self.analysis_listed = false;
            }
            self.analysis_data_version = Some(version);
        }
        let sources = self.db.sources();
        if !self.analysis_listed {
            // Availability and exact counts once per pass. Keep only IDs;
            // paths are read/revalidated in bounded primary-key batches.
            self.analysis_ids = self
                .db
                .queue(patch_analysis::GENERATION)
                .unwrap_or_default()
                .into_iter()
                .filter(|(_, p)| {
                    // Hidden files ("._song.mp3", macOS metadata twins that
                    // older scans imported) are not music.
                    !Path::new(p)
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
                        && source_of(&sources, p)
                            .is_none_or(|s| self.connected.get(&s.id) == Some(&true))
                        && Path::new(p).is_file()
                })
                .map(|(id, _)| id)
                .collect();
            self.status.queue = self.analysis_ids.len();
            self.analysis_listed = true;
        }
        // Several tracks at once: analysis is CPU-bound and independent per
        // track. Bounded, so decoded audio (~100 MB per track) stays modest
        // and the machine keeps headroom; the audio thread runs real-time
        // and is never contended.
        let width = std::thread::available_parallelism()
            .map_or(1, |n| n.get() / 2)
            .clamp(1, 8);
        let mut jobs = Vec::with_capacity(width);
        while jobs.len() < width {
            let Some(job) = self.next_pending(&sources) else {
                break;
            };
            jobs.push(job);
        }
        if jobs.is_empty() {
            return false;
        }
        self.status.analysing = Some(title_of(&jobs[0].1));
        self.status.analysing_source = source_of(&sources, &jobs[0].1).map(|s| s.name.clone());
        self.publish(false);
        let t0 = Instant::now();
        let results: Vec<_> = std::thread::scope(|scope| {
            let running: Vec<_> = jobs
                .iter()
                .map(|(_, path)| scope.spawn(move || patch_analysis::analyse_file(Path::new(path))))
                .collect();
            running
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|_| Err("analysis panicked".into()))
                })
                .collect()
        });
        let per_track = t0.elapsed().as_secs_f64() / jobs.len() as f64;
        for ((id, _), result) in jobs.into_iter().zip(results) {
            match result {
                Ok(a) => {
                    // Not saved: shown as this track's failure (with why),
                    // and it is analysed again when asked.
                    if let Err(e) = store(&self.db, id, &a) {
                        eprintln!("librarian: analysis of track {id} not saved: {e}");
                        let _ = self.db.set_failure(
                            id,
                            patch_analysis::GENERATION,
                            &format!("not saved: {e}"),
                        );
                    }
                    if self.recent.len() == 20 {
                        self.recent.pop_front();
                    }
                    self.recent.push_back(per_track);
                }
                Err(e) => {
                    let _ = self.db.set_failure(id, patch_analysis::GENERATION, &e);
                }
            }
            self.status.queue = self.status.queue.saturating_sub(1);
        }
        true
    }

    /// The next queued track whose file is there and still needs analysis.
    fn next_pending(&mut self, sources: &[Source]) -> Option<(i64, String)> {
        loop {
            if self.analysis_todo.is_empty() {
                if self.analysis_ids.is_empty() {
                    self.analysis_exhausted = Some(Instant::now());
                    self.status.analysing = None;
                    self.status.analysing_source = None;
                    return None;
                }
                let ids: Vec<_> = self
                    .analysis_ids
                    .drain(..self.analysis_ids.len().min(64))
                    .collect();
                let batch = self
                    .db
                    .analysis_batch(patch_analysis::GENERATION, &ids)
                    .unwrap_or_default();
                self.status.queue = self.status.queue.saturating_sub(ids.len() - batch.len());
                self.analysis_todo.extend(batch);
            }
            let Some((id, path)) = self.analysis_todo.pop_front() else {
                continue;
            };
            if source_of(sources, &path).is_some_and(|s| self.connected.get(&s.id) != Some(&true))
                || !Path::new(&path).is_file()
            {
                continue;
            }
            // Another frontend may have completed this queued item meanwhile.
            if !self
                .db
                .analysis_pending(patch_analysis::GENERATION, id, &path)
                .unwrap_or(false)
            {
                self.status.queue = self.status.queue.saturating_sub(1);
                continue;
            }
            return Some((id, path));
        }
    }
}

impl Librarian {
    /// With nothing new to analyse: analyse again the tracks the library
    /// counts as analysed whose cached analysis is gone. The library keeps
    /// only a summary; the AI DJ and the decks read the cache, so without
    /// it a track is invisible to them (2026-10-12: 2,184 of 2,307 tracks,
    /// and a set of 100 had 5 to choose from). True if one was analysed.
    fn restore_next(&mut self) -> bool {
        if self.lost_todo.is_empty()
            && self
                .lost_listed
                .is_none_or(|t| t.elapsed() > Duration::from_secs(600))
        {
            self.lost_listed = Some(Instant::now());
            let tracks = self.db.tracks().unwrap_or_default();
            self.lost_todo = tracks
                .into_iter()
                .filter(|t| !t.missing && Path::new(&t.path).is_file())
                .filter(|t| {
                    self.db.analysis_version(t.id) == Some(patch_analysis::GENERATION)
                        && patch_analysis::cache_needs_analysis(Path::new(&t.path))
                })
                .map(|t| (t.id, t.path))
                .collect();
            self.status.queue += self.lost_todo.len();
        }
        let width = std::thread::available_parallelism()
            .map_or(1, |n| n.get() / 2)
            .clamp(1, 8);
        let n = self.lost_todo.len().min(width);
        if n == 0 {
            return false;
        }
        let jobs: Vec<_> = self.lost_todo.drain(..n).collect();
        self.status.analysing = Some(title_of(&jobs[0].1));
        self.publish(false);
        let results: Vec<_> = std::thread::scope(|scope| {
            let running: Vec<_> = jobs
                .iter()
                .map(|(_, path)| scope.spawn(move || patch_analysis::analyse_file(Path::new(path))))
                .collect();
            running
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|_| Err("analysis panicked".into())))
                .collect()
        });
        for ((id, _), result) in jobs.into_iter().zip(results) {
            if let Err(e) = result {
                let _ = self.db.set_failure(id, patch_analysis::GENERATION, &e);
            }
            self.status.queue = self.status.queue.saturating_sub(1);
        }
        if self.lost_todo.is_empty() {
            self.status.analysing = None;
        }
        true
    }

    /// With nothing to analyse: measure the beat grid of the next track
    /// that has none yet (analysed before grids existed). A wrong tempo
    /// ruins every transition, so every track gets one; about half a
    /// second each, at the lowest priority. True if one was measured.
    fn measure_next_grid(&mut self) -> bool {
        if self.grid_todo.is_empty()
            && self
                .grid_listed
                .is_none_or(|t| t.elapsed() > Duration::from_secs(600))
        {
            self.grid_listed = Some(Instant::now());
            let tracks = self.db.tracks().unwrap_or_default();
            self.grid_todo = tracks
                .into_iter()
                .filter(|t| !t.missing && Path::new(&t.path).is_file())
                .filter(|t| {
                    patch_analysis::cached(Path::new(&t.path)).is_some_and(|a| {
                        a.bpm.is_some()
                            && a.grid
                                .as_ref()
                                .is_none_or(|g| g.method < patch_analysis::grid::METHOD)
                    })
                })
                .map(|t| t.path)
                .collect();
        }
        self.status.grids_left = self.grid_todo.len();
        let Some(path) = self.grid_todo.pop_front() else {
            self.status.measuring = None;
            return false;
        };
        self.status.measuring = Some(title_of(&path));
        self.publish(false);
        let _ = patch_analysis::measure_grid(Path::new(&path), false);
        self.status.grids_left = self.grid_todo.len();
        if self.grid_todo.is_empty() {
            self.status.measuring = None;
        }
        true
    }
}

impl Librarian {
    /// With everything else done: measure where the next track sings (see
    /// patch_analysis::vocals), with the stem separator if it is installed.
    /// About 7 s a track on a GPU. True if one was measured.
    fn measure_next_vocals(&mut self) -> bool {
        if self.vocal_todo.is_empty()
            && self
                .vocal_listed
                .is_none_or(|t| t.elapsed() > Duration::from_secs(600))
        {
            self.vocal_listed = Some(Instant::now());
            if patch_stems::separator::configured().is_ok() {
                let mut tracks = self.db.tracks().unwrap_or_default();
                // The newest music first: what gets played next.
                tracks.sort_by_key(|t| std::cmp::Reverse(t.id));
                self.vocal_todo = tracks
                    .into_iter()
                    .filter(|t| !t.missing && Path::new(&t.path).is_file())
                    .filter(|t| patch_analysis::vocals_due(Path::new(&t.path)))
                    .map(|t| t.path)
                    .collect();
            }
        }
        self.status.vocals_left = self.vocal_todo.len();
        let Some(path) = self.vocal_todo.pop_front() else {
            self.status.voicing = None;
            self.meter = None;
            return false;
        };
        if self.meter.is_none() {
            match patch_stems::vocals::VocalMeter::start() {
                Ok(m) => self.meter = Some(m),
                Err(e) => {
                    eprintln!("librarian: no vocal meter: {e}");
                    self.vocal_todo.clear();
                    self.status.vocals_left = 0;
                    return false;
                }
            }
        }
        self.status.voicing = Some(title_of(&path));
        self.publish(false);
        let meter = self.meter.as_mut().expect("started above");
        match meter.measure(Path::new(&path)) {
            Ok(seconds) => {
                let v = patch_analysis::vocals::VocalEvidence::from_ratios(&meter.model, &seconds);
                patch_analysis::store_vocals(Path::new(&path), v);
            }
            Err(e) => {
                eprintln!("librarian: vocals of {path}: {e}");
                // A meter that stopped starts again with the next track.
                if e.starts_with("The vocal meter stopped") {
                    self.meter = None;
                }
            }
        }
        self.status.vocals_left = self.vocal_todo.len();
        true
    }
}

/// Run until the binary is replaced (the engine starts the new one).
/// Tracks waiting for a release-year lookup, from the years thread.
static YEARS_LEFT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Release years and genres, beside the analysis: each track's from its
/// tags (offline), and, once the user allowed it, what MusicBrainz knows,
/// one track at a time within its one-request-a-second rule: the original
/// release year and, with the same search, the genre where the tag names
/// none. Network waits never hold up analysis.
fn years_loop() {
    use std::sync::atomic::Ordering;
    let Ok(db) = patch_library::open_default() else { return };
    let mut tags_at: Option<Instant> = None;
    let mut list: Option<std::collections::HashSet<String>> = None;
    loop {
        if tags_at.is_none_or(|t| t.elapsed() > Duration::from_secs(600)) {
            let _ = db.years_from_tags();
            let _ = db.genres_from_tags();
            // Tempos the file's own BPM (tag, name) confirms: a soft-beat
            // track is not one to check. Each analysis once, 500 at a time
            // (the first pass over a 17,000-track library: ~3 s).
            while db.confirm_tempos(500).is_ok_and(|n| n > 0) {
                std::thread::sleep(Duration::from_millis(50));
            }
            tags_at = Some(Instant::now());
        }
        let allowed = patch_core::setup::load().online_lookups;
        if allowed && list.is_none() {
            match patch_library::genres::genre_list() {
                Ok(l) => list = Some(l),
                Err(_) => {
                    std::thread::sleep(Duration::from_secs(120));
                    continue;
                }
            }
        }
        let years = if allowed { db.years_to_look_up(20) } else { vec![] };
        // The years first (their search brings the genre along), then the
        // genres of tracks whose years were looked up before.
        let (todo, years_too) = if years.is_empty() && allowed {
            (db.genres_to_look_up(20), false)
        } else {
            (years, true)
        };
        YEARS_LEFT.store(
            if allowed { db.years_progress().1 + db.genres_left() } else { 0 },
            Ordering::Relaxed,
        );
        if todo.is_empty() {
            std::thread::sleep(Duration::from_secs(30));
            continue;
        }
        let empty = std::collections::HashSet::new();
        let list = list.as_ref().unwrap_or(&empty);
        for (id, artist, title) in todo {
            if !patch_core::setup::load().online_lookups {
                break;
            }
            let found = if years_too {
                patch_library::years::musicbrainz(&artist, &title)
            } else {
                patch_library::years::musicbrainz_tags(&artist, &title)
            };
            let done = found.and_then(|found| {
                if years_too {
                    db.set_years_found(id, &found, now_s())?;
                }
                db.set_genre_found(id, &found, list, now_s())
            });
            match done {
                Ok(()) => {
                    let _ = YEARS_LEFT.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1));
                }
                // Offline or refused: try again later, not track by track.
                Err(_) => {
                    std::thread::sleep(Duration::from_secs(120));
                    break;
                }
            }
        }
    }
}

pub fn run() -> Result<(), String> {
    let _ = std::fs::create_dir_all(runtime());
    if !take_lock() {
        return Ok(()); // one is running
    }
    go_idle();
    // Old analyses nothing can use: gone (older versions, orphan waveforms).
    let _ = patch_analysis::prune_cache();
    let db = patch_library::open_default()?;
    // The first run: the music folder is the first source. (Not once the
    // library has tracks: the last folder taken out stays out.)
    if db.sources().is_empty() && db.tracks().is_ok_and(|t| t.is_empty()) {
        let music = music_dir();
        if music.is_dir() {
            let _ = db.add_source(&music, "Music");
        }
    }
    let mut l = Librarian {
        db,
        connected: HashMap::new(),
        scanned: HashMap::new(),
        folders: HashMap::new(),
        folder_watches: HashMap::new(),
        status: Status::default(),
        written: None,
        recent: Default::default(),
        build: crate::client::build_stamp(),
        grid_todo: Default::default(),
        grid_listed: None,
        lost_todo: Default::default(),
        lost_listed: None,
        analysis_todo: Default::default(),
        analysis_ids: Default::default(),
        analysis_listed: false,
        analysis_data_version: None,
        analysis_exhausted: None,
        vocal_todo: Default::default(),
        vocal_listed: None,
        meter: None,
        counts_clean: None,
        last_body: String::new(),
    };
    // The path as started: once the file is replaced, /proc/self/exe
    // says "(deleted)" and can't be read for the new one's time.
    let exe = std::env::current_exe().ok();
    let stamp = || {
        exe.as_ref()
            .and_then(|e| std::fs::metadata(e).ok())
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
    };
    let _ = std::thread::Builder::new()
        .name("patch-years".into())
        .spawn(years_loop);
    let mut setup = patch_core::setup::Watched::default();
    loop {
        // The DJ Booth switched off: the library isn't needed.
        if !setup.get().dj {
            return Ok(());
        }
        // A rebuilt Patch: leave; the engine starts the new librarian.
        if stamp().is_some_and(|s| s != l.build) {
            return Ok(());
        }
        l.scan_due();
        if !l.analyse_next()
            && !l.restore_next()
            && !l.measure_next_grid()
            && !l.measure_next_vocals()
        {
            l.publish(true);
            std::thread::sleep(Duration::from_secs(3));
        } else {
            // It wrote (its own writes don't move the database version).
            l.counts_clean = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_track_counts_as_stored_only_once_every_part_is_saved() {
        let dir =
            std::env::temp_dir().join(format!("patch-librarian-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Library::open(&dir.join("library.db")).unwrap();
        let file = dir.join("t.flac");
        std::fs::write(&file, vec![7u8; 4096]).unwrap();
        let (track, _) = db.import(&file, |_| None).unwrap();
        // Two seconds of clicks: enough for figures and a waveform.
        let pcm: Vec<f32> = (0..patch_analysis::RATE as usize * 4)
            .map(|i| if i % 22_050 < 200 { 0.5 } else { 0.0 })
            .collect();
        let a = patch_analysis::analyse(&pcm);
        assert!(!a.waveform.is_empty());
        // A file where the waveform folder goes: the sidecar cannot be saved.
        let waveforms = dir.join("analysis/waveforms");
        std::fs::create_dir_all(waveforms.parent().unwrap()).unwrap();
        std::fs::write(&waveforms, b"in the way").unwrap();
        assert!(store(&db, track, &a).is_err());
        assert_eq!(db.analysis_version(track), None, "not counted as stored");
        std::fs::remove_file(&waveforms).unwrap();
        store(&db, track, &a).unwrap();
        assert_eq!(db.analysis_version(track), Some(a.generation()));
        assert_eq!(
            db.waveform_bins(file.to_str().unwrap()).unwrap().len(),
            a.waveform.len(),
            "the library sidecar can restore the overview waveform"
        );
        let saved: serde_json::Value = serde_json::from_str(
            &db.analysis_doc(file.to_str().unwrap()).expect("saved analysis document"),
        )
        .unwrap();
        assert!(saved.get("harmony").is_some(), "planner harmony is durable");
        assert!(saved.get("deep_regions").is_some(), "planner regions are durable");
        assert!(saved["rhythm"].get("beats").is_some(), "planner beats are durable");
        assert!(saved["dj_bands"].is_array(), "planner bands are durable");
        assert!(db.waveform_sidecar_present(track));
        assert!(db.current_analysis_result(
            track,
            "track_analysis",
            "org.patch.analysis",
            &a.generation().to_string()
        ));
        let sidecar = std::fs::read_dir(dir.join("analysis/waveforms"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|x| x == "pwf"))
            .expect("waveform sidecar");
        patch_library::waveform::Waveform::decode(&std::fs::read(&sidecar).unwrap()).unwrap();
        assert!(
            std::fs::read_dir(sidecar.parent().unwrap())
                .unwrap()
                .flatten()
                .all(|e| e.path().extension().is_some_and(|x| x == "pwf")),
            "no temporary part file left beside the sidecar"
        );
        std::fs::remove_file(&sidecar).unwrap();
        assert!(!db.waveform_sidecar_present(track));
        store(&db, track, &a).unwrap();
        assert!(
            db.waveform_sidecar_present(track),
            "a missing sidecar is restored"
        );
        assert_eq!(db.analysis_version(track), Some(a.generation()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod moved_drive_tests {
    use super::*;

    #[test]
    fn a_drive_one_digit_apart_with_the_same_files_is_the_same_drive() {
        let media = std::env::temp_dir().join(format!("patch-media-{}", std::process::id()));
        let depth = media.components().count();
        let now = media.join("SAMSUNG_T5");
        std::fs::create_dir_all(now.join("NL Top 40/2010")).unwrap();
        std::fs::write(now.join("NL Top 40/2010/a.mp3"), vec![0u8; 50]).unwrap();
        std::fs::create_dir_all(media.join("OTHER_DRIVE/NL Top 40")).unwrap();
        let old = media.join("SAMSUNG_T52").display().to_string();
        let source = format!("{old}/NL Top 40");
        let track = format!("{old}/NL Top 40/2010/a.mp3");
        let roots = vec![media.join("OTHER_DRIVE"), now.clone()];
        let found = moved_under(&source, depth, &roots, &[(track.clone(), 50)]);
        assert_eq!(found, Some((old.clone(), now.display().to_string())));
        // A different size: not the same files, so nothing moves.
        assert_eq!(moved_under(&source, depth, &roots, &[(track.clone(), 51)]), None);
        // Nothing sampled: never on the folder name alone.
        assert_eq!(moved_under(&source, depth, &roots, &[]), None);
        // And back: T5 gone, the drive at T52.
        let back = format!("{}/NL Top 40", now.display());
        let t52 = media.join("SAMSUNG_T52");
        std::fs::rename(&now, &t52).unwrap();
        let track5 = format!("{}/NL Top 40/2010/a.mp3", now.display());
        assert_eq!(
            moved_under(&back, depth, std::slice::from_ref(&t52), &[(track5, 50)]),
            Some((now.display().to_string(), t52.display().to_string()))
        );
        assert_eq!(moved_drive("/home/u/Music", &roots, &[]), None, "only drives");
        let _ = std::fs::remove_dir_all(&media);
    }
}
