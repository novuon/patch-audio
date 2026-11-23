//! Background preflight for source-referenced performance graphs.
//!
//! This binds stable track identities to current files and verifies the
//! content hashes before a schedule can be handed to playback workers. Call
//! only from a control/preparation worker; this performs filesystem I/O.

use patch_core::performance::{PerformanceGraph, PreparedPerformanceSchedule};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct BoundSource {
    pub id: String,
    pub path: PathBuf,
    pub content_hash: String,
    pub duration_s: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoundStem {
    pub source_id: String,
    pub stem_set_id: String,
    pub stem_id: String,
    pub path: PathBuf,
    pub checksum: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PerformanceLaneAssignment {
    pub state_id: String,
    pub source_id: String,
    pub layer_id: String,
    /// Native engine deck index: 0=A, 1=B, 2=C, 3=D.
    pub lane: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EntryLane {
    pub lane: usize,
    pub source_id: String,
    pub path: PathBuf,
    pub source_start_s: f64,
    pub source_end_s: f64,
    /// Engine tempo offset needed to synchronize this source to the graph.
    pub tempo_adjustment: f64,
    pub original_gain: f32,
    pub starts_at_entry: bool,
    /// This source is already playing and remains on its existing transport.
    pub live: bool,
    /// Offset from the shared performance start sample.
    pub start_offset_samples: u64,
    /// Only selected stems are audible; the worker advances the aligned set
    /// with the same transport as the source track.
    pub stem_gains: BTreeMap<String, f32>,
    pub stem_set_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScheduledLayerAutomation {
    pub lane: usize,
    pub source_id: String,
    /// None targets the source track's original mix; Some targets a prepared stem.
    pub stem_id: Option<String>,
    pub at_sample: u64,
    pub duration_samples: u64,
    pub to_gain: f32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledDeckPause {
    pub lane: usize,
    pub source_id: String,
    pub at_sample: u64,
}

pub struct PreparedPerformance {
    pub schedule: PreparedPerformanceSchedule,
    pub sources: Vec<BoundSource>,
    pub stems: Vec<BoundStem>,
    pub lane_assignments: Vec<PerformanceLaneAssignment>,
    /// Shared read leases keep selected cache entries resident until player
    /// workers have adopted their own leases.
    _stem_leases: Vec<patch_stems::cache::Lease>,
}

impl std::fmt::Debug for PreparedPerformance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedPerformance")
            .field("schedule", &self.schedule)
            .field("sources", &self.sources)
            .field("stems", &self.stems)
            .field("lane_assignments", &self.lane_assignments)
            .field("stem_lease_count", &self._stem_leases.len())
            .finish()
    }
}

/// Verify each source file against the graph's content identity, then
/// compile its selected arrangement path to the engine's sample clock.
/// `source_paths` comes from the library index and must contain every source.
pub fn prepare(
    graph: &PerformanceGraph,
    source_paths: &BTreeMap<String, PathBuf>,
    sample_rate: u32,
) -> Result<PreparedPerformance, String> {
    prepare_with_lane_hints(graph, source_paths, sample_rate, &BTreeMap::new())
}

/// Prepare while retaining designated engine lanes for sources that already
/// occupy live decks (for example, the playing and incoming tracks).
pub fn prepare_with_lane_hints(
    graph: &PerformanceGraph,
    source_paths: &BTreeMap<String, PathBuf>,
    sample_rate: u32,
    preferred_lanes: &BTreeMap<String, usize>,
) -> Result<PreparedPerformance, String> {
    prepare_with_probe_and_lanes(
        graph,
        source_paths,
        sample_rate,
        preferred_lanes,
        crate::deck::pcm::probe,
    )
}

#[cfg(test)]
fn prepare_with_probe(
    graph: &PerformanceGraph,
    source_paths: &BTreeMap<String, PathBuf>,
    sample_rate: u32,
    probe: impl FnMut(&Path) -> Result<crate::deck::pcm::SourceInfo, String>,
) -> Result<PreparedPerformance, String> {
    prepare_with_probe_and_lanes(graph, source_paths, sample_rate, &BTreeMap::new(), probe)
}

fn prepare_with_probe_and_lanes(
    graph: &PerformanceGraph,
    source_paths: &BTreeMap<String, PathBuf>,
    sample_rate: u32,
    preferred_lanes: &BTreeMap<String, usize>,
    probe: impl FnMut(&Path) -> Result<crate::deck::pcm::SourceInfo, String>,
) -> Result<PreparedPerformance, String> {
    prepare_with_probe_cache(
        graph,
        source_paths,
        sample_rate,
        preferred_lanes,
        None,
        probe,
    )
}

#[cfg(test)]
fn prepare_with_probe_and_cache(
    graph: &PerformanceGraph,
    source_paths: &BTreeMap<String, PathBuf>,
    sample_rate: u32,
    cache: &patch_stems::cache::Cache,
    probe: impl FnMut(&Path) -> Result<crate::deck::pcm::SourceInfo, String>,
) -> Result<PreparedPerformance, String> {
    prepare_with_probe_cache(
        graph,
        source_paths,
        sample_rate,
        &BTreeMap::new(),
        Some(cache),
        probe,
    )
}

fn prepare_with_probe_cache(
    graph: &PerformanceGraph,
    source_paths: &BTreeMap<String, PathBuf>,
    sample_rate: u32,
    preferred_lanes: &BTreeMap<String, usize>,
    cache_override: Option<&patch_stems::cache::Cache>,
    mut probe: impl FnMut(&Path) -> Result<crate::deck::pcm::SourceInfo, String>,
) -> Result<PreparedPerformance, String> {
    graph.validate_prepared()?;
    if source_paths.len() != graph.sources.len() {
        return Err("source path set does not exactly match the performance".into());
    }
    let mut resolved = Vec::with_capacity(graph.sources.len());
    let mut path_set = BTreeSet::new();
    for source in &graph.sources {
        let path = source_paths
            .get(&source.id)
            .ok_or_else(|| format!("no current library path for source {}", source.id))?;
        let canonical = std::fs::canonicalize(path)
            .map_err(|e| format!("source {} is unavailable: {e}", source.id))?;
        if !canonical.is_file() || !path_set.insert(canonical.clone()) {
            return Err(format!("source {} is not a unique regular file", source.id));
        }
        let expected = source
            .content_hash
            .as_deref()
            .ok_or_else(|| format!("source {} has no content hash", source.id))?;
        let actual = patch_stems::hash_file(Path::new(&canonical))?;
        if actual != expected {
            return Err(format!("source {} changed since planning", source.id));
        }
        let info = probe(&canonical)
            .map_err(|e| format!("source {} cannot be decoded: {e}", source.id))?;
        if info.rate == 0
            || info.channels == 0
            || !info.duration.is_finite()
            || info.duration <= 0.0
        {
            return Err(format!("source {} has invalid audio metadata", source.id));
        }
        let required_end = graph
            .states
            .iter()
            .flat_map(|state| &state.layers)
            .filter(|layer| layer.source_id == source.id)
            .map(|layer| layer.source_end_s)
            .fold(0.0f64, f64::max);
        if required_end > info.duration + 0.05 {
            return Err(format!(
                "source {} is shorter than its planned regions",
                source.id
            ));
        }
        resolved.push(BoundSource {
            id: source.id.clone(),
            path: canonical,
            content_hash: actual,
            duration_s: info.duration,
        });
    }
    let schedule = graph.compile(sample_rate)?;
    let lane_assignments = allocate_lanes(graph, preferred_lanes)?;
    let has_stems = graph
        .states
        .iter()
        .flat_map(|state| &state.layers)
        .any(|layer| {
            matches!(
                layer.material,
                patch_core::performance::MaterialKind::Stem(_)
            )
        });
    let owned_cache = (has_stems && cache_override.is_none())
        .then(patch_stems::cache::Cache::default_cache)
        .transpose()?;
    let cache = cache_override.or(owned_cache.as_ref());
    let (stems, stem_leases) = bind_stems(graph, &resolved, cache)?;
    Ok(PreparedPerformance {
        schedule,
        sources: resolved,
        stems,
        lane_assignments,
        _stem_leases: stem_leases,
    })
}

/// Project the graph entry state to one lane per canonical track source.
/// This performs no filesystem or audio work.
pub fn entry_lanes(
    graph: &PerformanceGraph,
    prepared: &PreparedPerformance,
) -> Result<Vec<EntryLane>, String> {
    graph.validate_prepared()?;
    let entry = graph
        .states
        .iter()
        .find(|state| state.id == graph.entry)
        .ok_or("performance entry state is missing")?;
    let paths: BTreeMap<_, _> = prepared
        .sources
        .iter()
        .map(|source| (source.id.as_str(), source.path.as_path()))
        .collect();
    let mut lanes: BTreeMap<&str, EntryLane> = BTreeMap::new();
    for layer in &entry.layers {
        let assignment = prepared
            .lane_assignments
            .iter()
            .find(|assignment| {
                assignment.state_id == entry.id
                    && assignment.source_id == layer.source_id
                    && assignment.layer_id == layer.id
            })
            .ok_or_else(|| format!("entry layer {} has no engine lane", layer.id))?;
        let path = paths
            .get(layer.source_id.as_str())
            .ok_or_else(|| format!("entry source {} was not preflighted", layer.source_id))?;
        let tempo_adjustment = tempo_adjustment(graph, &layer.source_id)?;
        let lane = lanes
            .entry(layer.source_id.as_str())
            .or_insert_with(|| EntryLane {
                lane: assignment.lane,
                source_id: layer.source_id.clone(),
                path: (*path).to_path_buf(),
                source_start_s: layer.source_start_s,
                source_end_s: layer.source_end_s,
                tempo_adjustment,
                original_gain: 1.0,
                starts_at_entry: true,
                live: false,
                start_offset_samples: 0,
                stem_gains: BTreeMap::new(),
                stem_set_id: None,
            });
        if lane.lane != assignment.lane
            || lane.source_start_s != layer.source_start_s
            || lane.source_end_s != layer.source_end_s
        {
            return Err(format!(
                "entry source {} has inconsistent lane or transport bounds",
                layer.source_id
            ));
        }
        if let patch_core::performance::MaterialKind::Stem(stem) = &layer.material {
            let set_id = layer
                .stem_set_id
                .as_deref()
                .ok_or("stem layer has no StemSet")?;
            if lane
                .stem_set_id
                .as_deref()
                .is_some_and(|existing| existing != set_id)
            {
                return Err(format!(
                    "entry source {} uses multiple StemSets",
                    layer.source_id
                ));
            }
            if lane.stem_gains.insert(stem.clone(), layer.gain).is_some() {
                return Err(format!(
                    "entry source {} repeats stem {stem}",
                    layer.source_id
                ));
            }
            lane.stem_set_id = Some(set_id.to_owned());
        } else {
            lane.original_gain = layer.gain;
        }
    }
    let mut result: Vec<_> = lanes.into_values().collect();
    result.sort_by_key(|lane| lane.lane);
    Ok(result)
}

/// Project a short, fully prepared arrangement onto stable engine decks.
/// Entry sources retain their normal A/B assignments; upcoming sources use
/// C/D so their automation never depends on moving the human crossfader.
pub fn performance_lanes(
    graph: &PerformanceGraph,
    prepared: &PreparedPerformance,
) -> Result<Vec<EntryLane>, String> {
    use patch_core::performance::MaterialKind;
    graph.validate_prepared()?;
    if graph.sources.len() > 4 {
        return Err(
            "one prepared performance horizon can use at most four distinct source tracks".into(),
        );
    }
    let entry_lanes = entry_lanes(graph, prepared)?;
    let entry_state = graph
        .states
        .iter()
        .find(|state| state.id == graph.entry)
        .ok_or("performance entry state is missing")?;
    let entry_sources: BTreeSet<_> = entry_state
        .layers
        .iter()
        .map(|layer| layer.source_id.as_str())
        .collect();
    let paths: BTreeMap<_, _> = prepared
        .sources
        .iter()
        .map(|source| (source.id.as_str(), source.path.as_path()))
        .collect();
    let mut lane_by_source: BTreeMap<String, usize> = entry_lanes
        .iter()
        .map(|lane| (lane.source_id.clone(), lane.lane))
        .collect();
    let mut free_future_lanes: Vec<_> = [2usize, 3]
        .into_iter()
        .filter(|lane| !lane_by_source.values().any(|assigned| assigned == lane))
        .collect();
    let mut future_sources: Vec<_> = graph
        .sources
        .iter()
        .map(|source| source.id.as_str())
        .filter(|source| !entry_sources.contains(source))
        .collect();
    future_sources.sort_unstable();
    for source_id in future_sources {
        let lane = free_future_lanes
            .first()
            .copied()
            .ok_or("future source needs an unused C/D arrangement lane")?;
        free_future_lanes.remove(0);
        lane_by_source.insert(source_id.to_owned(), lane);
    }

    let states: BTreeMap<_, _> = graph
        .states
        .iter()
        .map(|state| (state.id.as_str(), state))
        .collect();
    let mut result = Vec::with_capacity(lane_by_source.len());
    for source in &graph.sources {
        let layers: Vec<_> = graph
            .states
            .iter()
            .flat_map(|state| &state.layers)
            .filter(|layer| layer.source_id == source.id)
            .collect();
        let first = layers.first().ok_or("performance source has no layers")?;
        let stem_set_id = layers.iter().find_map(|layer| {
            matches!(layer.material, MaterialKind::Stem(_))
                .then(|| layer.stem_set_id.clone())
                .flatten()
        });
        let mut stem_gains = BTreeMap::new();
        for layer in &layers {
            if layer.source_start_s != first.source_start_s
                || layer.source_end_s != first.source_end_s
                || matches!(layer.material, MaterialKind::Stem(_))
                    && layer.stem_set_id != stem_set_id
            {
                return Err(format!(
                    "source {} changes transport region or material bundle inside the prepared horizon",
                    source.id
                ));
            }
            match &layer.material {
                MaterialKind::Stem(stem) => {
                    stem_gains.entry(stem.clone()).or_insert(0.0);
                }
                MaterialKind::OriginalMix => {}
            }
        }
        let mut original_gain = 0.0;
        if let Some(entry) = states.get(graph.entry.as_str()) {
            for layer in entry
                .layers
                .iter()
                .filter(|layer| layer.source_id == source.id)
            {
                match &layer.material {
                    MaterialKind::OriginalMix => original_gain = layer.gain,
                    MaterialKind::Stem(stem) => {
                        stem_gains.insert(stem.clone(), layer.gain);
                    }
                }
            }
        }
        let starts_at_entry = entry_sources.contains(source.id.as_str());
        let start_offset_samples = if starts_at_entry {
            0
        } else {
            prepared
                .schedule
                .events
                .iter()
                .filter_map(|event| {
                    let from = states.get(event.from.as_str())?;
                    let to = states.get(event.to.as_str())?;
                    let was_active = from.layers.iter().any(|layer| layer.source_id == source.id);
                    let becomes_active = to.layers.iter().any(|layer| layer.source_id == source.id);
                    (!was_active && becomes_active).then_some(event.at_sample)
                })
                .min()
                .ok_or_else(|| format!("source {} has no scheduled introduction edge", source.id))?
        };
        let path = paths
            .get(source.id.as_str())
            .ok_or_else(|| format!("performance source {} was not preflighted", source.id))?;
        result.push(EntryLane {
            lane: lane_by_source[&source.id],
            source_id: source.id.clone(),
            path: (*path).to_path_buf(),
            source_start_s: first.source_start_s,
            source_end_s: first.source_end_s,
            tempo_adjustment: tempo_adjustment(graph, &source.id)?,
            original_gain,
            starts_at_entry,
            live: false,
            start_offset_samples,
            stem_gains,
            stem_set_id,
        });
    }
    result.sort_by_key(|lane| lane.lane);
    Ok(result)
}

/// Whether a lane time-stretches (keylock) or resamples. Drums alone within
/// 2 % of their own tempo are resampled: their pitch moves at most a third
/// of a semitone, and every attack stays exactly where the grid puts it
/// (the stretcher moves kicks by milliseconds). Everything tonal keeps its
/// key through the stretcher.
pub fn lane_keylock<'a>(mut stems: impl Iterator<Item = &'a str>, tempo_adjustment: f64) -> bool {
    let drums_only =
        stems.next().is_some_and(|first| first == "drums") && stems.all(|s| s == "drums");
    !(drums_only && tempo_adjustment.abs() <= 0.02)
}

/// What a source plays across the graph: its stems by name, "mix" for its
/// original mix.
pub fn source_materials<'a>(
    graph: &'a PerformanceGraph,
    source_id: &'a str,
) -> impl Iterator<Item = &'a str> {
    graph
        .states
        .iter()
        .flat_map(|state| &state.layers)
        .filter(move |layer| layer.source_id == source_id)
        .map(|layer| match &layer.material {
            patch_core::performance::MaterialKind::Stem(stem) => stem.as_str(),
            patch_core::performance::MaterialKind::OriginalMix => "mix",
        })
}

fn tempo_adjustment(graph: &PerformanceGraph, source_id: &str) -> Result<f64, String> {
    let source = graph
        .sources
        .iter()
        .find(|source| source.id == source_id)
        .ok_or_else(|| format!("unknown performance source {source_id}"))?;
    let source_bpm = source
        .tempo_bpm
        .ok_or_else(|| format!("source {source_id} has no native tempo evidence"))?;
    let target_bpm = graph.tempo[0].bpm;
    let adjustment = target_bpm / source_bpm - 1.0;
    if !adjustment.is_finite() || !(-0.5..=0.5).contains(&adjustment) {
        return Err(format!(
            "source {source_id} needs unsupported tempo adjustment"
        ));
    }
    Ok(adjustment)
}

fn bind_live_entry_lanes(
    lanes: &mut [EntryLane],
    playing_paths: &[Option<String>],
    protected_lanes: &BTreeSet<usize>,
) -> Result<(), String> {
    for lane in lanes.iter_mut().filter(|lane| lane.starts_at_entry) {
        if let Some((physical, _)) = playing_paths
            .iter()
            .enumerate()
            .find(|(_, path)| path.as_deref() == Some(lane.path.to_string_lossy().as_ref()))
        {
            lane.lane = physical;
            lane.live = true;
        }
    }
    let mut occupied: BTreeSet<usize> = lanes
        .iter()
        .filter(|lane| lane.live)
        .map(|lane| lane.lane)
        .collect();
    for lane in lanes.iter_mut().filter(|lane| !lane.live) {
        if !protected_lanes.contains(&lane.lane) && occupied.insert(lane.lane) {
            continue;
        }
        let candidates: &[usize] = if lane.starts_at_entry {
            &[0, 1, 2, 3]
        } else {
            // Future sources never borrow A/B: those lanes belong to the
            // live mixer and may contain the user's current performance.
            &[2, 3]
        };
        let free = candidates
            .iter()
            .copied()
            .find(|physical| !occupied.contains(physical) && !protected_lanes.contains(physical))
            .ok_or("no free engine lane for this performance source")?;
        lane.lane = free;
        occupied.insert(free);
    }
    Ok(())
}

/// Decode and adopt the prepared entry-state material on its assigned deck
/// workers. A matching live entry source stays on its current transport; all
/// other lanes are prepared paused. Call only from a background worker.
pub fn stage_entry(
    graph: &PerformanceGraph,
    prepared: &PreparedPerformance,
    params: &crate::Params,
) -> Result<Vec<EntryLane>, String> {
    use crate::deck::transport::Cmd;
    verify_fresh(prepared)?;
    let rate = match params.last_rate.load(std::sync::atomic::Ordering::Acquire) {
        0 => 48_000,
        rate => rate,
    };
    if prepared.schedule.sample_rate != rate {
        return Err("prepared performance sample rate does not match the running engine".into());
    }
    let mut lanes = performance_lanes(graph, prepared)?;
    // The planned entry lane is logical, while a live deck is physical. Keep
    // a matching playing source on its current A/B worker.
    let playing_paths = params
        .decks
        .iter()
        .map(|worker| {
            let status = worker.status.lock().unwrap();
            status.playing.then(|| status.path.to_string())
        })
        .collect::<Vec<_>>();
    let protected_lanes = params
        .decks
        .iter()
        .enumerate()
        .filter_map(|(index, worker)| {
            worker.stems.snapshot()["controls"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|control| control["human"] == true)
                .then_some(index)
        })
        .collect::<BTreeSet<_>>();
    bind_live_entry_lanes(&mut lanes, &playing_paths, &protected_lanes)?;
    for (index, worker) in params.decks.iter().enumerate() {
        let status = worker.status.lock().unwrap();
        if status.playing && !lanes.iter().any(|lane| lane.live && lane.lane == index) {
            return Err(format!(
                "Deck {} is playing a source outside this performance entry",
                (b'A' + index as u8) as char
            ));
        }
    }
    let stem_signature = |worker: &crate::deck::worker::Shared| {
        worker.stems.snapshot()["controls"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|slot| {
                json!({
                    "id":slot["id"],
                    "epoch":slot["epoch"],
                    "gain":slot["gain"],
                    "mute":slot["mute"],
                    "solo":slot["solo"],
                    "human":slot["human"],
                })
            })
            .collect::<Vec<_>>()
    };
    let mut initial_decks = BTreeMap::new();
    for lane in &mut lanes {
        let worker = params
            .decks
            .get(lane.lane)
            .ok_or_else(|| format!("performance needs unavailable deck lane {}", lane.lane))?;
        let status = worker.status.lock().unwrap();
        if status.playing {
            if !lane.starts_at_entry
                || status.path.as_ref() != lane.path.to_string_lossy().as_ref()
                || status.scheduled_start_sample.is_some()
                || status.loop_on
                || status.slip
                || status.previewing
                || status.bend.abs() > 1e-6
                || lane.stem_set_id.is_some()
                || (status.tempo - lane.tempo_adjustment).abs() > 0.003
            {
                return Err(format!(
                    "Deck {} cannot serve as a live performance source in its current transport state",
                    (b'A' + lane.lane as u8) as char
                ));
            }
            let (frame, step, _) = worker.timed_playhead();
            let target_frame = lane.source_start_s * f64::from(rate);
            if !step.is_finite() || step <= 0.0 || target_frame <= frame {
                return Err(format!(
                    "Deck {} has already passed the planned live source region",
                    (b'A' + lane.lane as u8) as char
                ));
            }
            lane.live = true;
        } else if status.scheduled_start_sample.is_some() {
            return Err(format!(
                "Deck {} already has a scheduled start and cannot be replaced during entry staging",
                (b'A' + lane.lane as u8) as char
            ));
        }
        initial_decks.insert(
            lane.lane,
            (
                status.loaded,
                status.path.clone(),
                status.playing,
                status.tempo,
                stem_signature(worker),
            ),
        );
    }

    // Predecode every required asset before mutating any live deck state.
    let mut staged = Vec::with_capacity(lanes.len());
    for lane in &lanes {
        let pcm = if lane.live {
            None
        } else {
            Some(
                crate::deck::pcm::Pcm::load(&lane.path, rate).map_err(|error| {
                    format!(
                        "Could not load performance source {}: {error}",
                        lane.source_id
                    )
                })?,
            )
        };
        let stems = lane
            .stem_set_id
            .as_deref()
            .map(|set_id| {
                let lease = prepared
                    ._stem_leases
                    .iter()
                    .find(|lease| lease.set.id == set_id)
                    .ok_or_else(|| format!("prepared stem cache lease {set_id} is missing"))?
                    .try_clone()?;
                crate::stem_service::decode_lease(lane.path.to_string_lossy().as_ref(), lease, rate)
            })
            .transpose()?;
        staged.push((lane.clone(), pcm, stems));
    }

    // Decoding may take long enough for a human to load, start or change stem
    // controls on a target deck. Those newer actions win; do not overwrite.
    for lane in &lanes {
        let worker = &params.decks[lane.lane];
        let status = worker.status.lock().unwrap();
        let (was_loaded, was_path, was_playing, was_tempo, was_stems) = &initial_decks[&lane.lane];
        let changed = if lane.live {
            !status.playing
                || !was_playing
                || status.path != *was_path
                || status.scheduled_start_sample.is_some()
                || (status.tempo - *was_tempo).abs() > 0.003
        } else {
            status.playing
                || status.scheduled_start_sample.is_some()
                || status.loaded != *was_loaded
                || status.path != *was_path
                || stem_signature(worker) != *was_stems
        };
        if changed {
            return Err(format!(
                "Deck {} changed while the entry state was being prepared; the newer deck action was kept",
                (b'A' + lane.lane as u8) as char
            ));
        }
    }

    for (lane, pcm, stems) in staged {
        if lane.live {
            continue;
        }
        let worker = &params.decks[lane.lane];
        worker
            .rate
            .store(rate, std::sync::atomic::Ordering::Release);
        worker.send(Cmd::Load(pcm.ok_or("staged deck audio is missing")?));
        worker.send(Cmd::Seek(lane.source_start_s));
        worker.send(Cmd::Keylock(lane_keylock(
            source_materials(graph, &lane.source_id),
            lane.tempo_adjustment,
        )));
        worker.send(Cmd::Tempo(lane.tempo_adjustment));
        if let Some(stems) = stems {
            worker.send(Cmd::AttachStems(Box::new(stems)));
        }
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let adopted = lanes.iter().all(|lane| {
            let worker = &params.decks[lane.lane];
            let status = worker.status.lock().unwrap();
            status.loaded
                && status.path.as_ref() == lane.path.to_string_lossy().as_ref()
                && (lane.stem_set_id.is_none()
                    || worker
                        .stems
                        .ready
                        .load(std::sync::atomic::Ordering::Acquire))
        });
        if adopted {
            break;
        }
        if std::time::Instant::now() >= deadline {
            return Err("One or more deck workers did not adopt the staged entry state".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    for lane in &lanes {
        if lane.live {
            continue;
        }
        params.decks[lane.lane]
            .stems
            .set_original_gain(lane.original_gain)?;
        if lane.stem_set_id.is_some() {
            let worker = &params.decks[lane.lane];
            let controls = worker.stems.snapshot();
            for control in controls["controls"].as_array().into_iter().flatten() {
                let id = control["id"]
                    .as_str()
                    .ok_or("stem layout has an invalid ID")?;
                let gain = lane.stem_gains.get(id).copied().unwrap_or(0.0);
                let slot = worker
                    .stems
                    .index(id)
                    .ok_or("stem layout changed during staging")?;
                worker
                    .stems
                    .set(slot, patch_core::stems::StemValue::Gain(gain), false)?;
            }
        }
    }
    Ok(lanes)
}

/// Start every staged entry lane at one future output-clock sample. The
/// worker has already decoded and adopted its source; the RT consumer holds
/// each new generation silent until the shared sample, then begins all lanes
/// on that exact frame. This does no I/O and does not wait.
pub fn start_entry(params: &crate::Params, lanes: &[EntryLane]) -> Result<u64, String> {
    let start_sample = entry_start_sample(params, lanes)?;
    start_entry_at(params, lanes, start_sample)?;
    Ok(start_sample)
}

/// Choose a common future graph-clock sample without queuing playback.
pub fn entry_start_sample(params: &crate::Params, lanes: &[EntryLane]) -> Result<u64, String> {
    validate_entry_lanes(params, lanes)?;
    let current = params
        .decks
        .iter()
        .map(|worker| worker.graph_sample())
        .max()
        .unwrap_or(0);
    let live_samples = lanes
        .iter()
        .filter(|lane| lane.live)
        .map(|lane| live_source_start_sample(params, lane))
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(&start) = live_samples.first() {
        if live_samples.iter().any(|sample| sample.abs_diff(start) > 1) {
            return Err(
                "live entry sources do not reach their planned regions on the same sample".into(),
            );
        }
        if start < current.saturating_add(crate::deck::worker::TARGET_FRAMES as u64) {
            return Err(
                "the live source region is too close or has passed; prepare the next phrase".into(),
            );
        }
        return Ok(start);
    }
    Ok(current.saturating_add((crate::deck::worker::TARGET_FRAMES * 2) as u64))
}

fn live_source_start_sample(params: &crate::Params, lane: &EntryLane) -> Result<u64, String> {
    let worker = params
        .decks
        .get(lane.lane)
        .ok_or_else(|| format!("performance needs unavailable deck lane {}", lane.lane))?;
    let status = worker.status.lock().unwrap();
    if !status.playing
        || status.path.as_ref() != lane.path.to_string_lossy().as_ref()
        || status.loop_on
        || status.slip
        || status.previewing
        || status.bend.abs() > 1e-6
        || (status.tempo - lane.tempo_adjustment).abs() > 0.003
    {
        return Err(format!(
            "Deck {} changed its live transport after preparation",
            (b'A' + lane.lane as u8) as char
        ));
    }
    drop(status);
    let rate = worker
        .rate
        .load(std::sync::atomic::Ordering::Acquire)
        .max(1) as f64;
    let (frame, step, sample) = worker.timed_playhead();
    let target_frame = lane.source_start_s * rate;
    source_target_sample(frame, step, sample, target_frame).map_err(|_| {
        format!(
            "Deck {} has already passed the planned live source region",
            (b'A' + lane.lane as u8) as char
        )
    })
}

fn source_target_sample(frame: f64, step: f64, sample: u64, target_frame: f64) -> Result<u64, ()> {
    let frames_remaining = target_frame - frame;
    if !frame.is_finite()
        || !step.is_finite()
        || step <= 0.0
        || !frames_remaining.is_finite()
        || frames_remaining <= 0.0
    {
        return Err(());
    }
    let output_frames = (frames_remaining / step).ceil();
    if !output_frames.is_finite() || output_frames > u64::MAX as f64 {
        return Err(());
    }
    sample.checked_add(output_frames as u64).ok_or(())
}

/// Queue all lanes for a previously selected shared graph-clock sample.
pub fn start_entry_at(
    params: &crate::Params,
    lanes: &[EntryLane],
    start_sample: u64,
) -> Result<(), String> {
    use crate::deck::transport::Cmd;
    validate_entry_lanes(params, lanes)?;
    let current = params
        .decks
        .iter()
        .map(|worker| worker.graph_sample())
        .max()
        .unwrap_or(0);
    if start_sample < current.saturating_add(crate::deck::worker::TARGET_FRAMES as u64) {
        return Err("the shared start sample is too close for deck buffers to prepare".into());
    }
    for lane in lanes {
        let at = start_sample
            .checked_add(lane.start_offset_samples)
            .ok_or("scheduled source start exceeds engine sample clock")?;
        if !lane.live {
            params.decks[lane.lane].send(Cmd::PlayAt(at));
        }
    }
    Ok(())
}

fn validate_entry_lanes(params: &crate::Params, lanes: &[EntryLane]) -> Result<(), String> {
    if lanes.is_empty() {
        return Err("the performance entry has no active lanes".into());
    }
    let mut seen = BTreeSet::new();
    for lane in lanes {
        if !seen.insert(lane.lane) {
            return Err("the performance entry assigns a lane more than once".into());
        }
        let worker = params
            .decks
            .get(lane.lane)
            .ok_or_else(|| format!("performance needs unavailable deck lane {}", lane.lane))?;
        let status = worker.status.lock().unwrap();
        if !status.loaded
            || status.path.as_ref() != lane.path.to_string_lossy().as_ref()
            || status.playing != lane.live
            || status.scheduled_start_sample.is_some()
            || lane.live
                && (status.loop_on
                    || status.slip
                    || status.previewing
                    || status.bend.abs() > 1e-6
                    || (status.tempo - lane.tempo_adjustment).abs() > 0.003)
            || lane.stem_set_id.is_some()
                && !worker
                    .stems
                    .ready
                    .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(format!(
                "Deck {} no longer holds its paused staged entry material",
                (b'A' + lane.lane as u8) as char
            ));
        }
    }
    Ok(())
}

/// Compile only layer gain operations whose synchronized source transport is
/// already staged in the entry state. Track changes and loops need additional
/// prepared-player operations and are rejected explicitly.
pub fn compile_entry_automation(
    graph: &PerformanceGraph,
    prepared: &PreparedPerformance,
    origin_sample: u64,
) -> Result<Vec<ScheduledLayerAutomation>, String> {
    let lanes = performance_lanes(graph, prepared)?;
    compile_entry_automation_on_lanes(graph, prepared, &lanes, origin_sample)
}

/// Compile graph events against the physical lane assignments chosen during
/// staging. This preserves a live source's actual A/B deck identity.
pub fn compile_entry_automation_on_lanes(
    graph: &PerformanceGraph,
    prepared: &PreparedPerformance,
    lanes: &[EntryLane],
    origin_sample: u64,
) -> Result<Vec<ScheduledLayerAutomation>, String> {
    use patch_core::performance::{ArrangementOperation as Op, MaterialKind};
    let entry_sources: BTreeMap<_, _> = lanes
        .iter()
        .map(|lane| (lane.source_id.as_str(), lane))
        .collect();
    let states: BTreeMap<_, _> = graph.states.iter().map(|s| (s.id.as_str(), s)).collect();
    let mut out = Vec::new();
    for event in &prepared.schedule.events {
        let from = states
            .get(event.from.as_str())
            .ok_or_else(|| format!("schedule references missing state {}", event.from))?;
        let to = states
            .get(event.to.as_str())
            .ok_or_else(|| format!("schedule references missing state {}", event.to))?;
        let mut targets: Vec<(&patch_core::performance::ActiveLayer, f32, u64)> = Vec::new();
        match &event.operation {
            Op::SetGain {
                layer_id, to: gain, ..
            } => {
                let layer = to
                    .layers
                    .iter()
                    .find(|layer| layer.id == *layer_id)
                    .ok_or("gain event target layer is missing")?;
                targets.push((
                    layer,
                    *gain,
                    event.end_sample.saturating_sub(event.at_sample),
                ));
            }
            Op::SetGains { targets: gains, .. } => {
                let duration = event.end_sample.saturating_sub(event.at_sample);
                for target in gains {
                    let layer = to
                        .layers
                        .iter()
                        .find(|layer| layer.id == target.layer_id)
                        .ok_or("gain event target layer is missing")?;
                    targets.push((layer, target.to, duration));
                }
            }
            Op::IntroduceLayer { layer_id } => {
                let layer = to
                    .layers
                    .iter()
                    .find(|layer| layer.id == *layer_id)
                    .ok_or("introduced layer is missing")?;
                targets.push((layer, layer.gain, 0));
            }
            Op::RemoveLayer { layer_id } => {
                let layer = from
                    .layers
                    .iter()
                    .find(|layer| layer.id == *layer_id)
                    .ok_or("removed layer is missing")?;
                targets.push((layer, 0.0, 0));
            }
            Op::ReplaceLayer {
                remove_id,
                introduce_id,
            } => {
                let old = from
                    .layers
                    .iter()
                    .find(|layer| layer.id == *remove_id)
                    .ok_or("replaced layer is missing")?;
                let new = to
                    .layers
                    .iter()
                    .find(|layer| layer.id == *introduce_id)
                    .ok_or("replacement layer is missing")?;
                targets.push((old, 0.0, 0));
                targets.push((new, new.gain, 0));
            }
            Op::ReturnToOriginal {
                source_id,
                over_beats: _,
            } => {
                let duration = event.end_sample.saturating_sub(event.at_sample);
                for layer in to
                    .layers
                    .iter()
                    .filter(|layer| layer.source_id == *source_id)
                {
                    let gain = match &layer.material {
                        MaterialKind::OriginalMix => 1.0,
                        MaterialKind::Stem(_) => 0.0,
                    };
                    targets.push((layer, gain, duration));
                }
            }
            Op::LoopLayer { .. } => {
                return Err(
                    "this arrangement operation needs a prepared source-player action".into(),
                );
            }
        }
        for (layer, to_gain, duration_samples) in targets {
            let stem_id = match &layer.material {
                MaterialKind::Stem(id) => Some(id.clone()),
                MaterialKind::OriginalMix => None,
            };
            let lane = entry_sources.get(layer.source_id.as_str()).ok_or_else(|| {
                format!(
                    "source {} is not loaded in the entry state",
                    layer.source_id
                )
            })?;
            if matches!(layer.material, MaterialKind::Stem(_))
                && lane.stem_set_id.as_deref() != layer.stem_set_id.as_deref()
            {
                return Err(format!(
                    "source {} does not have the scheduled StemSet loaded",
                    layer.source_id
                ));
            }
            let at_sample = origin_sample
                .checked_add(event.at_sample)
                .ok_or("scheduled stem automation exceeds the engine sample clock")?;
            out.push(ScheduledLayerAutomation {
                lane: lane.lane,
                source_id: layer.source_id.clone(),
                stem_id,
                at_sample,
                duration_samples,
                to_gain,
            });
        }
    }
    out.sort_by(|a, b| {
        a.at_sample
            .cmp(&b.at_sample)
            .then_with(|| a.lane.cmp(&b.lane))
            .then_with(|| a.stem_id.cmp(&b.stem_id))
    });
    Ok(out)
}

/// Pause a source at the sample where its final graph edge removes it. Sources
/// that appear again later in the graph keep their transport alive.
pub fn compile_final_source_pauses(
    graph: &PerformanceGraph,
    prepared: &PreparedPerformance,
    lanes: &[EntryLane],
    origin_sample: u64,
) -> Result<Vec<ScheduledDeckPause>, String> {
    let states: BTreeMap<_, _> = graph
        .states
        .iter()
        .map(|state| (state.id.as_str(), state))
        .collect();
    let mut final_at = BTreeMap::<&str, u64>::new();
    for (event_index, event) in prepared.schedule.events.iter().enumerate() {
        let from = states
            .get(event.from.as_str())
            .ok_or_else(|| format!("schedule references missing state {}", event.from))?;
        let to = states
            .get(event.to.as_str())
            .ok_or_else(|| format!("schedule references missing state {}", event.to))?;
        let from_sources: BTreeSet<_> = from
            .layers
            .iter()
            .map(|layer| layer.source_id.as_str())
            .collect();
        let to_sources: BTreeSet<_> = to
            .layers
            .iter()
            .map(|layer| layer.source_id.as_str())
            .collect();
        for source_id in from_sources.difference(&to_sources) {
            let reappears = prepared.schedule.events[event_index + 1..]
                .iter()
                .any(|later| {
                    states.get(later.to.as_str()).is_some_and(|state| {
                        state
                            .layers
                            .iter()
                            .any(|layer| layer.source_id == **source_id)
                    })
                });
            if !reappears {
                final_at.insert(
                    source_id,
                    origin_sample
                        .checked_add(event.end_sample)
                        .ok_or("scheduled source pause exceeds engine sample clock")?,
                );
            }
        }
    }
    let mut pauses = Vec::new();
    for (source_id, at_sample) in final_at {
        let lane = lanes
            .iter()
            .find(|lane| lane.source_id == source_id)
            .ok_or_else(|| format!("final source {source_id} has no prepared deck lane"))?;
        pauses.push(ScheduledDeckPause {
            lane: lane.lane,
            source_id: source_id.to_owned(),
            at_sample,
        });
    }
    pauses.sort_by_key(|pause| (pause.at_sample, pause.lane));
    Ok(pauses)
}

/// After an arrangement: the source that plays on (on a C/D lane) goes back
/// to the A/B deck the outgoing track left, once that deck has stopped. Some
/// `(lane, from, to, after_sample)` when there is exactly one source left,
/// it is on C/D, and an outgoing A/B deck is released.
pub fn hand_back_plan(
    lanes: &[EntryLane],
    pauses: &[ScheduledDeckPause],
) -> Option<(EntryLane, usize, usize, u64)> {
    let paused: BTreeSet<&str> = pauses.iter().map(|p| p.source_id.as_str()).collect();
    let mut staying = lanes
        .iter()
        .filter(|l| !paused.contains(l.source_id.as_str()));
    let lane = staying.next()?;
    if staying.next().is_some() || lane.lane < 2 {
        return None;
    }
    let released = pauses
        .iter()
        .filter(|p| p.lane < 2)
        .max_by_key(|p| p.at_sample)?;
    let after = pauses.iter().map(|p| p.at_sample).max()?;
    Some((lane.clone(), lane.lane, released.lane, after))
}

/// Hand the playing source on deck `from` (an arrangement's C/D lane) to deck
/// `to` (a free A/B deck the set planner continues from) at graph sample
/// `at`, without a seam: `to` starts on that sample at the frame `from` will
/// have reached, with its tempo and keylock, at `from`'s channel gain; `from`
/// is gated silent on the same sample. `pcm` loads `to` first when it does
/// not hold the source yet. Call it a second or more before `at`, while
/// `from` plays; it does no waiting on the RT thread.
pub fn hand_back_at(
    params: &crate::Params,
    from: usize,
    to: usize,
    pcm: Option<std::sync::Arc<crate::deck::pcm::Pcm>>,
    at: u64,
) -> Result<(), String> {
    use crate::deck::transport::Cmd;
    let (source, target) = (
        params
            .decks
            .get(from)
            .ok_or("hand-back source deck is missing")?,
        params
            .decks
            .get(to)
            .ok_or("hand-back target deck is missing")?,
    );
    let (path, tempo, keylock) = {
        let st = source.status.lock().unwrap();
        if !st.playing {
            return Err("the arrangement's source stopped before its hand-back".into());
        }
        (st.path.clone(), st.tempo, st.keylock)
    };
    {
        let st = target.status.lock().unwrap();
        if st.playing || st.scheduled_start_sample.is_some() {
            return Err(format!(
                "Deck {} is busy; the arrangement keeps its source",
                (b'A' + to as u8) as char
            ));
        }
    }
    if let Some(pcm) = pcm {
        target.send(Cmd::Load(pcm));
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while target.status.lock().unwrap().path != path {
        if std::time::Instant::now() >= deadline {
            return Err("the hand-back deck did not load the source".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let (frame, step, sample) = source.timed_playhead();
    if !step.is_finite() || step <= 0.0 || at <= sample {
        return Err("the hand-back sample has already passed".into());
    }
    let rate = target
        .rate
        .load(std::sync::atomic::Ordering::Acquire)
        .max(1);
    let position = (frame + (at - sample) as f64 * step) / f64::from(rate);
    target.send(Cmd::Keylock(keylock));
    target.send(Cmd::Tempo(tempo));
    target.send(Cmd::Seek(position));
    target.send(Cmd::PlayAt(at));
    // The target keeps its own (the DJ's) level: the arrangement played its
    // lanes at it. The source closes on the sample.
    let channel = |deck: usize| params.channel(&format!("deck-{}", (b'a' + deck as u8) as char));
    if let Some(f) = channel(from)
        && !params.post_at(at, f, crate::TIMED_GAIN, 0.0)
    {
        return Err("the timed-change queue is full".into());
    }
    source.send(Cmd::PauseAt(at));
    Ok(())
}

/// Arm source releases only after entry playback has been accepted.
pub fn schedule_final_source_pauses(params: &crate::Params, pauses: &[ScheduledDeckPause]) {
    for pause in pauses {
        if let Some(worker) = params.decks.get(pause.lane) {
            worker.send(crate::deck::transport::Cmd::PauseAt(pause.at_sample));
        }
    }
}

/// Arm a previously compiled layer-automation batch on its existing deck
/// transports. The bounded per-frame deck mixer applies each ramp at its
/// requested engine sample.
pub fn schedule_entry_automation(
    params: &crate::Params,
    moves: &[ScheduledLayerAutomation],
) -> Result<(), String> {
    let mut resolved = Vec::with_capacity(moves.len());
    let mut queue_state = BTreeMap::<(usize, String), (u64, u64)>::new();
    let mut additions = BTreeMap::<(usize, String), Vec<(u64, u64)>>::new();
    for movement in moves {
        let worker = params
            .decks
            .get(movement.lane)
            .ok_or_else(|| format!("automation requires unavailable lane {}", movement.lane))?;
        let (slot, control) = if let Some(stem_id) = &movement.stem_id {
            if !worker
                .stems
                .ready
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err(format!(
                    "Deck {} no longer has prepared stems",
                    (b'A' + movement.lane as u8) as char
                ));
            }
            let slot = worker
                .stems
                .index(stem_id)
                .ok_or_else(|| format!("stem {stem_id} is unavailable on its assigned lane"))?;
            let controls = worker.stems.snapshot();
            let control = controls["controls"]
                .get(slot)
                .cloned()
                .ok_or("stem controls changed during scheduling")?;
            if control["human"] == true {
                return Err(format!("stem {stem_id} is now controlled by the DJ"));
            }
            (Some(slot), control)
        } else {
            let controls = worker.stems.snapshot();
            (None, controls["original_mix"].clone())
        };
        let earliest = worker
            .graph_sample()
            .saturating_add(crate::deck::worker::TARGET_FRAMES as u64);
        if movement.at_sample < earliest {
            return Err(format!(
                "stem {} automation is too close to the live sample clock",
                movement.stem_id.as_deref().unwrap_or("original mix")
            ));
        }
        let automation_key = movement
            .stem_id
            .clone()
            .unwrap_or_else(|| "original".into());
        let key = (movement.lane, automation_key);
        queue_state.entry(key.clone()).or_insert_with(|| {
            (
                control["automation_queued"].as_u64().unwrap_or(0),
                control["automation_end_sample"].as_u64().unwrap_or(0),
            )
        });
        additions
            .entry(key)
            .or_default()
            .push((movement.at_sample, movement.duration_samples));
        resolved.push((movement, worker, slot));
    }
    for (key, mut ramps) in additions {
        ramps.sort_unstable_by_key(|(at, _)| *at);
        let (queued, mut end) = queue_state[&key];
        if queued.saturating_add(ramps.len() as u64)
            > crate::deck::stems::MAX_RAMPS_PER_GAIN_CONTROL as u64
        {
            return Err(format!(
                "stem {} exceeds its bounded automation horizon",
                key.1
            ));
        }
        for (at, duration) in ramps {
            if at < end {
                return Err(format!(
                    "stem {} has overlapping or out-of-order automation",
                    key.1
                ));
            }
            end = at
                .checked_add(duration)
                .ok_or("scheduled stem automation exceeds the engine sample clock")?;
        }
    }
    resolved.sort_by_key(|(movement, _, _)| movement.at_sample);
    for (movement, worker, slot) in resolved {
        if let Some(slot) = slot {
            worker.stems.ramp(
                slot,
                movement.at_sample,
                movement.duration_samples,
                movement.to_gain,
            )?;
        } else {
            worker.stems.ramp_original(
                movement.at_sample,
                movement.duration_samples,
                movement.to_gain,
            )?;
        }
    }
    Ok(())
}

/// Remove the automation batch for a failed start while preserving any stem
/// that became human-owned during the attempt.
pub fn cancel_entry_automation(params: &crate::Params, moves: &[ScheduledLayerAutomation]) {
    let mut seen = BTreeSet::new();
    for movement in moves {
        if !seen.insert((
            movement.lane,
            movement.stem_id.as_deref().unwrap_or("original"),
        )) {
            continue;
        }
        let Some(worker) = params.decks.get(movement.lane) else {
            continue;
        };
        if let Some(stem_id) = &movement.stem_id {
            let Some(slot) = worker.stems.index(stem_id) else {
                continue;
            };
            let controls = worker.stems.snapshot();
            let Some(control) = controls["controls"].get(slot) else {
                continue;
            };
            if control["human"] == true {
                continue;
            }
            let gain = control["gain"].as_f64().unwrap_or(1.0) as f32;
            let _ = worker
                .stems
                .set(slot, patch_core::stems::StemValue::Gain(gain), false);
        } else {
            worker.stems.cancel_original_automation();
        }
    }
}

/// Allocate up to the engine's four synchronized deck workers over the
/// selected linear arrangement. Hints reserve current live tracks on known
/// lanes; additional sources receive a free lane, which can be reused after
/// its previous source leaves the state graph.
pub fn allocate_lanes(
    graph: &PerformanceGraph,
    preferred_lanes: &BTreeMap<String, usize>,
) -> Result<Vec<PerformanceLaneAssignment>, String> {
    graph.validate()?;
    if preferred_lanes
        .values()
        .any(|lane| *lane >= crate::MAX_DECKS)
    {
        return Err("preferred performance lane is outside A–D".into());
    }
    let source_ids: BTreeSet<_> = graph
        .sources
        .iter()
        .map(|source| source.id.as_str())
        .collect();
    if preferred_lanes
        .keys()
        .any(|source_id| !source_ids.contains(source_id.as_str()))
    {
        return Err("preferred lane references an unknown source".into());
    }
    let mut next = BTreeMap::new();
    for edge in &graph.edges {
        if next.insert(edge.from.as_str(), edge.to.as_str()).is_some() {
            return Err("select one arrangement branch before lane allocation".into());
        }
    }
    let states: BTreeMap<_, _> = graph
        .states
        .iter()
        .map(|state| (state.id.as_str(), state))
        .collect();
    let mut current = graph.entry.as_str();
    let mut visited = BTreeSet::new();
    let mut lanes: BTreeMap<String, usize> = BTreeMap::new();
    let mut assignments = Vec::new();
    loop {
        if !visited.insert(current) {
            return Err("performance graph contains a cycle".into());
        }
        let state = states
            .get(current)
            .ok_or_else(|| format!("unknown performance state {current}"))?;
        let live: BTreeSet<_> = state
            .layers
            .iter()
            .map(|layer| layer.source_id.as_str())
            .collect();
        lanes.retain(|source_id, _| live.contains(source_id.as_str()));
        let mut arriving: Vec<_> = live
            .iter()
            .copied()
            .filter(|source_id| !lanes.contains_key(*source_id))
            .collect();
        arriving.sort_unstable();

        // Honor explicit reservations before filling unassigned sources.
        for source_id in arriving.iter().copied() {
            let Some(&lane) = preferred_lanes.get(source_id) else {
                continue;
            };
            if lanes.values().any(|active| *active == lane) {
                return Err(format!(
                    "preferred lane {lane} is occupied in state {current}"
                ));
            }
            lanes.insert(source_id.into(), lane);
        }
        for source_id in arriving {
            if lanes.contains_key(source_id) {
                continue;
            }
            let lane = (0..crate::MAX_DECKS)
                .find(|lane| !lanes.values().any(|active| active == lane))
                .ok_or_else(|| {
                    format!(
                        "more than {} sources overlap in state {current}",
                        crate::MAX_DECKS
                    )
                })?;
            lanes.insert(source_id.into(), lane);
        }
        for layer in &state.layers {
            let lane = *lanes
                .get(&layer.source_id)
                .ok_or_else(|| format!("no lane allocated for source {}", layer.source_id))?;
            assignments.push(PerformanceLaneAssignment {
                state_id: state.id.clone(),
                source_id: layer.source_id.clone(),
                layer_id: layer.id.clone(),
                lane,
            });
        }
        let Some(next_state) = next.get(current) else {
            break;
        };
        current = next_state;
    }
    if visited.len() != graph.states.len() {
        return Err("performance graph is not one selected linear path".into());
    }
    Ok(assignments)
}

fn bind_stems(
    graph: &PerformanceGraph,
    sources: &[BoundSource],
    cache: Option<&patch_stems::cache::Cache>,
) -> Result<(Vec<BoundStem>, Vec<patch_stems::cache::Lease>), String> {
    use patch_core::performance::MaterialKind;
    let requested: Vec<_> = graph
        .states
        .iter()
        .flat_map(|state| &state.layers)
        .filter_map(|layer| match &layer.material {
            MaterialKind::OriginalMix => None,
            MaterialKind::Stem(stem_id) => Some((
                layer.source_id.as_str(),
                layer.stem_set_id.as_deref().unwrap_or_default(),
                stem_id.as_str(),
                layer.source_start_s,
                layer.source_end_s,
            )),
        })
        .collect();
    if requested.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let cache = cache.ok_or("stem cache is required for stem layers")?;
    let mut leases = BTreeMap::new();
    for (_, set_id, _, _, _) in &requested {
        if !leases.contains_key(*set_id) {
            let lease = cache.acquire(set_id)?;
            leases.insert(*set_id, lease);
        }
    }
    let source_hashes: BTreeMap<_, _> = sources
        .iter()
        .map(|source| (source.id.as_str(), source.content_hash.as_str()))
        .collect();
    let mut bound = Vec::with_capacity(requested.len());
    for (source_id, set_id, stem_id, start_s, end_s) in requested {
        let source_hash = source_hashes
            .get(source_id)
            .ok_or_else(|| format!("stem layer references unbound source {source_id}"))?;
        let lease = leases
            .get(set_id)
            .ok_or_else(|| format!("stem cache {set_id} was not leased"))?;
        if lease.set.id != set_id || lease.set.source_hash.as_str() != *source_hash {
            return Err(format!(
                "StemSet {set_id} does not belong to source {source_id}"
            ));
        }
        if end_s > lease.set.duration + 0.05 || start_s < 0.0 {
            return Err(format!("stem region exceeds StemSet {set_id} duration"));
        }
        if !lease.set.layout.iter().any(|id| id == stem_id) {
            return Err(format!("StemSet {set_id} does not contain {stem_id}"));
        }
        let artifact = lease
            .set
            .stems
            .get(stem_id)
            .ok_or_else(|| format!("StemSet {set_id} has no artifact for {stem_id}"))?;
        if artifact.start_frame != 0 || artifact.frames != lease.set.frames {
            return Err(format!(
                "stem {stem_id} in {set_id} is not playback aligned"
            ));
        }
        let path = lease.directory.join(&artifact.cache_ref);
        if patch_stems::hash_file(&path)? != artifact.checksum {
            return Err(format!("stem checksum mismatch for {stem_id} in {set_id}"));
        }
        bound.push(BoundStem {
            source_id: source_id.into(),
            stem_set_id: set_id.into(),
            stem_id: stem_id.into(),
            path,
            checksum: artifact.checksum.clone(),
        });
    }
    Ok((bound, leases.into_values().collect()))
}

/// Recheck identities immediately before handing sources to media decoders.
/// This closes the planning-to-load race for edits and replacements.
pub fn verify_fresh(prepared: &PreparedPerformance) -> Result<(), String> {
    for source in &prepared.sources {
        let actual = patch_stems::hash_file(&source.path)?;
        if actual != source.content_hash {
            return Err(format!("source {} changed after preparation", source.id));
        }
    }
    verify_bound_stems(&prepared.stems)?;
    Ok(())
}

fn verify_bound_stems(stems: &[BoundStem]) -> Result<(), String> {
    for stem in stems {
        if patch_stems::hash_file(&stem.path)? != stem.checksum {
            return Err(format!(
                "stem {} in {} changed after preparation",
                stem.stem_id, stem.stem_set_id
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "performance_tests.rs"]
mod tests;
