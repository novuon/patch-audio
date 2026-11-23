//! What the library knows about a track's analysis, for the decks, the
//! library windows and the DJ: the content cache or the library's own
//! analysis document, with a saved human grid applied. Control plane only.

use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

type ManualGridCatalogue = (
    Instant,
    std::collections::HashMap<String, patch_library::HumanGridOverride>,
);
static MANUAL_GRIDS: LazyLock<Mutex<Option<ManualGridCatalogue>>> =
    LazyLock::new(|| Mutex::new(None));

/// The library's human grids; None when it could not be read just now.
fn read_manual_grids() -> Option<std::collections::HashMap<String, patch_library::HumanGridOverride>>
{
    if !patch_library::default_path().is_file() {
        return Some(Default::default());
    }
    patch_library::open_quick()
        .and_then(|db| db.human_grid_overrides())
        .ok()
}

/// A manual editor has committed new authoritative values to the library.
pub fn invalidate_manual_grids() {
    *MANUAL_GRIDS.lock().unwrap() = None;
}

/// Apply a saved human grid to a runtime analysis copy. The analyzer cache is
/// left untouched; the library remains the authoritative source of overrides.
pub fn apply_saved_manual_grid(path: &str, analysis: &mut patch_analysis::Analysis) {
    // The first read waits for the library; later ones use what was read
    // while a background thread reads it again (every 2 s at most): the UI
    // thread asks here, and opening the library there cost it frames.
    static REFRESHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let mut catalogue = MANUAL_GRIDS.lock().unwrap();
    match catalogue.as_ref() {
        None => *catalogue = Some((Instant::now(), read_manual_grids().unwrap_or_default())),
        Some((loaded, _))
            if loaded.elapsed() >= Duration::from_secs(2)
                && !REFRESHING.swap(true, std::sync::atomic::Ordering::AcqRel) =>
        {
            let spawned = std::thread::Builder::new()
                .name("patch-manual-grids".into())
                .spawn(|| {
                    let grids = read_manual_grids();
                    let mut catalogue = MANUAL_GRIDS.lock().unwrap();
                    match (grids, catalogue.as_mut()) {
                        (Some(grids), _) => *catalogue = Some((Instant::now(), grids)),
                        // A short library lock must not make a known human
                        // override disappear from active DJ decisions.
                        (None, Some((at, _))) => *at = Instant::now(),
                        (None, None) => {}
                    }
                    REFRESHING.store(false, std::sync::atomic::Ordering::Release);
                });
            if spawned.is_err() {
                REFRESHING.store(false, std::sync::atomic::Ordering::Release);
            }
        }
        Some(_) => {}
    }
    analysis.human_grid = catalogue
        .as_ref()
        .and_then(|(_, grids)| grids.get(path))
        .map(|grid| patch_analysis::HumanGridOverride {
            bpm: grid.bpm,
            offset: grid.first_beat_s,
            beats_per_bar: grid.beats_per_bar,
            downbeat_phase: grid.downbeat_phase,
            segments: grid
                .segments
                .iter()
                .map(|segment| {
                    let rate = grid.sample_rate.max(1) as f64;
                    patch_analysis::HumanTempoSegment {
                        start_s: segment.start_sample as f64 / rate,
                        end_s: segment.end_sample.map(|sample| sample as f64 / rate),
                        bpm_start: segment.bpm_start,
                        bpm_end: segment.bpm_end,
                    }
                })
                .collect(),
        });
    analysis.use_grid();
}

/// Current compact analysis from the content cache or, when its regenerable
/// sidecar is absent, from the library's authoritative analysis document.
pub fn available_analysis(path: &str) -> Option<patch_analysis::Analysis> {
    let mut analysis = stored_analysis(path)?;
    apply_saved_manual_grid(path, &mut analysis);
    Some(analysis)
}

/// Machine facts before runtime human grid overrides. Asset recovery must
/// never serialize a runtime override into the machine-analysis cache.
pub fn stored_analysis(path: &str) -> Option<patch_analysis::Analysis> {
    patch_analysis::cached(std::path::Path::new(path)).or_else(|| library_analysis(path))
}

pub(crate) type LibraryDecisionData = (patch_analysis::Analysis, Vec<[u8; 3]>);

fn library_analysis(path: &str) -> Option<patch_analysis::Analysis> {
    Some(library_data(path)?.0.clone())
}

pub(crate) fn library_data(path: &str) -> Option<std::sync::Arc<LibraryDecisionData>> {
    use std::sync::Arc;
    struct Reader {
        db: patch_library::Library,
        version: i64,
        cache: patch_analysis::cache::Cache<String, Arc<LibraryDecisionData>>,
    }
    const BUDGET: usize = 32 * 1024 * 1024;
    static READER: LazyLock<Mutex<Option<Reader>>> = LazyLock::new(|| Mutex::new(None));
    let mut reader = READER.lock().unwrap();
    if reader.is_none() {
        let db = patch_library::open_quick().ok()?;
        let version = db.data_version().ok()?;
        *reader = Some(Reader {
            db,
            version,
            cache: patch_analysis::cache::Cache::new(BUDGET),
        });
    }
    let reader = reader.as_mut()?;
    let version = reader.db.data_version().ok()?;
    if version != reader.version {
        reader.version = version;
        reader.cache = patch_analysis::cache::Cache::new(BUDGET);
    }
    let key = path.to_string();
    if let Some(data) = reader.cache.get(&key) {
        return Some(data.clone());
    }
    let doc = reader.db.analysis_doc(path)?;
    let data = Arc::new(decision_from_doc(&doc)?);
    let bytes = data.0.heap_bytes() + data.1.capacity() * 3 + path.len() + 256;
    reader.cache.insert(key, data.clone(), bytes);
    Some(data)
}

pub(crate) fn decision_from_doc(doc: &str) -> Option<LibraryDecisionData> {
    let mut doc: serde_json::Value = serde_json::from_str(doc).ok()?;
    let object = doc.as_object_mut()?;
    let bands = object
        .remove("dj_bands")
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    object
        .entry("waveform")
        .or_insert_with(|| serde_json::json!([]));
    if let Some(rhythm) = object
        .get_mut("rhythm")
        .and_then(serde_json::Value::as_object_mut)
    {
        rhythm
            .entry("beats")
            .or_insert_with(|| serde_json::json!([]));
    }
    let analysis: patch_analysis::Analysis = serde_json::from_value(doc).ok()?;
    (analysis.version == patch_analysis::VERSION
        && analysis.analyzers.beat == patch_analysis::AnalyzerVersions::CURRENT.beat
        && analysis.analyzers.key == patch_analysis::AnalyzerVersions::CURRENT.key
        && analysis.analyzers.levels == patch_analysis::AnalyzerVersions::CURRENT.levels)
        .then_some((analysis, bands))
}

/// The analysed library as (path, title, artist, rating).
pub fn library() -> Vec<(String, String, String, u8)> {
    // Busy for a moment (the librarian writing): once more, shortly after.
    let read = || patch_library::open_quick().and_then(|db| db.dj_pool());
    let tracks = read().or_else(|_| {
        std::thread::sleep(std::time::Duration::from_millis(300));
        read()
    });
    tracks.unwrap_or_default()
}
