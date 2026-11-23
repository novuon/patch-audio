//! Offline rhythm evaluation against manually annotated, content-addressed
//! tracks. This module runs only in tools/tests, never in the audio callback.

use crate::Analysis;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

pub const CORPUS_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_BEAT_TOLERANCE_MS: f64 = 70.0;
pub const TEMPO_STRENGTH_THRESHOLDS: [f32; 10] = [0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9];

/// Portable annotation manifest for a user-owned or licensed evaluation set.
/// Audio is referenced by path and full SHA-256; it is not copied into Patch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RhythmCorpus {
    pub schema_version: u32,
    pub name: String,
    pub cases: Vec<RhythmAnnotation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RhythmAnnotation {
    pub id: String,
    pub path: String,
    pub sha256: String,
    #[serde(default)]
    pub genre: Option<String>,
    #[serde(default)]
    pub tempo_bpm: Option<f64>,
    /// Ordered beat attack timestamps in source seconds.
    pub beats_s: Vec<f64>,
    /// Ordered downbeat attack timestamps, if annotated.
    #[serde(default)]
    pub downbeats_s: Vec<f64>,
    #[serde(default)]
    pub beats_per_bar: Option<u8>,
}

impl RhythmCorpus {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != CORPUS_SCHEMA_VERSION {
            return Err(format!(
                "unsupported rhythm corpus schema {}, expected {}",
                self.schema_version, CORPUS_SCHEMA_VERSION
            ));
        }
        if self.name.trim().is_empty() {
            return Err("rhythm corpus needs a name".into());
        }
        if self.cases.is_empty() {
            return Err("rhythm corpus has no cases".into());
        }
        let mut ids = std::collections::HashSet::new();
        for case in &self.cases {
            if case.id.trim().is_empty() || !ids.insert(case.id.as_str()) {
                return Err(format!("case ID is empty or duplicated: {:?}", case.id));
            }
            if case.path.trim().is_empty() {
                return Err(format!("case {} has no audio path", case.id));
            }
            if case.sha256.len() != 64 || !case.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!("case {} has an invalid SHA-256", case.id));
            }
            if case.beats_s.is_empty() || !strictly_increasing_finite(&case.beats_s) {
                return Err(format!("case {} needs ordered finite beat times", case.id));
            }
            if !strictly_increasing_finite_or_empty(&case.downbeats_s) {
                return Err(format!("case {} has invalid downbeat times", case.id));
            }
            if case
                .tempo_bpm
                .is_some_and(|bpm| !bpm.is_finite() || !(20.0..=400.0).contains(&bpm))
            {
                return Err(format!("case {} has an invalid reference tempo", case.id));
            }
            if case
                .beats_per_bar
                .is_some_and(|meter| !(2..=12).contains(&meter))
            {
                return Err(format!("case {} has an invalid meter", case.id));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimeMatchMetrics {
    pub true_positives: usize,
    pub false_positives: usize,
    pub false_negatives: usize,
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
    pub mean_absolute_error_ms: Option<f64>,
    pub p95_absolute_error_ms: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PhaseDriftMetric {
    pub beat_span: usize,
    /// Endpoint phase error after removing the initial alignment offset.
    pub endpoint_drift_ms: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RhythmMetrics {
    pub case_id: String,
    pub source_sha256: String,
    pub genre: Option<String>,
    pub beat_analyzer_version: u32,
    pub rhythm_analyzer_version: Option<u32>,
    pub tempo_abs_error_bpm: Option<f64>,
    /// Raw, uncalibrated tempo selection strength from the current beat scan.
    pub tempo_selection_strength: Option<f32>,
    /// Whether the selected tempo is within max(0.5 BPM, 2%) of annotation.
    pub tempo_correct: Option<bool>,
    pub beats: TimeMatchMetrics,
    pub downbeats: Option<TimeMatchMetrics>,
    pub phase_drift: Vec<PhaseDriftMetric>,
    /// One-based position of the annotated meter among current raw candidates.
    /// Raw ranking is reported, not presented as confidence.
    pub meter_rank: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AggregateTimeMetrics {
    pub true_positives: usize,
    pub false_positives: usize,
    pub false_negatives: usize,
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
    /// Pooled by the number of matched events across all cases.
    pub matched_mean_absolute_error_ms: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AggregateDriftMetrics {
    pub sample_count: usize,
    pub mean_absolute_drift_ms: f64,
    pub p95_absolute_drift_ms: f64,
    pub max_absolute_drift_ms: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RhythmAggregate {
    pub case_count: usize,
    pub beat: AggregateTimeMetrics,
    pub downbeat: Option<AggregateTimeMetrics>,
    pub macro_mean_case_beat_f1: f64,
    pub macro_mean_case_downbeat_f1: Option<f64>,
    pub mean_tempo_abs_error_bpm: Option<f64>,
    /// Accuracy/coverage as increasingly selective thresholds are applied to
    /// raw tempo-selection strength. This does not imply calibration.
    pub tempo_selectivity: Vec<TempoSelectivityPoint>,
    pub phase_drift_by_span: BTreeMap<usize, AggregateDriftMetrics>,
    /// Evaluated only for cases with both a meter annotation and a current
    /// meter candidate list.
    pub meter_ranked_case_count: usize,
    pub meter_rank1_fraction: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TempoSelectivityPoint {
    pub minimum_raw_strength: f32,
    pub annotated_cases: usize,
    pub selected_cases: usize,
    pub correct_selected_cases: usize,
    pub coverage: f64,
    pub accuracy: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RhythmEvaluationSummary {
    pub overall: RhythmAggregate,
    pub per_genre: BTreeMap<String, RhythmAggregate>,
}

pub fn summarize_rhythm(cases: &[RhythmMetrics]) -> RhythmEvaluationSummary {
    let mut groups: BTreeMap<String, Vec<&RhythmMetrics>> = BTreeMap::new();
    for case in cases {
        groups
            .entry(case.genre.clone().unwrap_or_else(|| "unspecified".into()))
            .or_default()
            .push(case);
    }
    RhythmEvaluationSummary {
        overall: aggregate_group(cases.iter().collect()),
        per_genre: groups
            .into_iter()
            .map(|(genre, cases)| (genre, aggregate_group(cases)))
            .collect(),
    }
}

fn aggregate_group(cases: Vec<&RhythmMetrics>) -> RhythmAggregate {
    let beat = aggregate_times(cases.iter().map(|case| &case.beats).collect());
    let downbeat_cases = cases
        .iter()
        .filter_map(|case| case.downbeats.as_ref())
        .collect::<Vec<_>>();
    let downbeat = (!downbeat_cases.is_empty()).then(|| aggregate_times(downbeat_cases.clone()));
    let tempo_errors = cases
        .iter()
        .filter_map(|case| case.tempo_abs_error_bpm)
        .collect::<Vec<_>>();
    let tempo_annotated = cases
        .iter()
        .filter(|case| case.tempo_correct.is_some())
        .collect::<Vec<_>>();
    let tempo_selectivity = TEMPO_STRENGTH_THRESHOLDS
        .into_iter()
        .map(|minimum_raw_strength| {
            let selected = tempo_annotated
                .iter()
                .filter(|case| {
                    case.tempo_selection_strength
                        .is_some_and(|score| score >= minimum_raw_strength)
                })
                .collect::<Vec<_>>();
            let correct_selected_cases = selected
                .iter()
                .filter(|case| case.tempo_correct == Some(true))
                .count();
            TempoSelectivityPoint {
                minimum_raw_strength,
                annotated_cases: tempo_annotated.len(),
                selected_cases: selected.len(),
                correct_selected_cases,
                coverage: ratio(selected.len(), tempo_annotated.len()),
                accuracy: (!selected.is_empty())
                    .then(|| ratio(correct_selected_cases, selected.len())),
            }
        })
        .collect();
    let meter_ranks = cases
        .iter()
        .filter_map(|case| case.meter_rank)
        .collect::<Vec<_>>();
    let mut drift_by_span: BTreeMap<usize, Vec<f64>> = BTreeMap::new();
    for drift in cases.iter().flat_map(|case| &case.phase_drift) {
        drift_by_span
            .entry(drift.beat_span)
            .or_default()
            .push(drift.endpoint_drift_ms.abs());
    }
    let phase_drift_by_span = drift_by_span
        .into_iter()
        .map(|(span, mut values)| {
            values.sort_by(f64::total_cmp);
            let sample_count = values.len();
            let metrics = AggregateDriftMetrics {
                sample_count,
                mean_absolute_drift_ms: values.iter().sum::<f64>() / sample_count as f64,
                p95_absolute_drift_ms: percentile(&values, 95),
                max_absolute_drift_ms: values.last().copied().unwrap_or_default(),
            };
            (span, metrics)
        })
        .collect();
    RhythmAggregate {
        case_count: cases.len(),
        beat,
        downbeat,
        macro_mean_case_beat_f1: if cases.is_empty() {
            0.0
        } else {
            cases.iter().map(|case| case.beats.f1).sum::<f64>() / cases.len() as f64
        },
        macro_mean_case_downbeat_f1: (!downbeat_cases.is_empty()).then(|| {
            downbeat_cases.iter().map(|case| case.f1).sum::<f64>() / downbeat_cases.len() as f64
        }),
        mean_tempo_abs_error_bpm: (!tempo_errors.is_empty())
            .then(|| tempo_errors.iter().sum::<f64>() / tempo_errors.len() as f64),
        tempo_selectivity,
        phase_drift_by_span,
        meter_ranked_case_count: meter_ranks.len(),
        meter_rank1_fraction: (!meter_ranks.is_empty()).then(|| {
            ratio(
                meter_ranks.iter().filter(|rank| **rank == 1).count(),
                meter_ranks.len(),
            )
        }),
    }
}

fn aggregate_times(cases: Vec<&TimeMatchMetrics>) -> AggregateTimeMetrics {
    let true_positives = cases.iter().map(|case| case.true_positives).sum::<usize>();
    let false_positives = cases.iter().map(|case| case.false_positives).sum::<usize>();
    let false_negatives = cases.iter().map(|case| case.false_negatives).sum::<usize>();
    let precision = ratio(true_positives, true_positives + false_positives);
    let recall = ratio(true_positives, true_positives + false_negatives);
    let f1 = if precision + recall > 0.0 {
        2.0 * precision * recall / (precision + recall)
    } else {
        0.0
    };
    let error_sum = cases
        .iter()
        .filter_map(|case| {
            case.mean_absolute_error_ms
                .map(|error| error * case.true_positives as f64)
        })
        .sum::<f64>();
    AggregateTimeMetrics {
        true_positives,
        false_positives,
        false_negatives,
        precision,
        recall,
        f1,
        matched_mean_absolute_error_ms: (true_positives > 0)
            .then(|| error_sum / true_positives as f64),
    }
}

fn percentile(sorted_values: &[f64], percentile: usize) -> f64 {
    if sorted_values.is_empty() {
        0.0
    } else {
        let rank = percentile.clamp(1, 100);
        let index = (sorted_values.len() * rank).div_ceil(100).saturating_sub(1);
        sorted_values[index.min(sorted_values.len() - 1)]
    }
}

pub fn evaluate_rhythm(
    analysis: &Analysis,
    annotation: &RhythmAnnotation,
    beat_tolerance_ms: f64,
) -> RhythmMetrics {
    let predictions = analysis
        .rhythm
        .as_ref()
        .filter(|e| e.analyzer_version == crate::tempo::RHYTHM_EVIDENCE_VERSION)
        .map(|e| e.beats.iter().map(|beat| beat.time_s).collect::<Vec<_>>())
        .unwrap_or_default();
    let predicted_downbeats = analysis
        .rhythm
        .as_ref()
        .filter(|e| e.analyzer_version == crate::tempo::RHYTHM_EVIDENCE_VERSION)
        .map(|e| {
            e.beats
                .iter()
                .filter(|beat| beat.beat_in_bar == Some(0))
                .map(|beat| beat.time_s)
                .collect::<Vec<_>>()
        });
    let tolerance_ms = if beat_tolerance_ms.is_finite() {
        beat_tolerance_ms
    } else {
        DEFAULT_BEAT_TOLERANCE_MS
    };
    let tolerance_s = (tolerance_ms / 1000.0).clamp(0.001, 0.5);
    let meter_rank = annotation.beats_per_bar.and_then(|meter| {
        analysis
            .rhythm
            .as_ref()
            .filter(|e| e.analyzer_version == crate::tempo::RHYTHM_EVIDENCE_VERSION)?
            .meter_hypotheses
            .iter()
            .position(|hypothesis| hypothesis.beats_per_bar == meter)
            .map(|rank| rank + 1)
    });
    let tempo_current = analysis.analyzers.beat == crate::BEAT_ANALYZER_VERSION;
    let tempo_abs_error_bpm = tempo_current
        .then(|| annotation.tempo_bpm.zip(analysis.bpm))
        .flatten()
        .map(|(expected, actual)| (expected - actual).abs());
    let tempo_selection_strength = tempo_current
        .then(|| analysis.rhythm.as_ref()?.selection_strength)
        .flatten();
    let tempo_correct = annotation
        .tempo_bpm
        .zip(tempo_abs_error_bpm)
        .map(|(expected, error)| error <= (expected * 0.02).max(0.5));
    RhythmMetrics {
        case_id: annotation.id.clone(),
        source_sha256: annotation.sha256.to_ascii_lowercase(),
        genre: annotation.genre.clone(),
        beat_analyzer_version: analysis.analyzers.beat,
        rhythm_analyzer_version: analysis
            .rhythm
            .as_ref()
            .map(|rhythm| rhythm.analyzer_version),
        tempo_abs_error_bpm,
        tempo_selection_strength,
        tempo_correct,
        beats: match_times(&annotation.beats_s, &predictions, tolerance_s),
        downbeats: (!annotation.downbeats_s.is_empty()).then(|| {
            match_times(
                &annotation.downbeats_s,
                predicted_downbeats.as_deref().unwrap_or_default(),
                tolerance_s,
            )
        }),
        phase_drift: phase_drift(&annotation.beats_s, &predictions),
        meter_rank,
    }
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let bytes = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if bytes == 0 {
            break;
        }
        hasher.update(&buffer[..bytes]);
    }
    let digest = hasher.finalize();
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn match_times(expected: &[f64], predicted: &[f64], tolerance_s: f64) -> TimeMatchMetrics {
    let (mut i, mut j) = (0, 0);
    let mut errors = Vec::new();
    while i < expected.len() && j < predicted.len() {
        let delta = predicted[j] - expected[i];
        if delta.abs() <= tolerance_s {
            errors.push(delta.abs() * 1000.0);
            i += 1;
            j += 1;
        } else if delta < 0.0 {
            j += 1;
        } else {
            i += 1;
        }
    }
    let tp = errors.len();
    let fp = predicted.len().saturating_sub(tp);
    let fn_ = expected.len().saturating_sub(tp);
    let precision = ratio(tp, tp + fp);
    let recall = ratio(tp, tp + fn_);
    let f1 = if precision + recall > 0.0 {
        2.0 * precision * recall / (precision + recall)
    } else {
        0.0
    };
    let mut sorted = errors.clone();
    sorted.sort_by(f64::total_cmp);
    TimeMatchMetrics {
        true_positives: tp,
        false_positives: fp,
        false_negatives: fn_,
        precision,
        recall,
        f1,
        mean_absolute_error_ms: (!errors.is_empty())
            .then(|| errors.iter().sum::<f64>() / errors.len() as f64),
        p95_absolute_error_ms: sorted
            .get((sorted.len() * 95 / 100).saturating_sub(1))
            .copied(),
    }
}

fn phase_drift(expected: &[f64], predicted: &[f64]) -> Vec<PhaseDriftMetric> {
    if expected.is_empty() || predicted.is_empty() {
        return Vec::new();
    }
    let Some(anchor) = nearest(predicted, expected[0]) else {
        return Vec::new();
    };
    [16, 32, 64, 128]
        .into_iter()
        .filter_map(|span| {
            let truth = *expected.get(span)?;
            let estimate = nearest(predicted, truth)?;
            Some(PhaseDriftMetric {
                beat_span: span,
                endpoint_drift_ms: ((estimate - truth) - (anchor - expected[0])) * 1000.0,
            })
        })
        .collect()
}

fn nearest(times: &[f64], target: f64) -> Option<f64> {
    times
        .iter()
        .copied()
        .filter(|time| time.is_finite())
        .min_by(|a, b| (a - target).abs().total_cmp(&(b - target).abs()))
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn strictly_increasing_finite(values: &[f64]) -> bool {
    !values.is_empty()
        && values
            .iter()
            .all(|value| value.is_finite() && *value >= 0.0)
        && values.windows(2).all(|pair| pair[1] > pair[0])
}

fn strictly_increasing_finite_or_empty(values: &[f64]) -> bool {
    values.is_empty() || strictly_increasing_finite(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn annotation(beats_s: Vec<f64>) -> RhythmAnnotation {
        RhythmAnnotation {
            id: "fixture".into(),
            path: "fixture.wav".into(),
            sha256: "a".repeat(64),
            genre: Some("test".into()),
            tempo_bpm: Some(120.0),
            beats_s,
            downbeats_s: vec![],
            beats_per_bar: Some(4),
        }
    }

    #[test]
    fn ordered_tolerance_matching_is_one_to_one_and_reports_phase_drift() {
        let mut analysis = crate::analyse(&vec![0.0; 44_100 * 2]);
        let beats = (0..130)
            .map(|index| crate::tempo::BeatEvent {
                time_s: index as f64 * 0.5 + index as f64 * 0.0005,
                beat_index: index,
                interval_s: 0.5005,
                bar_index: Some(index.div_euclid(4)),
                beat_in_bar: Some(index.rem_euclid(4) as u8),
                onset_offset_s: None,
                onset_support: 0.0,
            })
            .collect();
        analysis.bpm = Some(120.0);
        analysis.bpm_confidence = 0.82;
        analysis.rhythm = Some(crate::tempo::RhythmEvidence {
            analyzer_version: crate::tempo::RHYTHM_EVIDENCE_VERSION,
            beats,
            regions: vec![],
            tempo_hypotheses: vec![],
            meter_hypotheses: vec![crate::tempo::MeterHypothesis {
                beats_per_bar: 4,
                beat_unit: 4,
                phase: 0,
                raw_score: 0.7,
                supporting_beats: 130,
            }],
            selection_strength: Some(0.82),
        });
        let expected = annotation((0..130).map(|index| index as f64 * 0.5).collect());
        let metrics = evaluate_rhythm(&analysis, &expected, DEFAULT_BEAT_TOLERANCE_MS);
        assert_eq!(metrics.beats.true_positives, 130);
        assert_eq!(metrics.beats.false_positives, 0);
        assert_eq!(metrics.beats.false_negatives, 0);
        assert!((metrics.beats.mean_absolute_error_ms.unwrap() - 32.25).abs() < 1.0);
        assert_eq!(metrics.phase_drift.len(), 4);
        assert!(metrics.phase_drift[3].endpoint_drift_ms.abs() > 60.0);
        assert_eq!(metrics.meter_rank, Some(1));
        assert_eq!(metrics.tempo_selection_strength, Some(0.82));
        assert_eq!(metrics.tempo_correct, Some(true));
        assert_eq!(metrics.source_sha256, "a".repeat(64));

        analysis.analyzers.beat -= 1;
        let stale = evaluate_rhythm(&analysis, &expected, DEFAULT_BEAT_TOLERANCE_MS);
        assert_eq!(stale.tempo_selection_strength, None);
        assert_eq!(stale.tempo_abs_error_bpm, None);
    }

    #[test]
    fn corpus_validation_rejects_duplicate_ids_and_bad_timestamps() {
        let mut corpus = RhythmCorpus {
            schema_version: CORPUS_SCHEMA_VERSION,
            name: "dj torture test".into(),
            cases: vec![annotation(vec![0.0, 0.5]), annotation(vec![0.0, 0.5])],
        };
        assert!(corpus.validate().unwrap_err().contains("duplicated"));
        corpus.cases.pop();
        corpus.cases[0].beats_s = vec![0.0, f64::NAN];
        assert!(corpus.validate().unwrap_err().contains("ordered finite"));
    }

    #[test]
    fn summary_reports_micro_and_per_genre_quality_without_hiding_weak_domains() {
        let make_case = |id: &str, genre: &str, tp, fp, fn_| {
            let (db_tp, db_fp, db_fn) = (tp / 4, fp / 4, fn_ / 4);
            let (db_precision, db_recall) =
                (ratio(db_tp, db_tp + db_fp), ratio(db_tp, db_tp + db_fn));
            RhythmMetrics {
                case_id: id.into(),
                source_sha256: "a".repeat(64),
                genre: Some(genre.into()),
                beat_analyzer_version: crate::BEAT_ANALYZER_VERSION,
                rhythm_analyzer_version: Some(crate::tempo::RHYTHM_EVIDENCE_VERSION),
                tempo_abs_error_bpm: Some(1.0),
                tempo_selection_strength: Some(if genre == "live" { 0.3 } else { 0.8 }),
                tempo_correct: Some(genre != "live"),
                beats: TimeMatchMetrics {
                    true_positives: tp,
                    false_positives: fp,
                    false_negatives: fn_,
                    precision: ratio(tp, tp + fp),
                    recall: ratio(tp, tp + fn_),
                    f1: 0.0,
                    mean_absolute_error_ms: Some(4.0),
                    p95_absolute_error_ms: Some(8.0),
                },
                downbeats: Some(TimeMatchMetrics {
                    true_positives: db_tp,
                    false_positives: db_fp,
                    false_negatives: db_fn,
                    precision: db_precision,
                    recall: db_recall,
                    f1: if db_precision + db_recall > 0.0 {
                        2.0 * db_precision * db_recall / (db_precision + db_recall)
                    } else {
                        0.0
                    },
                    mean_absolute_error_ms: Some(5.0),
                    p95_absolute_error_ms: Some(9.0),
                }),
                phase_drift: vec![PhaseDriftMetric {
                    beat_span: 16,
                    endpoint_drift_ms: if genre == "live" { 120.0 } else { -18.0 },
                }],
                meter_rank: Some(1),
            }
        };
        let mut weak = make_case("weak-live", "live", 1, 4, 7);
        weak.beats.f1 = 2.0 * weak.beats.precision * weak.beats.recall
            / (weak.beats.precision + weak.beats.recall);
        let mut strong = make_case("club", "house", 90, 2, 1);
        strong.beats.f1 = 2.0 * strong.beats.precision * strong.beats.recall
            / (strong.beats.precision + strong.beats.recall);
        let summary = summarize_rhythm(&[weak, strong]);
        assert_eq!(summary.overall.beat.true_positives, 91);
        assert_eq!(summary.overall.case_count, 2);
        assert_eq!(summary.per_genre["live"].case_count, 1);
        assert!(summary.per_genre["live"].beat.f1 < summary.per_genre["house"].beat.f1);
        assert_eq!(summary.overall.meter_rank1_fraction, Some(1.0));
        assert_eq!(
            summary.overall.downbeat.as_ref().unwrap().true_positives,
            22
        );
        assert_eq!(
            summary.per_genre["live"].phase_drift_by_span[&16].max_absolute_drift_ms,
            120.0
        );
        assert_eq!(summary.overall.phase_drift_by_span[&16].sample_count, 2);
        let threshold = summary
            .overall
            .tempo_selectivity
            .iter()
            .find(|point| (point.minimum_raw_strength - 0.5).abs() < 0.001)
            .unwrap();
        assert_eq!(threshold.annotated_cases, 2);
        assert_eq!(threshold.selected_cases, 1);
        assert_eq!(threshold.correct_selected_cases, 1);
        assert_eq!(threshold.coverage, 0.5);
        assert_eq!(threshold.accuracy, Some(1.0));
    }
}
