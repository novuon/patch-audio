//! Source-referenced arrangement graph contracts shared by AI planning and
//! deterministic preparation/execution. Tracks remain the source objects;
//! original mixes and stems are selectable materials from those tracks.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_PERFORMANCE_SOURCES: usize = 8;
pub const MAX_ACTIVE_LAYERS: usize = 16;
pub const MAX_SCHEDULED_OPERATIONS: usize = 256;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PerformanceSource {
    /// Stable Patch track identity.
    pub id: String,
    /// Present only once content identity has been resolved for preparation.
    /// Draft plans may refer to the stable library identity alone.
    #[serde(default)]
    pub content_hash: Option<String>,
    /// Native tempo at the selected source region. Required for execution so
    /// every lane can be synchronized to the performance clock before play.
    #[serde(default)]
    pub tempo_bpm: Option<f64>,
    pub duration_s: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "stem", rename_all = "snake_case")]
pub enum MaterialKind {
    OriginalMix,
    Stem(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActiveLayer {
    pub id: String,
    pub source_id: String,
    pub material: MaterialKind,
    /// Exact managed cache identity for derived stem audio. Original mixes
    /// must leave this empty; a stem name alone is not a stable asset ref.
    #[serde(default)]
    pub stem_set_id: Option<String>,
    pub source_start_s: f64,
    pub source_end_s: f64,
    pub gain: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PerformanceState {
    pub id: String,
    /// Beat position on the performance clock, not a source track's beat index.
    pub at_beat: f64,
    pub layers: Vec<ActiveLayer>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ArrangementOperation {
    IntroduceLayer {
        layer_id: String,
    },
    RemoveLayer {
        layer_id: String,
    },
    ReplaceLayer {
        remove_id: String,
        introduce_id: String,
    },
    SetGain {
        layer_id: String,
        to: f32,
        over_beats: f32,
    },
    /// Several controls move on one shared phrase-time ramp.
    SetGains {
        targets: Vec<LayerGainTarget>,
        over_beats: f32,
    },
    LoopLayer {
        layer_id: String,
        start_s: f64,
        end_s: f64,
    },
    ReturnToOriginal {
        source_id: String,
        over_beats: f32,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PerformanceEdge {
    pub from: String,
    pub to: String,
    pub at_beat: f64,
    pub operation: ArrangementOperation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerGainTarget {
    pub layer_id: String,
    pub to: f32,
}

/// Performance-clock tempo. Segments start at beat zero, are contiguous, and
/// the final segment may be open-ended. Beat positions remain the musical
/// authority; this map converts them to deterministic wall-clock time.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PerformanceTempoSegment {
    pub start_beat: f64,
    #[serde(default)]
    pub end_beat: Option<f64>,
    pub bpm: f64,
}

/// A directed graph supports a committed short horizon and alternative
/// farther-future arrangements without requiring every set to be a mashup.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PerformanceGraph {
    pub entry: String,
    /// Required for executable timing; an absent legacy/default map fails
    /// validation instead of assuming an arbitrary tempo.
    #[serde(default)]
    pub tempo: Vec<PerformanceTempoSegment>,
    pub sources: Vec<PerformanceSource>,
    pub states: Vec<PerformanceState>,
    pub edges: Vec<PerformanceEdge>,
}

impl PerformanceGraph {
    /// A real arrangement: material from a third track, or stem layers.
    /// A plain two-track original-mix handover is not one; it belongs on
    /// the rehearsed conventional transition path.
    pub fn is_layered_arrangement(&self) -> bool {
        self.sources.len() > 2
            || self.states.iter().any(|state| {
                state
                    .layers
                    .iter()
                    .any(|layer| matches!(layer.material, MaterialKind::Stem(_)))
            })
    }

    /// Whether layered arrangements may execute. They can leave the music on
    /// decks C/D and have not yet passed listening validation, so they stay
    /// opt-in: `PATCH_EXPERIMENTAL_ARRANGEMENTS=1`.
    pub fn execution_enabled() -> bool {
        std::env::var_os("PATCH_EXPERIMENTAL_ARRANGEMENTS").is_some_and(|v| v == "1")
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.tempo.is_empty() {
            return Err("performance is missing its tempo map".into());
        }
        let mut expected_start = 0.0;
        for (index, segment) in self.tempo.iter().enumerate() {
            if !segment.start_beat.is_finite()
                || (segment.start_beat - expected_start).abs() > 1e-6
                || !segment.bpm.is_finite()
                || !(40.0..=300.0).contains(&segment.bpm)
            {
                return Err("invalid or discontinuous performance tempo map".into());
            }
            match segment.end_beat {
                Some(end) if end.is_finite() && end > segment.start_beat => {
                    expected_start = end;
                    if index + 1 == self.tempo.len() {
                        return Err("last performance tempo segment must be open-ended".into());
                    }
                }
                None if index + 1 == self.tempo.len() => {}
                _ => return Err("invalid performance tempo segment boundary".into()),
            }
        }
        if self.sources.is_empty() || self.sources.len() > MAX_PERFORMANCE_SOURCES {
            return Err(format!(
                "performance needs 1–{MAX_PERFORMANCE_SOURCES} source tracks"
            ));
        }
        let mut sources = BTreeMap::new();
        for source in &self.sources {
            if !valid_text_id(&source.id)
                || source
                    .content_hash
                    .as_ref()
                    .is_some_and(|hash| hash.trim().is_empty())
                || !source.duration_s.is_finite()
                || source.duration_s <= 0.0
                || sources.insert(source.id.as_str(), source).is_some()
            {
                return Err("invalid or duplicate performance source".into());
            }
        }

        let mut states = BTreeMap::new();
        let mut used_sources = BTreeSet::new();
        for state in &self.states {
            if !valid_text_id(&state.id)
                || !state.at_beat.is_finite()
                || state.at_beat < 0.0
                || state.layers.len() > MAX_ACTIVE_LAYERS
                || states.insert(state.id.as_str(), state).is_some()
            {
                return Err("invalid or duplicate performance state".into());
            }
            let mut layer_ids = BTreeSet::new();
            let mut source_materials: BTreeMap<&str, Vec<&ActiveLayer>> = BTreeMap::new();
            for layer in &state.layers {
                let source = sources
                    .get(layer.source_id.as_str())
                    .ok_or_else(|| format!("layer {} references an unknown source", layer.id))?;
                used_sources.insert(layer.source_id.as_str());
                if !valid_text_id(&layer.id)
                    || !layer_ids.insert(layer.id.as_str())
                    || !layer.source_start_s.is_finite()
                    || !layer.source_end_s.is_finite()
                    || layer.source_start_s < 0.0
                    || layer.source_end_s <= layer.source_start_s
                    || layer.source_end_s > source.duration_s
                    || !layer.gain.is_finite()
                    || !(0.0..=2.0).contains(&layer.gain)
                {
                    return Err(format!("invalid performance layer {}", layer.id));
                }
                if let MaterialKind::Stem(id) = &layer.material
                    && !crate::stems::valid_id(id)
                {
                    return Err(format!("invalid stem id in layer {}", layer.id));
                }
                match &layer.material {
                    MaterialKind::OriginalMix if layer.stem_set_id.is_some() => {
                        return Err(format!(
                            "original layer {} cannot reference a StemSet",
                            layer.id
                        ));
                    }
                    MaterialKind::Stem(_)
                        if layer.stem_set_id.as_deref().is_none_or(str::is_empty) =>
                    {
                        return Err(format!(
                            "stem layer {} is missing its StemSet identity",
                            layer.id
                        ));
                    }
                    _ => {}
                }
                let materials = source_materials
                    .entry(layer.source_id.as_str())
                    .or_default();
                let same_range = materials.iter().all(|previous| {
                    previous.source_start_s == layer.source_start_s
                        && previous.source_end_s == layer.source_end_s
                });
                if !same_range {
                    return Err(format!(
                        "all material from source {} must share one transport range",
                        layer.source_id
                    ));
                }
                match &layer.material {
                    MaterialKind::OriginalMix => {
                        if materials
                            .iter()
                            .any(|previous| matches!(previous.material, MaterialKind::OriginalMix))
                        {
                            return Err(format!(
                                "source {} has multiple original-mix layers in state {}",
                                layer.source_id, state.id
                            ));
                        }
                    }
                    MaterialKind::Stem(stem) => {
                        if materials.iter().any(|previous| {
                            matches!(&previous.material, MaterialKind::Stem(other) if other == stem)
                        }) {
                            return Err(format!(
                                "source {} repeats stem {stem} in state {}",
                                layer.source_id, state.id
                            ));
                        }
                        if materials.iter().any(|previous| {
                            matches!(previous.material, MaterialKind::Stem(_))
                                && previous.stem_set_id != layer.stem_set_id
                        }) {
                            return Err(format!(
                                "stem layers from source {} must use one StemSet",
                                layer.source_id
                            ));
                        }
                    }
                }
                materials.push(layer);
            }
            for (source_id, materials) in source_materials {
                let original_audible = materials.iter().any(|layer| {
                    matches!(layer.material, MaterialKind::OriginalMix) && layer.gain > 0.0
                });
                let stem_audible = materials.iter().any(|layer| {
                    matches!(layer.material, MaterialKind::Stem(_)) && layer.gain > 0.0
                });
                if original_audible && stem_audible {
                    return Err(format!(
                        "source {source_id} cannot sum its original mix with separated stems"
                    ));
                }
            }
        }
        if !states.contains_key(self.entry.as_str()) {
            return Err("performance entry state does not exist".into());
        }

        for edge in &self.edges {
            let from = states
                .get(edge.from.as_str())
                .ok_or_else(|| format!("edge references unknown state {}", edge.from))?;
            let to = states
                .get(edge.to.as_str())
                .ok_or_else(|| format!("edge references unknown state {}", edge.to))?;
            if !edge.at_beat.is_finite()
                || edge.at_beat < from.at_beat
                || edge.at_beat > to.at_beat
                || to.at_beat < from.at_beat
            {
                return Err("performance edge has invalid or backward timing".into());
            }
            let ramp_beats = match &edge.operation {
                ArrangementOperation::SetGain { over_beats, .. }
                | ArrangementOperation::SetGains { over_beats, .. }
                | ArrangementOperation::ReturnToOriginal { over_beats, .. } => {
                    Some(f64::from(*over_beats))
                }
                _ => None,
            };
            if ramp_beats.is_some_and(|beats| edge.at_beat + beats > to.at_beat + 1e-6) {
                return Err("gain ramp extends beyond its target state".into());
            }
            validate_operation(&edge.operation, from, to, &sources)?;
        }
        let mut reachable = BTreeSet::new();
        let mut stack = vec![self.entry.as_str()];
        while let Some(id) = stack.pop() {
            if reachable.insert(id) {
                stack.extend(
                    self.edges
                        .iter()
                        .filter(|edge| edge.from == id)
                        .map(|edge| edge.to.as_str()),
                );
            }
        }
        if reachable.len() != states.len() {
            return Err("performance graph contains unreachable states".into());
        }
        if used_sources.len() != sources.len() {
            return Err("performance declares a source that no active layer uses".into());
        }
        let mut indegree: BTreeMap<&str, usize> = states.keys().map(|id| (*id, 0usize)).collect();
        for edge in &self.edges {
            *indegree.get_mut(edge.to.as_str()).unwrap() += 1;
        }
        let mut roots: Vec<&str> = indegree
            .iter()
            .filter_map(|(id, degree)| (*degree == 0).then_some(*id))
            .collect();
        let mut visited = 0;
        while let Some(id) = roots.pop() {
            visited += 1;
            for edge in self.edges.iter().filter(|edge| edge.from == id) {
                let degree = indegree.get_mut(edge.to.as_str()).unwrap();
                *degree -= 1;
                if *degree == 0 {
                    roots.push(edge.to.as_str());
                }
            }
        }
        if visited != states.len() {
            return Err(
                "performance graph must be acyclic; model loops as timed operations".into(),
            );
        }
        Ok(())
    }

    /// Convert a performance beat to seconds by integrating the tempo map.
    pub fn seconds_at_beat(&self, beat: f64) -> Option<f64> {
        if !beat.is_finite() || beat < 0.0 || self.tempo.is_empty() {
            return None;
        }
        let mut seconds = 0.0;
        for segment in &self.tempo {
            if beat <= segment.start_beat {
                return Some(seconds);
            }
            let end = segment.end_beat.unwrap_or(beat).min(beat);
            seconds += (end - segment.start_beat) * 60.0 / segment.bpm;
            if segment
                .end_beat
                .is_none_or(|segment_end| beat <= segment_end)
            {
                return seconds.is_finite().then_some(seconds);
            }
        }
        None
    }

    /// Compile a selected, linear graph path to sample-clock events. This is
    /// control-plane work; the RT callback receives only the bounded result.
    pub fn compile(&self, sample_rate: u32) -> Result<PreparedPerformanceSchedule, String> {
        self.validate_prepared()?;
        if !(8_000..=384_000).contains(&sample_rate) {
            return Err("unsupported performance sample rate".into());
        }
        if self.edges.len() > MAX_SCHEDULED_OPERATIONS {
            return Err("performance has too many scheduled operations".into());
        }
        let mut next: BTreeMap<&str, &PerformanceEdge> = BTreeMap::new();
        for edge in &self.edges {
            if next.insert(&edge.from, edge).is_some() {
                return Err("select one arrangement branch before compilation".into());
            }
        }
        let mut events = Vec::with_capacity(self.edges.len());
        let mut current = self.entry.as_str();
        let mut visited = BTreeSet::new();
        while let Some(edge) = next.get(current).copied() {
            if !visited.insert(current) {
                return Err("performance path contains a cycle".into());
            }
            let start_s = self
                .seconds_at_beat(edge.at_beat)
                .ok_or("performance edge is outside its tempo map")?;
            let end_beat = match edge.operation {
                ArrangementOperation::SetGain { over_beats, .. }
                | ArrangementOperation::SetGains { over_beats, .. }
                | ArrangementOperation::ReturnToOriginal { over_beats, .. } => {
                    edge.at_beat + f64::from(over_beats)
                }
                _ => edge.at_beat,
            };
            let end_s = self
                .seconds_at_beat(end_beat)
                .ok_or("performance ramp is outside its tempo map")?;
            events.push(ScheduledPerformanceOperation {
                from: edge.from.clone(),
                to: edge.to.clone(),
                at_beat: edge.at_beat,
                at_sample: seconds_to_sample(start_s, sample_rate)?,
                end_sample: seconds_to_sample(end_s, sample_rate)?,
                operation: edge.operation.clone(),
            });
            current = edge.to.as_str();
        }
        if visited.len() + 1 != self.states.len() {
            return Err("performance graph is not one selected linear path".into());
        }
        // Stable ordering preserves the selected path if adjacent operations
        // quantize to the same sample.
        events.sort_by_key(|event| event.at_sample);
        let duration_s = self
            .states
            .iter()
            .map(|state| self.seconds_at_beat(state.at_beat))
            .collect::<Option<Vec<_>>>()
            .ok_or("performance state is outside its tempo map")?
            .into_iter()
            .fold(0.0f64, f64::max);
        let duration_frames = seconds_to_sample(duration_s, sample_rate)?;
        Ok(PreparedPerformanceSchedule {
            sample_rate,
            duration_frames: events
                .iter()
                .map(|event| event.end_sample)
                .fold(duration_frames, u64::max),
            events,
        })
    }

    /// Execution/preparation must bind every source to its exact audio bytes.
    pub fn validate_prepared(&self) -> Result<(), String> {
        self.validate()?;
        if self.sources.iter().any(|source| {
            source
                .tempo_bpm
                .is_none_or(|bpm| !bpm.is_finite() || !(40.0..=300.0).contains(&bpm))
        }) {
            return Err("executable performance is missing valid source tempo evidence".into());
        }
        if self.tempo.len() != 1 {
            return Err(
                "engine preparation currently requires one constant performance tempo".into(),
            );
        }
        if self.sources.iter().any(|source| {
            source
                .content_hash
                .as_ref()
                .is_none_or(|hash| hash.trim().is_empty())
        }) {
            return Err("prepared performance source is missing a content hash".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScheduledPerformanceOperation {
    pub from: String,
    pub to: String,
    pub at_beat: f64,
    pub at_sample: u64,
    /// Ramp end; instantaneous operations have equal start and end samples.
    pub end_sample: u64,
    pub operation: ArrangementOperation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PreparedPerformanceSchedule {
    pub sample_rate: u32,
    pub duration_frames: u64,
    pub events: Vec<ScheduledPerformanceOperation>,
}

fn seconds_to_sample(seconds: f64, sample_rate: u32) -> Result<u64, String> {
    let frames = seconds * f64::from(sample_rate);
    if !frames.is_finite() || frames < 0.0 || frames > u64::MAX as f64 {
        return Err("performance timing exceeds sample-clock range".into());
    }
    Ok(frames.round() as u64)
}

fn validate_operation(
    operation: &ArrangementOperation,
    from: &PerformanceState,
    to: &PerformanceState,
    sources: &BTreeMap<&str, &PerformanceSource>,
) -> Result<(), String> {
    let has = |state: &PerformanceState, id: &str| state.layers.iter().any(|l| l.id == id);
    match operation {
        ArrangementOperation::IntroduceLayer { layer_id } => {
            let added = to
                .layers
                .iter()
                .filter(|layer| !has(from, &layer.id))
                .count();
            if has(from, layer_id)
                || !has(to, layer_id)
                || added != 1
                || from
                    .layers
                    .iter()
                    .any(|layer| to.layers.iter().find(|next| next.id == layer.id) != Some(layer))
            {
                return Err("introduce operation does not match its state edge".into());
            }
        }
        ArrangementOperation::RemoveLayer { layer_id } => {
            let removed = from
                .layers
                .iter()
                .filter(|layer| !has(to, &layer.id))
                .count();
            if !has(from, layer_id)
                || has(to, layer_id)
                || removed != 1
                || to.layers.iter().any(|layer| {
                    from.layers.iter().find(|previous| previous.id == layer.id) != Some(layer)
                })
            {
                return Err("remove operation does not match its state edge".into());
            }
        }
        ArrangementOperation::ReplaceLayer {
            remove_id,
            introduce_id,
        } => {
            if !has(from, remove_id)
                || has(to, remove_id)
                || has(from, introduce_id)
                || !has(to, introduce_id)
                || from
                    .layers
                    .iter()
                    .filter(|layer| !has(to, &layer.id))
                    .count()
                    != 1
                || to
                    .layers
                    .iter()
                    .filter(|layer| !has(from, &layer.id))
                    .count()
                    != 1
                || from.layers.iter().any(|layer| {
                    layer.id != *remove_id
                        && to.layers.iter().find(|next| next.id == layer.id) != Some(layer)
                })
            {
                return Err("replace operation does not match its state edge".into());
            }
        }
        ArrangementOperation::SetGain {
            layer_id,
            to: gain,
            over_beats,
        } => {
            if !has(from, layer_id)
                || !has(to, layer_id)
                || !gain.is_finite()
                || !(0.0..=2.0).contains(gain)
                || !over_beats.is_finite()
                || *over_beats < 0.0
                || to
                    .layers
                    .iter()
                    .find(|layer| layer.id == *layer_id)
                    .map(|layer| layer.gain)
                    != Some(*gain)
                || from.layers.len() != to.layers.len()
                || from.layers.iter().any(|layer| {
                    to.layers
                        .iter()
                        .find(|next| next.id == layer.id)
                        .is_none_or(|next| {
                            if layer.id == *layer_id {
                                layer.source_id != next.source_id
                                    || layer.material != next.material
                                    || layer.source_start_s != next.source_start_s
                                    || layer.source_end_s != next.source_end_s
                            } else {
                                layer != next
                            }
                        })
                })
            {
                return Err("invalid gain automation".into());
            }
        }
        ArrangementOperation::SetGains {
            targets,
            over_beats,
        } => {
            let target_ids: BTreeSet<_> = targets
                .iter()
                .map(|target| target.layer_id.as_str())
                .collect();
            if targets.is_empty()
                || targets.len() > MAX_ACTIVE_LAYERS
                || target_ids.len() != targets.len()
                || !over_beats.is_finite()
                || *over_beats < 0.0
                || targets.iter().any(|target| {
                    !target.to.is_finite()
                        || !(0.0..=2.0).contains(&target.to)
                        || !has(from, &target.layer_id)
                        || !has(to, &target.layer_id)
                        || to
                            .layers
                            .iter()
                            .find(|layer| layer.id == target.layer_id)
                            .map(|layer| layer.gain)
                            != Some(target.to)
                })
                || from.layers.len() != to.layers.len()
                || from.layers.iter().any(|layer| {
                    to.layers
                        .iter()
                        .find(|next| next.id == layer.id)
                        .is_none_or(|next| {
                            if target_ids.contains(layer.id.as_str()) {
                                layer.source_id != next.source_id
                                    || layer.material != next.material
                                    || layer.source_start_s != next.source_start_s
                                    || layer.source_end_s != next.source_end_s
                                    || layer.stem_set_id != next.stem_set_id
                            } else {
                                layer != next
                            }
                        })
                })
            {
                return Err("invalid simultaneous gain automation".into());
            }
        }
        ArrangementOperation::LoopLayer {
            layer_id,
            start_s,
            end_s,
        } => {
            let layer = to.layers.iter().find(|l| l.id == *layer_id);
            let valid = layer.is_some_and(|layer| {
                has(from, layer_id)
                    && from.layers == to.layers
                    && start_s.is_finite()
                    && end_s.is_finite()
                    && *start_s >= layer.source_start_s
                    && *end_s > *start_s
                    && *end_s <= layer.source_end_s
            });
            if !valid {
                return Err("loop range is outside its active source region".into());
            }
        }
        ArrangementOperation::ReturnToOriginal {
            source_id,
            over_beats,
        } => {
            let from_source: Vec<_> = from
                .layers
                .iter()
                .filter(|layer| layer.source_id == *source_id)
                .collect();
            let to_source: Vec<_> = to
                .layers
                .iter()
                .filter(|layer| layer.source_id == *source_id)
                .collect();
            let original = to_source
                .iter()
                .find(|layer| layer.material == MaterialKind::OriginalMix);
            let valid = sources.contains_key(source_id.as_str())
                && over_beats.is_finite()
                && *over_beats >= 0.0
                && !from_source.is_empty()
                && from_source
                    .iter()
                    .any(|layer| matches!(layer.material, MaterialKind::Stem(_)))
                && original.is_some_and(|layer| (layer.gain - 1.0).abs() <= f32::EPSILON)
                && from_source.len() == to_source.len()
                && from_source.iter().all(|previous| {
                    to_source
                        .iter()
                        .find(|next| next.id == previous.id)
                        .is_some_and(|next| {
                            previous.source_start_s == next.source_start_s
                                && previous.source_end_s == next.source_end_s
                                && previous.material == next.material
                                && previous.stem_set_id == next.stem_set_id
                                && match next.material {
                                    MaterialKind::OriginalMix => true,
                                    MaterialKind::Stem(_) => next.gain == 0.0,
                                }
                        })
                })
                && from
                    .layers
                    .iter()
                    .filter(|layer| layer.source_id != *source_id)
                    .all(|previous| {
                        to.layers.iter().find(|next| next.id == previous.id) == Some(previous)
                    })
                && to
                    .layers
                    .iter()
                    .filter(|layer| layer.source_id != *source_id)
                    .all(|next| {
                        from.layers.iter().find(|previous| previous.id == next.id) == Some(next)
                    });
            if !valid {
                return Err("return-to-original must fade this source's stems to zero and restore its original mix".into());
            }
        }
    }
    Ok(())
}

fn valid_text_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(id: &str, source_id: &str, stem: &str) -> ActiveLayer {
        ActiveLayer {
            id: id.into(),
            source_id: source_id.into(),
            material: MaterialKind::Stem(stem.into()),
            stem_set_id: Some(format!("set-{source_id}")),
            source_start_s: 8.0,
            source_end_s: 40.0,
            gain: 1.0,
        }
    }

    fn tempo() -> Vec<PerformanceTempoSegment> {
        vec![PerformanceTempoSegment {
            start_beat: 0.0,
            end_beat: None,
            bpm: 120.0,
        }]
    }

    #[test]
    fn graph_can_describe_a_three_track_mashup_without_making_stems_primary() {
        let drums = layer("a-drums", "track-a", "drums");
        let vocals = layer("b-vocals", "track-b", "vocals");
        let bass = layer("c-bass", "track-c", "bass");
        let graph = PerformanceGraph {
            entry: "drums".into(),
            tempo: tempo(),
            sources: ["a", "b", "c"]
                .into_iter()
                .map(|id| PerformanceSource {
                    id: format!("track-{id}"),
                    content_hash: None,
                    tempo_bpm: Some(120.0),
                    duration_s: 240.0,
                })
                .collect(),
            states: vec![
                PerformanceState {
                    id: "drums".into(),
                    at_beat: 0.0,
                    layers: vec![drums.clone()],
                },
                PerformanceState {
                    id: "vocal".into(),
                    at_beat: 16.0,
                    layers: vec![drums.clone(), vocals.clone()],
                },
                PerformanceState {
                    id: "mashup".into(),
                    at_beat: 32.0,
                    layers: vec![drums, vocals, bass],
                },
            ],
            edges: vec![
                PerformanceEdge {
                    from: "drums".into(),
                    to: "vocal".into(),
                    at_beat: 16.0,
                    operation: ArrangementOperation::IntroduceLayer {
                        layer_id: "b-vocals".into(),
                    },
                },
                PerformanceEdge {
                    from: "vocal".into(),
                    to: "mashup".into(),
                    at_beat: 32.0,
                    operation: ArrangementOperation::IntroduceLayer {
                        layer_id: "c-bass".into(),
                    },
                },
            ],
        };
        graph.validate().unwrap();
        let mut unbound_stem = graph.clone();
        unbound_stem.states[1].layers[1].stem_set_id = None;
        assert!(
            unbound_stem
                .validate()
                .unwrap_err()
                .contains("StemSet identity")
        );
        let mut original_with_stem_cache = graph.clone();
        original_with_stem_cache.states[0].layers[0].material = MaterialKind::OriginalMix;
        original_with_stem_cache.states[0].layers[0].stem_set_id = Some("wrong-kind".into());
        assert!(
            original_with_stem_cache
                .validate()
                .unwrap_err()
                .contains("cannot reference a StemSet")
        );
        assert!(graph.validate_prepared().is_err());
        let mut prepared = graph.clone();
        for source in &mut prepared.sources {
            source.content_hash = Some(format!("content: {}", source.id));
        }
        prepared.validate_prepared().unwrap();
        let schedule = prepared.compile(48_000).unwrap();
        assert_eq!(schedule.events.len(), 2);
        assert_eq!(schedule.events[0].at_sample, 384_000);
        assert_eq!(schedule.duration_frames, 768_000);
    }

    #[test]
    fn one_source_transport_cannot_be_split_into_independent_decks_or_ranges() {
        let drums = layer("drums", "track-a", "drums");
        let vocals = layer("vocals", "track-a", "vocals");
        let base = PerformanceGraph {
            entry: "mix".into(),
            tempo: tempo(),
            sources: vec![PerformanceSource {
                id: "track-a".into(),
                content_hash: None,
                tempo_bpm: Some(120.0),
                duration_s: 60.0,
            }],
            states: vec![PerformanceState {
                id: "mix".into(),
                at_beat: 0.0,
                layers: vec![drums.clone(), vocals.clone()],
            }],
            edges: vec![],
        };
        assert!(
            base.validate().is_ok(),
            "multiple aligned stems share a deck"
        );

        let mut mismatched_range = base.clone();
        mismatched_range.states[0].layers[1].source_start_s += 1.0;
        assert!(
            mismatched_range
                .validate()
                .unwrap_err()
                .contains("share one transport range")
        );

        let mut mismatched_set = base.clone();
        mismatched_set.states[0].layers[1].stem_set_id = Some("other-set".into());
        assert!(
            mismatched_set
                .validate()
                .unwrap_err()
                .contains("one StemSet")
        );

        let mut duplicate_original = base.clone();
        duplicate_original.states[0].layers = vec![
            ActiveLayer {
                id: "original".into(),
                source_id: "track-a".into(),
                material: MaterialKind::OriginalMix,
                stem_set_id: None,
                source_start_s: 8.0,
                source_end_s: 40.0,
                gain: 1.0,
            },
            vocals.clone(),
        ];
        assert!(duplicate_original.validate().is_err());

        let mut switchable = base;
        let original = ActiveLayer {
            id: "original".into(),
            source_id: "track-a".into(),
            material: MaterialKind::OriginalMix,
            stem_set_id: None,
            source_start_s: 8.0,
            source_end_s: 40.0,
            gain: 0.0,
        };
        switchable.states[0].layers = vec![original, drums, vocals];
        assert!(
            switchable.validate().is_ok(),
            "the original and its aligned stems share one track transport"
        );
        switchable.states[0].layers[0].gain = 0.2;
        assert!(
            switchable
                .validate()
                .unwrap_err()
                .contains("cannot sum its original mix")
        );
    }

    #[test]
    fn invalid_edges_cannot_reference_material_not_in_the_target_state() {
        let graph = PerformanceGraph {
            entry: "a".into(),
            tempo: tempo(),
            sources: vec![PerformanceSource {
                id: "a".into(),
                content_hash: Some("hash".into()),
                tempo_bpm: Some(120.0),
                duration_s: 60.0,
            }],
            states: vec![
                PerformanceState {
                    id: "a".into(),
                    at_beat: 0.0,
                    layers: vec![],
                },
                PerformanceState {
                    id: "b".into(),
                    at_beat: 8.0,
                    layers: vec![],
                },
            ],
            edges: vec![PerformanceEdge {
                from: "a".into(),
                to: "b".into(),
                at_beat: 4.0,
                operation: ArrangementOperation::IntroduceLayer {
                    layer_id: "missing".into(),
                },
            }],
        };
        assert!(graph.validate().is_err());
    }

    #[test]
    fn edges_reject_undeclared_layer_and_gain_changes() {
        let source = PerformanceSource {
            id: "track-a".into(),
            content_hash: Some("hash".into()),
            tempo_bpm: Some(120.0),
            duration_s: 60.0,
        };
        let original = ActiveLayer {
            id: "original".into(),
            source_id: "track-a".into(),
            material: MaterialKind::OriginalMix,
            stem_set_id: None,
            source_start_s: 0.0,
            source_end_s: 60.0,
            gain: 1.0,
        };
        let extra = layer("drums", "track-a", "drums");
        let mut graph = PerformanceGraph {
            entry: "a".into(),
            tempo: tempo(),
            sources: vec![source],
            states: vec![
                PerformanceState {
                    id: "a".into(),
                    at_beat: 0.0,
                    layers: vec![original.clone()],
                },
                PerformanceState {
                    id: "b".into(),
                    at_beat: 4.0,
                    layers: vec![
                        ActiveLayer {
                            gain: 0.5,
                            ..original.clone()
                        },
                        extra.clone(),
                    ],
                },
            ],
            edges: vec![PerformanceEdge {
                from: "a".into(),
                to: "b".into(),
                at_beat: 2.0,
                operation: ArrangementOperation::IntroduceLayer {
                    layer_id: "drums".into(),
                },
            }],
        };
        assert!(
            graph.validate().is_err(),
            "introducing drums cannot silently alter original gain"
        );

        graph.states[1].layers = vec![ActiveLayer {
            gain: 0.5,
            ..original.clone()
        }];
        graph.edges[0].operation = ArrangementOperation::SetGain {
            layer_id: "original".into(),
            to: 0.25,
            over_beats: 2.0,
        };
        assert!(
            graph.validate().is_err(),
            "the target gain must equal the scheduled gain"
        );
    }

    #[test]
    fn graph_rejects_unreachable_states_and_cycles() {
        let source = PerformanceSource {
            id: "track-a".into(),
            content_hash: Some("hash".into()),
            tempo_bpm: Some(120.0),
            duration_s: 60.0,
        };
        let mut graph = PerformanceGraph {
            entry: "a".into(),
            tempo: tempo(),
            sources: vec![source],
            states: vec![
                PerformanceState {
                    id: "a".into(),
                    at_beat: 0.0,
                    layers: vec![],
                },
                PerformanceState {
                    id: "b".into(),
                    at_beat: 0.0,
                    layers: vec![],
                },
            ],
            edges: vec![],
        };
        assert!(graph.validate().unwrap_err().contains("unreachable"));
        graph.states.truncate(1);
        graph.states[0].layers.push(ActiveLayer {
            id: "original".into(),
            source_id: "track-a".into(),
            material: MaterialKind::OriginalMix,
            stem_set_id: None,
            source_start_s: 0.0,
            source_end_s: 60.0,
            gain: 1.0,
        });
        graph.edges.push(PerformanceEdge {
            from: "a".into(),
            to: "a".into(),
            at_beat: 0.0,
            operation: ArrangementOperation::SetGain {
                layer_id: "original".into(),
                to: 1.0,
                over_beats: 0.0,
            },
        });
        assert!(graph.validate().unwrap_err().contains("acyclic"));
    }

    #[test]
    fn performance_beat_clock_integrates_piecewise_tempo_deterministically() {
        let graph = PerformanceGraph {
            entry: "state".into(),
            tempo: vec![
                PerformanceTempoSegment {
                    start_beat: 0.0,
                    end_beat: Some(16.0),
                    bpm: 120.0,
                },
                PerformanceTempoSegment {
                    start_beat: 16.0,
                    end_beat: None,
                    bpm: 60.0,
                },
            ],
            sources: vec![PerformanceSource {
                id: "track".into(),
                content_hash: Some("hash".into()),
                tempo_bpm: Some(120.0),
                duration_s: 60.0,
            }],
            states: vec![PerformanceState {
                id: "state".into(),
                at_beat: 0.0,
                layers: vec![ActiveLayer {
                    id: "original".into(),
                    source_id: "track".into(),
                    material: MaterialKind::OriginalMix,
                    stem_set_id: None,
                    source_start_s: 0.0,
                    source_end_s: 60.0,
                    gain: 1.0,
                }],
            }],
            edges: vec![],
        };
        graph.validate().unwrap();
        assert_eq!(graph.seconds_at_beat(8.0), Some(4.0));
        assert_eq!(graph.seconds_at_beat(16.0), Some(8.0));
        assert_eq!(graph.seconds_at_beat(20.0), Some(12.0));
        assert_eq!(graph.seconds_at_beat(-1.0), None);

        let mut invalid = graph;
        invalid.tempo[1].start_beat = 15.0;
        assert!(invalid.validate().unwrap_err().contains("tempo map"));
    }

    #[test]
    fn schedule_compiles_gain_ramps_to_exact_sample_boundaries() {
        let layer = ActiveLayer {
            id: "vocal".into(),
            source_id: "track".into(),
            material: MaterialKind::Stem("vocals".into()),
            stem_set_id: Some("vocal-set".into()),
            source_start_s: 0.0,
            source_end_s: 60.0,
            gain: 0.0,
        };
        let graph = PerformanceGraph {
            entry: "quiet".into(),
            tempo: tempo(),
            sources: vec![PerformanceSource {
                id: "track".into(),
                content_hash: Some("hash".into()),
                tempo_bpm: Some(120.0),
                duration_s: 60.0,
            }],
            states: vec![
                PerformanceState {
                    id: "quiet".into(),
                    at_beat: 0.0,
                    layers: vec![layer.clone()],
                },
                PerformanceState {
                    id: "audible".into(),
                    at_beat: 16.0,
                    layers: vec![ActiveLayer { gain: 1.0, ..layer }],
                },
            ],
            edges: vec![PerformanceEdge {
                from: "quiet".into(),
                to: "audible".into(),
                at_beat: 8.0,
                operation: ArrangementOperation::SetGain {
                    layer_id: "vocal".into(),
                    to: 1.0,
                    over_beats: 8.0,
                },
            }],
        };
        let schedule = graph.compile(48_000).unwrap();
        let event = &schedule.events[0];
        assert_eq!(event.at_sample, 192_000);
        assert_eq!(event.end_sample, 384_000);
    }

    #[test]
    fn return_to_original_is_a_valid_timed_same_transport_switch() {
        let original = ActiveLayer {
            id: "original".into(),
            source_id: "track-a".into(),
            material: MaterialKind::OriginalMix,
            stem_set_id: None,
            source_start_s: 8.0,
            source_end_s: 40.0,
            gain: 0.0,
        };
        let drums = layer("drums", "track-a", "drums");
        let graph = PerformanceGraph {
            entry: "instrumental".into(),
            tempo: tempo(),
            sources: vec![PerformanceSource {
                id: "track-a".into(),
                content_hash: Some("hash".into()),
                tempo_bpm: Some(120.0),
                duration_s: 60.0,
            }],
            states: vec![
                PerformanceState {
                    id: "instrumental".into(),
                    at_beat: 0.0,
                    layers: vec![original.clone(), drums.clone()],
                },
                PerformanceState {
                    id: "original".into(),
                    at_beat: 8.0,
                    layers: vec![
                        ActiveLayer {
                            gain: 1.0,
                            ..original
                        },
                        ActiveLayer { gain: 0.0, ..drums },
                    ],
                },
            ],
            edges: vec![PerformanceEdge {
                from: "instrumental".into(),
                to: "original".into(),
                at_beat: 4.0,
                operation: ArrangementOperation::ReturnToOriginal {
                    source_id: "track-a".into(),
                    over_beats: 4.0,
                },
            }],
        };
        let schedule = graph.compile(48_000).unwrap();
        assert_eq!(schedule.events[0].at_sample, 96_000);
        assert_eq!(schedule.events[0].end_sample, 192_000);
    }
}
