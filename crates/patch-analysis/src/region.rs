//! Selective Level 2 analysis for source-time regions.
//!
//! Requests are deduplicated and performed by one low-priority worker. The
//! result is small, versioned musical context stored with the parent track's
//! existing bounded analysis cache; discovery never schedules this work.

use crate::WatchedOutput;
use crate::{Analysis, RATE, key};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
};

pub const DEEP_REGION_VERSION: u32 = 7;
pub const REGION_RHYTHM_VERSION: u32 = 1;
pub const GROOVE_EVIDENCE_VERSION: u32 = 2;
const REGION_QUANTUM_S: f64 = 4.0;
const MAX_REGION_S: f64 = 64.0;
const MAX_REGIONS_PER_TRACK: usize = 128;
const MAX_QUEUED_REGIONS: usize = 64;
const HARMONY_FRAME_SECONDS: usize = 4;
const MAX_FAILED_REGIONS: usize = 64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyHypothesis {
    pub key: key::Key,
    /// Raw profile similarity; not a calibrated probability.
    pub raw_score: f32,
}

/// A raw triad-template match for one local chroma frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChordHypothesis {
    /// Pitch class, C=0 through B=11.
    pub root: u8,
    pub minor: bool,
    /// Cosine similarity against a simple triad template, not a probability.
    pub raw_score: f32,
}

/// Time-local harmonic material derived from a source track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarmonyFrame {
    pub start_s: f64,
    pub end_s: f64,
    pub chroma: [f32; 12],
    pub chords: Vec<ChordHypothesis>,
}

/// Local onset timing checked against the already selected track beat grid.
/// Offsets and scores are raw observations, not calibrated probabilities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegionRhythmCheck {
    pub analyzer_version: u32,
    pub beats: Vec<RegionBeatCheck>,
    pub observed_coverage: f32,
    pub mean_offset_ms: Option<f32>,
    pub offset_jitter_ms: Option<f32>,
    /// Linear offset slope accumulated across the measured beat span.
    pub phase_drift_ms: Option<f32>,
}

/// Raw subdivision timing observed in a selective source-time region. The
/// histogram describes where secondary onsets fall within beats; it is not a
/// genre or calibrated swing classifier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrooveEvidence {
    pub analyzer_version: u32,
    pub beat_count: u16,
    pub observed_offbeats: u16,
    pub offbeat_coverage: f32,
    /// Weighted onset mass in eight equal phase bins spanning beat fractions
    /// 0.25–0.85, normalized to sum to one when any offbeats were observed.
    pub phase_histogram: [f32; 8],
    /// Swing: the weighted average phase of the eighth-note off-beat (the
    /// strongest onset at 0.40-0.72 of each beat; 0.50 straight, 0.67 a
    /// triplet shuffle), when supported.
    pub mean_phase: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegionBeatCheck {
    pub grid_time_s: f64,
    pub observed_time_s: Option<f64>,
    pub offset_ms: Option<f32>,
    pub raw_support: f32,
}

/// A cached harmony view of one region of an ordinary source track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeepRegion {
    pub source_identity: String,
    pub analyzer_version: u32,
    pub start_s: f64,
    pub end_s: f64,
    pub analyzed_seconds: f32,
    pub chroma: [f32; 12],
    /// Four-second summaries retain changes inside the requested region.
    #[serde(default)]
    pub harmony_frames: Vec<HarmonyFrame>,
    /// Selective local beat verification, independent of Level 1 grid scores.
    #[serde(default)]
    pub rhythm_check: Option<RegionRhythmCheck>,
    #[serde(default)]
    pub groove: Option<GrooveEvidence>,
    /// Fine local energy/onset boundaries from this candidate region.
    #[serde(default)]
    pub structure: Option<crate::structure::StructureEvidence>,
    pub key: Option<key::Key>,
    pub key_hypotheses: Vec<KeyHypothesis>,
    pub completed_at_ms: u128,
}

impl DeepRegion {
    pub fn contains(&self, time_s: f64) -> bool {
        time_s.is_finite() && time_s >= self.start_s && time_s < self.end_s
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Job {
    path: PathBuf,
    source_identity: String,
    start_ms: u64,
    end_ms: u64,
}

#[derive(Default)]
struct Jobs {
    queue: Vec<Job>,
    pending: HashSet<Job>,
    priorities: HashMap<Job, i32>,
    active: Option<Job>,
    failures: HashMap<Job, String>,
    running: bool,
}

/// State of selective musical-material analysis. Failures are in-memory and
/// bounded; successful results persist in the versioned analysis cache.
#[derive(Debug, Clone, PartialEq)]
pub enum RegionStatus {
    Unavailable,
    Queued,
    Preparing,
    Ready(Box<DeepRegion>),
    Failed(String),
}

fn job_status(job: &Job, state: &Jobs) -> RegionStatus {
    if state.active.as_ref() == Some(job) {
        RegionStatus::Preparing
    } else if state.pending.contains(job) {
        RegionStatus::Queued
    } else if let Some(error) = state.failures.get(job) {
        RegionStatus::Failed(error.clone())
    } else {
        RegionStatus::Unavailable
    }
}

static JOBS: OnceLock<Mutex<Jobs>> = OnceLock::new();

fn jobs() -> &'static Mutex<Jobs> {
    JOBS.get_or_init(Default::default)
}

fn identity(path: &Path) -> Option<String> {
    crate::analysis_cache_path(path)?
        .file_stem()?
        .to_str()
        .map(ToOwned::to_owned)
}

fn region_bounds(start_s: f64, end_s: f64, duration_s: f64) -> Option<(f64, f64)> {
    if !start_s.is_finite() || !end_s.is_finite() || !duration_s.is_finite() || duration_s <= 0.0 {
        return None;
    }
    let start = (start_s.max(0.0) / REGION_QUANTUM_S).floor() * REGION_QUANTUM_S;
    let end =
        ((end_s.min(duration_s) / REGION_QUANTUM_S).ceil() * REGION_QUANTUM_S).min(duration_s);
    (end > start && end - start <= MAX_REGION_S).then_some((start, end))
}

fn make_job(path: &Path, start_s: f64, end_s: f64, duration: f64) -> Option<Job> {
    let (start_s, end_s) = region_bounds(start_s, end_s, duration)?;
    Some(Job {
        path: path.to_path_buf(),
        source_identity: identity(path)?,
        start_ms: (start_s * 1000.0).round() as u64,
        end_ms: (end_s * 1000.0).round() as u64,
    })
}

fn cached_region(path: &Path, start_s: f64, end_s: f64) -> Option<DeepRegion> {
    let analysis = crate::cached_shared(path)?;
    let (start_s, end_s) = region_bounds(start_s, end_s, analysis.duration)?;
    let identity = identity(path)?;
    analysis
        .deep_regions
        .iter()
        .find(|r| {
            r.source_identity == identity
                && r.analyzer_version == DEEP_REGION_VERSION
                && (r.start_s - start_s).abs() < 0.001
                && (r.end_s - end_s).abs() < 0.001
        })
        .cloned()
}

/// Return analysis state without scheduling additional work.
pub fn status(path: &Path, start_s: f64, end_s: f64) -> RegionStatus {
    let Some(analysis) = crate::cached_shared(path) else {
        return RegionStatus::Unavailable;
    };
    let Some(job) = make_job(path, start_s, end_s, analysis.duration) else {
        return RegionStatus::Unavailable;
    };
    if let Some(region) = cached_region(path, start_s, end_s) {
        return RegionStatus::Ready(Box::new(region));
    }
    job_status(&job, &jobs().lock().unwrap())
}

/// Schedule one bounded background region job. Broad library discovery never
/// calls this API; only candidate regions under review should request work.
pub fn request(path: &Path, start_s: f64, end_s: f64) -> RegionStatus {
    request_prioritized(path, start_s, end_s, 0)
}

/// Schedule a candidate region with relative planner priority. Larger values
/// run first; equal values preserve FIFO order. A full queue admits a new job
/// only when it outranks the lowest queued job. Active work is never preempted.
pub fn request_prioritized(path: &Path, start_s: f64, end_s: f64, priority: i32) -> RegionStatus {
    if let Some(region) = cached_region(path, start_s, end_s) {
        return RegionStatus::Ready(Box::new(region));
    }
    let Some(analysis) = crate::cached_shared(path) else {
        return RegionStatus::Unavailable;
    };
    let Some(job) = make_job(path, start_s, end_s, analysis.duration) else {
        return RegionStatus::Unavailable;
    };
    let mut state = jobs().lock().unwrap();
    if state.active.as_ref() == Some(&job) {
        return RegionStatus::Preparing;
    }
    if let Some(error) = state.failures.get(&job) {
        return RegionStatus::Failed(error.clone());
    }
    let accepted = {
        let Jobs {
            queue,
            pending,
            priorities,
            ..
        } = &mut *state;
        enqueue(queue, pending, priorities, job.clone(), priority)
    };
    if !accepted {
        return RegionStatus::Failed("selective analysis queue is full".into());
    }
    if !state.running {
        state.running = true;
        if std::thread::Builder::new()
            .name("patch-region-analysis".into())
            .spawn(region_worker)
            .is_err()
        {
            state.running = false;
            if let Some(job) = state.queue.pop() {
                state.pending.remove(&job);
                state.priorities.remove(&job);
                state
                    .failures
                    .insert(job, "could not start analysis worker".into());
            }
            return RegionStatus::Failed("could not start analysis worker".into());
        }
    }
    RegionStatus::Queued
}

/// Compatibility helper for callers that only need a ready value.
pub fn soon(path: &Path, start_s: f64, end_s: f64) -> Option<DeepRegion> {
    match request(path, start_s, end_s) {
        RegionStatus::Ready(region) => Some(*region),
        _ => None,
    }
}

/// Explicitly retry a failed selective-analysis request.
pub fn retry(path: &Path, start_s: f64, end_s: f64) -> RegionStatus {
    let Some(analysis) = crate::cached_shared(path) else {
        return RegionStatus::Unavailable;
    };
    let Some(job) = make_job(path, start_s, end_s, analysis.duration) else {
        return RegionStatus::Unavailable;
    };
    jobs().lock().unwrap().failures.remove(&job);
    request(path, start_s, end_s)
}

fn region_worker() {
    // SAFETY: Linux thread niceness keeps optional deep analysis behind deck
    // workers and the user's foreground workload.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
    loop {
        let job = {
            let mut state = jobs().lock().unwrap();
            let next = {
                let Jobs {
                    queue, priorities, ..
                } = &mut *state;
                pop_next(queue, priorities)
            };
            match next {
                Some(job) => {
                    state.active = Some(job.clone());
                    state.pending.remove(&job);
                    state.priorities.remove(&job);
                    job
                }
                None => {
                    state.running = false;
                    return;
                }
            }
        };
        let result = if identity(&job.path).as_deref() != Some(job.source_identity.as_str()) {
            Err("source changed while regional analysis was queued".into())
        } else {
            measure(
                &job.path,
                job.start_ms as f64 / 1000.0,
                job.end_ms as f64 / 1000.0,
            )
        };
        let mut state = jobs().lock().unwrap();
        state.active = None;
        match result {
            Ok(_) => {
                state.failures.remove(&job);
            }
            Err(error) => {
                if state.failures.len() >= MAX_FAILED_REGIONS
                    && let Some(oldest) = state.failures.keys().next().cloned() {
                        state.failures.remove(&oldest);
                    }
                state.failures.insert(job, error);
            }
        }
    }
}

fn pop_next(queue: &mut Vec<Job>, priorities: &HashMap<Job, i32>) -> Option<Job> {
    let mut best_index = None;
    let mut best_priority = i32::MIN;
    for (index, job) in queue.iter().enumerate() {
        let priority = priorities.get(job).copied().unwrap_or_default();
        if best_index.is_none() || priority > best_priority {
            best_index = Some(index);
            best_priority = priority;
        }
    }
    best_index.map(|index| queue.remove(index))
}

fn enqueue(
    queue: &mut Vec<Job>,
    pending: &mut HashSet<Job>,
    priorities: &mut HashMap<Job, i32>,
    job: Job,
    priority: i32,
) -> bool {
    if pending.contains(&job) {
        let queued_priority = priorities.entry(job).or_default();
        *queued_priority = (*queued_priority).max(priority);
        return true;
    }
    if pending.len() >= MAX_QUEUED_REGIONS {
        let Some((weakest_index, weakest)) = queue
            .iter()
            .enumerate()
            .min_by_key(|(_, queued)| priorities.get(*queued).copied().unwrap_or_default())
        else {
            return false;
        };
        if priority <= priorities.get(weakest).copied().unwrap_or_default() {
            return false;
        }
        let evicted = queue.remove(weakest_index);
        pending.remove(&evicted);
        priorities.remove(&evicted);
    }
    pending.insert(job.clone());
    priorities.insert(job.clone(), priority);
    queue.push(job);
    true
}

/// Run regional analysis synchronously. Production callers use [`soon`];
/// this entry point is useful for preparation workers and deterministic tests.
pub fn measure(path: &Path, start_s: f64, end_s: f64) -> Result<DeepRegion, String> {
    let analysis = crate::cached_shared(path).ok_or("track needs Level 1 analysis first")?;
    let (start_s, end_s) = region_bounds(start_s, end_s, analysis.duration)
        .ok_or("invalid or oversized musical region")?;
    let source_identity = identity(path).ok_or("track cache identity unavailable")?;
    if let Some(found) = analysis.deep_regions.iter().find(|r| {
        r.source_identity == source_identity
            && r.analyzer_version == DEEP_REGION_VERSION
            && (r.start_s - start_s).abs() < 0.001
            && (r.end_s - end_s).abs() < 0.001
    }) {
        return Ok(found.clone());
    }
    let duration_s = end_s - start_s;
    let _permit = crate::DecodePermit::acquire();
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-ss"])
        .arg(format!("{start_s:.3}"))
        .args(["-i"])
        .arg(path)
        .args(["-t"])
        .arg(format!("{duration_s:.3}"))
        .args([
            "-vn",
            "-ac",
            "1",
            "-ar",
            &RATE.to_string(),
            "-f",
            "f32le",
            "-",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .watched_output()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "ffmpeg: {}",
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .next()
                .unwrap_or("failed")
        ));
    }
    let mono: Vec<f32> = output
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let analyzed_seconds = mono.len() as f32 / RATE as f32;
    if analyzed_seconds < 3.0 {
        return Err("region is too short to estimate local harmony".into());
    }
    let rhythm_check = analysis
        .rhythm
        .as_ref()
        .filter(|rhythm| rhythm.analyzer_version == crate::tempo::RHYTHM_EVIDENCE_VERSION)
        .map(|rhythm| verify_region_rhythm(&mono, RATE, start_s, end_s, &rhythm.beats));
    let groove = analysis
        .rhythm
        .as_ref()
        .filter(|rhythm| rhythm.analyzer_version == crate::tempo::RHYTHM_EVIDENCE_VERSION)
        .map(|rhythm| analyze_groove(&mono, RATE, start_s, end_s, &rhythm.beats));
    let beat_events = analysis
        .rhythm
        .as_ref()
        .filter(|rhythm| rhythm.analyzer_version == crate::tempo::RHYTHM_EVIDENCE_VERSION)
        .map(|rhythm| rhythm.beats.as_slice())
        .unwrap_or_default();
    let structure =
        analyze_region_structure(&mono, RATE, start_s, analyzed_seconds as f64, beat_events);
    let chroma = key::chroma(&mono, RATE);
    let ranked = key::best_key(&chroma, &key::profiles::KK, false);
    let hypotheses: Vec<KeyHypothesis> = ranked
        .iter()
        .take(4)
        .map(|(score, key)| KeyHypothesis {
            key: *key,
            raw_score: *score as f32,
        })
        .collect();
    let key = (chroma.iter().sum::<f64>() > 1e-9)
        .then(|| hypotheses.first().map(|h| h.key))
        .flatten();
    let harmony_frames = harmony_frames(&mono, start_s, end_s, beat_events);
    let result = DeepRegion {
        source_identity,
        analyzer_version: DEEP_REGION_VERSION,
        start_s,
        end_s,
        analyzed_seconds,
        chroma: chroma.map(|x| x as f32),
        harmony_frames,
        rhythm_check,
        groove,
        structure,
        key,
        key_hypotheses: if key.is_some() {
            hypotheses
        } else {
            Vec::new()
        },
        completed_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    };
    if identity(path).as_deref() != Some(result.source_identity.as_str()) {
        return Err("source changed during regional analysis".into());
    }
    store(path, &result)?;
    Ok(result)
}

fn analyze_region_structure(
    mono: &[f32],
    rate: u32,
    start_s: f64,
    duration_s: f64,
    beats: &[crate::tempo::BeatEvent],
) -> Option<crate::structure::StructureEvidence> {
    if !duration_s.is_finite() || duration_s < 10.0 || rate == 0 {
        return None;
    }
    let seconds = duration_s.ceil() as usize;
    let mut energy = vec![0u8; seconds];
    let per_second = rate as usize;
    for (second, samples) in mono.chunks(per_second).take(seconds).enumerate() {
        let rms = (samples
            .iter()
            .map(|sample| f64::from(*sample).powi(2))
            .sum::<f64>()
            / samples.len().max(1) as f64)
            .sqrt();
        energy[second] = (rms.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    let local_beats: Vec<_> = beats
        .iter()
        .filter(|beat| beat.time_s >= start_s && beat.time_s < start_s + duration_s)
        .cloned()
        .map(|mut beat| {
            beat.time_s -= start_s;
            beat
        })
        .collect();
    let mut evidence = crate::structure::analyze(&energy, &[], &local_beats, duration_s);
    for boundary in &mut evidence.boundaries {
        boundary.time_s += start_s;
    }
    for section in &mut evidence.sections {
        section.start_s += start_s;
        section.end_s += start_s;
    }
    Some(evidence)
}

/// Groove evidence of mono audio against a beat grid (as regions measure
/// it): benchmarks score its swing reading against known grooves.
pub fn groove_of(mono: &[f32], rate: u32, beats: &[crate::tempo::BeatEvent]) -> GrooveEvidence {
    let end = mono.len() as f64 / f64::from(rate.max(1));
    analyze_groove(mono, rate, 0.0, end, beats)
}

fn analyze_groove(
    mono: &[f32],
    rate: u32,
    start_s: f64,
    end_s: f64,
    beats: &[crate::tempo::BeatEvent],
) -> GrooveEvidence {
    let (envelope, frames_per_second) = crate::tempo::onset_envelope(mono, rate);
    summarize_groove(&envelope, frames_per_second, rate, start_s, end_s, beats)
}

fn summarize_groove(
    envelope: &[f64],
    frames_per_second: f64,
    rate: u32,
    start_s: f64,
    end_s: f64,
    beats: &[crate::tempo::BeatEvent],
) -> GrooveEvidence {
    const PHASE_START: f64 = 0.25;
    const PHASE_END: f64 = 0.85;
    const SWING_START: f64 = 0.40;
    const SWING_END: f64 = 0.72;
    let relevant: Vec<_> = beats
        .iter()
        .filter(|beat| beat.time_s >= start_s && beat.time_s < end_s)
        .filter(|beat| (0.25..=2.0).contains(&f64::from(beat.interval_s)))
        .collect();
    let frame_half_window = crate::tempo::WIN as f64 / (2.0 * rate.max(1) as f64);
    let global_mean = envelope.iter().sum::<f64>() / envelope.len().max(1) as f64;
    let mut histogram = [0.0f64; 8];
    let mut total_weight = 0.0;
    let mut observed = 0u16;
    // Swing: the strongest onset where the eighth-note off-beat can sit
    // (straight 0.50 to a triplet's 0.67), so straight 16ths at 0.25 and
    // 0.75 never pull the reading.
    let (mut swing_sum, mut swing_weight) = (0.0f64, 0.0f64);
    for beat in &relevant {
        let interval = f64::from(beat.interval_s);
        let frame_at = |phase: f64| (beat.time_s + phase * interval - start_s - frame_half_window) * frames_per_second;
        let (a, b) = (frame_at(SWING_START).ceil().max(0.0) as usize, frame_at(SWING_END).floor().max(0.0) as usize);
        // The beat's own onset, read from the same envelope, is the phase
        // reference: the detector's latency (13.5 ms, measured) cancels.
        let (c, d) = (frame_at(-0.12).ceil().max(0.0) as usize, frame_at(0.12).floor().max(0.0) as usize);
        let on_beat = (c <= d && d < envelope.len())
            .then(|| (c..=d).max_by(|x, y| envelope[*x].total_cmp(&envelope[*y])))
            .flatten()
            .filter(|p| envelope[*p] >= global_mean * 1.25)
            .map_or(beat.time_s, |p| start_s + p as f64 / frames_per_second + frame_half_window);
        if a <= b
            && b < envelope.len()
            && let Some(peak) = (a..=b).max_by(|x, y| envelope[*x].total_cmp(&envelope[*y]))
            && global_mean > 1e-12
            && envelope[peak] >= global_mean * 1.25
        {
            let observed_time = start_s + peak as f64 / frames_per_second + frame_half_window;
            let phase = (observed_time - on_beat) / interval;
            let weight = (envelope[peak] / global_mean).min(8.0);
            swing_sum += phase * weight;
            swing_weight += weight;
        }
    }
    for beat in &relevant {
        let interval = f64::from(beat.interval_s);
        let first_time = beat.time_s + PHASE_START * interval;
        let last_time = beat.time_s + PHASE_END * interval;
        let first = ((first_time - start_s - frame_half_window) * frames_per_second)
            .ceil()
            .max(0.0) as usize;
        let last = (((last_time - start_s - frame_half_window) * frames_per_second)
            .floor()
            .max(0.0) as usize)
            .min(envelope.len().saturating_sub(1));
        if first > last || first >= envelope.len() {
            continue;
        }
        let Some(peak) = (first..=last).max_by(|a, b| envelope[*a].total_cmp(&envelope[*b])) else {
            continue;
        };
        let support = envelope[peak];
        if global_mean <= 1e-12 || support < global_mean * 1.25 {
            continue;
        }
        let observed_time = start_s + peak as f64 / frames_per_second + frame_half_window;
        let phase = ((observed_time - beat.time_s) / interval).clamp(PHASE_START, PHASE_END);
        let bin =
            (((phase - PHASE_START) / (PHASE_END - PHASE_START) * 8.0).floor() as usize).min(7);
        // Bound the influence of isolated large transients.
        let weight = (support / global_mean).min(8.0);
        histogram[bin] += weight;
        total_weight += weight;
        observed = observed.saturating_add(1);
    }
    let phase_histogram = if total_weight > 0.0 {
        histogram.map(|v| (v / total_weight) as f32)
    } else {
        [0.0; 8]
    };
    GrooveEvidence {
        analyzer_version: GROOVE_EVIDENCE_VERSION,
        beat_count: relevant.len().min(u16::MAX as usize) as u16,
        observed_offbeats: observed,
        offbeat_coverage: if relevant.is_empty() {
            0.0
        } else {
            observed as f32 / relevant.len() as f32
        },
        phase_histogram,
        mean_phase: (swing_weight > 0.0).then_some((swing_sum / swing_weight) as f32),
    }
}

fn verify_region_rhythm(
    mono: &[f32],
    rate: u32,
    start_s: f64,
    end_s: f64,
    reference_beats: &[crate::tempo::BeatEvent],
) -> RegionRhythmCheck {
    let (envelope, frames_per_second) = crate::tempo::onset_envelope(mono, rate);
    summarize_region_rhythm(
        &envelope,
        frames_per_second,
        rate,
        start_s,
        end_s,
        reference_beats,
    )
}

fn summarize_region_rhythm(
    envelope: &[f64],
    frames_per_second: f64,
    rate: u32,
    start_s: f64,
    end_s: f64,
    reference_beats: &[crate::tempo::BeatEvent],
) -> RegionRhythmCheck {
    let mean = envelope.iter().sum::<f64>() / envelope.len().max(1) as f64;
    let beats: Vec<_> = reference_beats
        .iter()
        .filter(|beat| beat.time_s >= start_s && beat.time_s < end_s)
        .map(|beat| {
            let center = (beat.time_s - start_s - crate::tempo::WIN as f64 / (2.0 * rate as f64))
                * frames_per_second;
            let frame = center.round().max(0.0) as usize;
            let radius = (0.12 * frames_per_second).round() as usize;
            let first = frame.saturating_sub(radius);
            let last = (frame + radius).min(envelope.len().saturating_sub(1));
            let peak = (first..=last).max_by(|a, b| envelope[*a].total_cmp(&envelope[*b]));
            let support = peak.map_or(0.0, |index| envelope[index]);
            let observed = (support > mean * 0.2).then(|| {
                start_s
                    + peak.unwrap() as f64 / frames_per_second
                    + crate::tempo::WIN as f64 / (2.0 * rate as f64)
            });
            let offset = observed.map(|time| ((time - beat.time_s) * 1000.0) as f32);
            RegionBeatCheck {
                grid_time_s: beat.time_s,
                observed_time_s: observed,
                offset_ms: offset,
                raw_support: if mean > 0.0 {
                    (support / mean).min(100.0) as f32
                } else {
                    0.0
                },
            }
        })
        .collect();
    let offsets: Vec<(f64, f64)> = beats
        .iter()
        .filter_map(|beat| {
            beat.offset_ms
                .map(|offset| (beat.grid_time_s, f64::from(offset)))
        })
        .collect();
    let count = offsets.len();
    let mean_offset =
        (count > 0).then(|| offsets.iter().map(|(_, o)| o).sum::<f64>() / count as f64);
    let jitter = mean_offset.map(|mean| {
        (offsets
            .iter()
            .map(|(_, offset)| (offset - mean).powi(2))
            .sum::<f64>()
            / count as f64)
            .sqrt() as f32
    });
    let drift = (count >= 4).then(|| {
        let x_mean = offsets.iter().map(|(time, _)| time).sum::<f64>() / count as f64;
        let y_mean = offsets.iter().map(|(_, offset)| offset).sum::<f64>() / count as f64;
        let xx = offsets
            .iter()
            .map(|(time, _)| (time - x_mean).powi(2))
            .sum::<f64>();
        let slope = if xx > 0.0 {
            offsets
                .iter()
                .map(|(time, offset)| (time - x_mean) * (offset - y_mean))
                .sum::<f64>()
                / xx
        } else {
            0.0
        };
        (slope * (offsets.last().unwrap().0 - offsets.first().unwrap().0)) as f32
    });
    RegionRhythmCheck {
        analyzer_version: REGION_RHYTHM_VERSION,
        observed_coverage: count as f32 / beats.len().max(1) as f32,
        beats,
        mean_offset_ms: mean_offset.map(|value| value as f32),
        offset_jitter_ms: jitter,
        phase_drift_ms: drift,
    }
}

/// Local harmony one bar at a time, on the track's own bars: chords change
/// on bars, and a fixed 4 s frame straddling a change mixed two chords
/// (`patch-bench --chord-eval`: 46.5 % of bars right on fixed frames, 100 %
/// bar-aligned, on synthetic progressions). Without downbeats: 4 s frames.
fn harmony_frames(
    mono: &[f32],
    start_s: f64,
    end_s: f64,
    beats: &[crate::tempo::BeatEvent],
) -> Vec<HarmonyFrame> {
    let frame = |a: f64, b: f64| -> Option<HarmonyFrame> {
        let i0 = (((a - start_s) * f64::from(RATE)).max(0.0) as usize).min(mono.len());
        let i1 = (((b - start_s) * f64::from(RATE)).max(0.0) as usize).min(mono.len());
        (i1 > i0 + RATE as usize).then(|| {
            let chroma = key::chroma(&mono[i0..i1], RATE);
            HarmonyFrame {
                start_s: a,
                end_s: b.min(end_s),
                chroma: chroma.map(|x| x as f32),
                chords: chord_hypotheses(&chroma),
            }
        })
    };
    let bars: Vec<f64> = beats
        .iter()
        .filter(|b| b.beat_in_bar == Some(0) && b.time_s >= start_s - 1e-6 && b.time_s < end_s)
        .map(|b| b.time_s)
        .collect();
    if bars.len() >= 2 {
        // The partial bar before the first downbeat is left out; the last
        // bar ends at the region's end or one bar on.
        let last = bars[bars.len() - 1] + (bars[bars.len() - 1] - bars[bars.len() - 2]);
        return bars
            .iter()
            .zip(bars.iter().skip(1).copied().chain(std::iter::once(last)))
            .filter_map(|(a, b)| frame(*a, b.min(end_s)))
            .collect();
    }
    let step = HARMONY_FRAME_SECONDS as f64;
    (0..)
        .map(|k| start_s + k as f64 * step)
        .take_while(|a| *a + 2.0 <= end_s)
        .filter_map(|a| frame(a, (a + step).min(end_s)))
        .collect()
}

/// The best triad matches for a stretch of mono audio (as regions measure
/// them): benchmarks score this against known progressions.
pub fn estimate_chords(samples: &[f32], rate: u32) -> Vec<ChordHypothesis> {
    chord_hypotheses(&key::chroma(samples, rate))
}

fn chord_hypotheses(chroma: &[f64; 12]) -> Vec<ChordHypothesis> {
    let signal_norm = chroma.iter().map(|v| v * v).sum::<f64>().sqrt();
    if signal_norm <= 1e-12 {
        return Vec::new();
    }
    let mut candidates = Vec::with_capacity(24);
    for root in 0..12u8 {
        for minor in [false, true] {
            let third = (root + if minor { 3 } else { 4 }) % 12;
            let fifth = (root + 7) % 12;
            let score = (chroma[root as usize] + chroma[third as usize] + chroma[fifth as usize])
                / (signal_norm * 3.0f64.sqrt());
            candidates.push(ChordHypothesis {
                root,
                minor,
                raw_score: score.clamp(0.0, 1.0) as f32,
            });
        }
    }
    candidates.sort_by(|a, b| b.raw_score.total_cmp(&a.raw_score));
    candidates.truncate(3);
    candidates
}

fn store(path: &Path, region: &DeepRegion) -> Result<(), String> {
    let file = crate::cache_path(path).ok_or("track cache identity unavailable")?;
    crate::cache_store::update(&file, |latest| {
        let mut analysis: Analysis =
            serde_json::from_value(latest.ok_or("track analysis missing")?)
                .map_err(|e| e.to_string())?;
        if analysis.version != crate::VERSION {
            return Err("track analysis changed while regional analysis was running".into());
        }
        analysis.deep_regions.retain(|r| {
            !(r.source_identity == region.source_identity
                && (r.start_s - region.start_s).abs() < 0.001
                && (r.end_s - region.end_s).abs() < 0.001)
        });
        analysis.deep_regions.push(region.clone());
        analysis
            .deep_regions
            .sort_by_key(|r| std::cmp::Reverse(r.completed_at_ms));
        analysis.deep_regions.truncate(MAX_REGIONS_PER_TRACK);
        serde_json::to_value(analysis).map_err(|e| e.to_string())
    })
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_requests_are_bounded_and_quantized() {
        assert_eq!(region_bounds(9.2, 20.1, 60.0), Some((8.0, 24.0)));
        assert_eq!(region_bounds(-2.0, 4.0, 60.0), Some((0.0, 4.0)));
        assert!(region_bounds(0.0, 70.0, 80.0).is_none());
        assert!(region_bounds(f64::NAN, 10.0, 80.0).is_none());
    }

    #[test]
    fn region_job_status_distinguishes_queue_work_and_failure() {
        let job = Job {
            path: PathBuf::from("/tmp/source.flac"),
            source_identity: "content-id".into(),
            start_ms: 8_000,
            end_ms: 24_000,
        };
        let mut state = Jobs::default();
        assert_eq!(job_status(&job, &state), RegionStatus::Unavailable);
        state.pending.insert(job.clone());
        state.priorities.insert(job.clone(), 0);
        assert_eq!(job_status(&job, &state), RegionStatus::Queued);
        state.active = Some(job.clone());
        assert_eq!(job_status(&job, &state), RegionStatus::Preparing);
        state.active = None;
        state.pending.remove(&job);
        state.failures.insert(job.clone(), "decode failed".into());
        assert_eq!(
            job_status(&job, &state),
            RegionStatus::Failed("decode failed".into())
        );
    }

    #[test]
    fn region_queue_prefers_higher_priority_and_keeps_fifo_for_ties() {
        let job = |name: &str| Job {
            path: PathBuf::from(format!("/tmp/{name}.flac")),
            source_identity: name.into(),
            start_ms: 0,
            end_ms: 16_000,
        };
        let (first, second, urgent) = (job("first"), job("second"), job("urgent"));
        let mut queue = vec![first.clone(), second.clone(), urgent.clone()];
        let priorities =
            HashMap::from([(first.clone(), 5), (second.clone(), 5), (urgent.clone(), 9)]);
        assert_eq!(pop_next(&mut queue, &priorities), Some(urgent));
        assert_eq!(pop_next(&mut queue, &priorities), Some(first));
        assert_eq!(pop_next(&mut queue, &priorities), Some(second));
        assert!(queue.is_empty());
    }

    #[test]
    fn full_region_queue_only_replaces_a_lower_priority_queued_job() {
        let job = |index: usize| Job {
            path: PathBuf::from(format!("/tmp/{index}.flac")),
            source_identity: index.to_string(),
            start_ms: 0,
            end_ms: 16_000,
        };
        let mut queue = Vec::new();
        let mut pending = HashSet::new();
        let mut priorities = HashMap::new();
        for index in 0..MAX_QUEUED_REGIONS {
            assert!(enqueue(
                &mut queue,
                &mut pending,
                &mut priorities,
                job(index),
                index as i32,
            ));
        }
        let weakest = job(0);
        let urgent = job(999);
        assert!(!enqueue(
            &mut queue,
            &mut pending,
            &mut priorities,
            job(1000),
            0,
        ));
        assert!(enqueue(
            &mut queue,
            &mut pending,
            &mut priorities,
            urgent.clone(),
            100,
        ));
        assert!(!pending.contains(&weakest));
        assert!(pending.contains(&urgent));
        assert_eq!(queue.len(), MAX_QUEUED_REGIONS);
    }

    #[test]
    fn deep_results_only_match_their_source_region() {
        let result = DeepRegion {
            source_identity: "content-id".into(),
            analyzer_version: DEEP_REGION_VERSION,
            start_s: 8.0,
            end_s: 24.0,
            analyzed_seconds: 16.0,
            chroma: [0.0; 12],
            harmony_frames: Vec::new(),
            rhythm_check: None,
            groove: None,
            structure: None,
            key: None,
            key_hypotheses: vec![],
            completed_at_ms: 1,
        };
        assert!(result.contains(8.0));
        assert!(result.contains(23.99));
        assert!(!result.contains(24.0));
    }

    #[test]
    fn selective_rhythm_check_separates_constant_phase_from_phase_drift() {
        let frames_per_second = 100.0;
        let half_window = crate::tempo::WIN as f64 / (2.0 * 22_050.0);
        let beats: Vec<_> = (1..=7)
            .map(|index| crate::tempo::BeatEvent {
                time_s: f64::from(index),
                beat_index: i64::from(index),
                interval_s: 1.0,
                bar_index: None,
                beat_in_bar: None,
                onset_offset_s: None,
                onset_support: 0.0,
            })
            .collect();
        let mut steady = vec![0.0; 900];
        for beat in &beats {
            let frame = ((beat.time_s + 0.025 - half_window) * frames_per_second).round() as usize;
            steady[frame] = 10.0;
        }
        let stable = summarize_region_rhythm(&steady, frames_per_second, 22_050, 0.0, 9.0, &beats);
        assert_eq!(stable.observed_coverage, 1.0);
        assert!(stable.offset_jitter_ms.unwrap() < 6.0);
        assert!(stable.phase_drift_ms.unwrap().abs() < 6.0);

        let mut drifting = vec![0.0; 900];
        for (ordinal, beat) in beats.iter().enumerate() {
            let offset_s = 0.01 + ordinal as f64 * 0.01;
            let frame =
                ((beat.time_s + offset_s - half_window) * frames_per_second).round() as usize;
            drifting[frame] = 10.0;
        }
        let drift = summarize_region_rhythm(&drifting, frames_per_second, 22_050, 0.0, 9.0, &beats);
        assert_eq!(drift.observed_coverage, 1.0);
        assert!(drift.offset_jitter_ms.unwrap() > 15.0);
        assert!(drift.phase_drift_ms.unwrap() > 50.0);
    }

    #[test]
    fn selective_groove_evidence_distinguishes_straight_and_late_eighths() {
        let fps = 100.0;
        let rate = 22_050;
        let beats: Vec<_> = (1..=8)
            .map(|index| crate::tempo::BeatEvent {
                time_s: f64::from(index),
                beat_index: i64::from(index),
                interval_s: 1.0,
                bar_index: None,
                beat_in_bar: None,
                onset_offset_s: None,
                onset_support: 0.0,
            })
            .collect();
        let envelope_for = |phase: f64| {
            let mut envelope = vec![0.1; 1_000];
            let half_window = crate::tempo::WIN as f64 / (2.0 * rate as f64);
            for beat in &beats {
                let frame = ((beat.time_s + phase - half_window) * fps).round() as usize;
                envelope[frame] = 4.0;
            }
            envelope
        };
        let straight = summarize_groove(&envelope_for(0.5), fps, rate, 0.0, 10.0, &beats);
        let swung = summarize_groove(&envelope_for(2.0 / 3.0), fps, rate, 0.0, 10.0, &beats);
        assert_eq!(straight.offbeat_coverage, 1.0);
        assert_eq!(swung.offbeat_coverage, 1.0);
        assert!((straight.mean_phase.unwrap() - 0.5).abs() < 0.02);
        assert!((swung.mean_phase.unwrap() - 2.0 / 3.0).abs() < 0.02);
        assert_ne!(straight.phase_histogram, swung.phase_histogram);
    }

    #[test]
    fn candidate_region_structure_keeps_boundaries_in_source_time() {
        let rate = 2_000;
        let mut mono = vec![0.04f32; rate as usize * 10];
        mono.extend(vec![0.6f32; rate as usize * 10]);
        let structure = analyze_region_structure(&mono, rate, 72.0, 20.0, &[]).unwrap();
        assert!(
            structure.boundaries.iter().any(|boundary| {
                (boundary.time_s - 82.0).abs() <= 1.0 && boundary.raw_score > 0.2
            })
        );
        assert!((structure.sections.first().unwrap().start_s - 72.0).abs() < f64::EPSILON);
        assert!(structure.sections.last().unwrap().end_s > 91.0);
    }

    /// Harmony is read one bar at a time on the grid's downbeats: C major
    /// and A minor alternating every bar (2 s) come out bar by bar, where a
    /// 4 s frame would have mixed them.
    #[test]
    fn harmony_frames_follow_the_bars() {
        let bar = 2.0;
        let bars = 6;
        let n = (bar * f64::from(bars) * f64::from(RATE)) as usize;
        let tone = |midi: f64, t: f64| (std::f64::consts::TAU * 440.0 * 2f64.powf((midi - 69.0) / 12.0) * t).sin();
        let mono: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / f64::from(RATE);
                let notes: [f64; 3] = if ((t / bar) as usize).is_multiple_of(2) { [60.0, 64.0, 67.0] } else { [57.0, 60.0, 64.0] };
                (notes.iter().map(|m| tone(*m, t)).sum::<f64>() * 0.2) as f32
            })
            .collect();
        let beats: Vec<crate::tempo::BeatEvent> = (0..bars * 4)
            .map(|i| crate::tempo::BeatEvent {
                time_s: f64::from(i) * bar / 4.0,
                beat_index: i64::from(i),
                interval_s: (bar / 4.0) as f32,
                bar_index: Some(i64::from(i / 4)),
                beat_in_bar: Some((i % 4) as u8),
                onset_offset_s: None,
                onset_support: 1.0,
            })
            .collect();
        let frames = harmony_frames(&mono, 0.0, bar * f64::from(bars), &beats);
        assert_eq!(frames.len(), bars as usize);
        for (k, f) in frames.iter().enumerate() {
            assert!((f.start_s - k as f64 * bar).abs() < 1e-9);
            let top = &f.chords[0];
            let want = if k.is_multiple_of(2) { (0, false) } else { (9, true) };
            assert_eq!((top.root, top.minor), want, "bar {k}");
        }
    }

    #[test]
    fn chord_hypotheses_retain_major_minor_ambiguity_as_raw_similarity() {
        let mut c_major = [0.0; 12];
        c_major[0] = 1.0;
        c_major[4] = 0.8;
        c_major[7] = 0.7;
        let chords = chord_hypotheses(&c_major);
        assert_eq!((chords[0].root, chords[0].minor), (0, false));
        assert!(chords[0].raw_score > 0.98);
        assert!(
            chords
                .iter()
                .all(|chord| (0.0..=1.0).contains(&chord.raw_score))
        );
    }
}
