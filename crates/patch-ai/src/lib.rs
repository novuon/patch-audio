//! The DJ planner's interface: the types and shared helpers the rest of
//! Patch uses. This build has no planner: nothing is suggested or planned,
//! and the engine says so (`AVAILABLE`).

pub mod intents;
pub mod materials;
pub mod mix;
pub mod select;
pub mod stems;

use patch_analysis::Analysis;
use patch_core::command::Deck;
use patch_core::intent::{TransitionPlan, TransitionStyle};
use serde::{Deserialize, Serialize};

/// Whether this build has a planner.
pub const AVAILABLE: bool = false;

/// Why there is nothing here.
pub const NOT_IN_THIS_BUILD: &str = "Not available in this build";

/// A track the AI can reason about: identity plus its analysis.
#[derive(Clone, Debug)]
pub struct TrackInfo {
    pub stems: patch_core::stems::StemContext,
    /// Stable identity (library UUID, or the path).
    pub id: String,
    pub path: String,
    pub title: String,
    pub artist: String,
    /// 0 … 5 stars.
    pub rating: u8,
    pub analysis: Analysis,
    /// Per second: mean [low, mid, high] band level (0…255), from the
    /// detailed waveform. Empty when that isn't available.
    pub bands: Vec<[u8; 3]>,
}

impl TrackInfo {

    /// Whether source time is close to current coarse structural evidence.
    /// This is raw salience used only as a low-priority planning hint.
    pub fn near_coarse_boundary(&self, time_s: f64, tolerance_s: f64) -> bool {
        let matches = |structure: &patch_analysis::structure::StructureEvidence| {
            structure.analyzer_version == patch_analysis::STRUCTURE_ANALYZER_VERSION
                && structure
                    .boundaries
                    .iter()
                    .any(|b| b.raw_score >= 0.22 && (b.time_s - time_s).abs() <= tolerance_s)
        };
        self.analysis.structure.as_ref().is_some_and(matches)
            || self.analysis.deep_regions.iter().any(|region| {
                region.analyzer_version == patch_analysis::region::DEEP_REGION_VERSION
                    && time_s >= region.start_s
                    && time_s <= region.end_s
                    && region.structure.as_ref().is_some_and(matches)
            })
    }

    /// Derive a raw 4/4 groove summary from the cached local beat sequence.
    /// This describes on-grid accent strength, onset coverage and timing by
    /// bar position; it is not a genre label or calibrated groove score.
    /// Unknown downbeat phase or fewer than two bars yields no estimate.
    pub fn groove_4_4(&self, start_s: f64, end_s: f64) -> Option<GrooveSignature> {
        let evidence = self.analysis.rhythm.as_ref()?;
        if evidence.analyzer_version != patch_analysis::tempo::RHYTHM_EVIDENCE_VERSION
            || !start_s.is_finite()
            || !end_s.is_finite()
            || start_s < 0.0
            || end_s <= start_s
            || self.analysis.downbeat.is_none()
            || self.analysis.downbeat_confidence < 0.3
        {
            return None;
        }
        let beats: Vec<_> = evidence
            .beats
            .iter()
            .filter(|b| b.time_s >= start_s && b.time_s < end_s)
            .collect();
        if beats.len() < 8 || beats.iter().any(|b| b.beat_in_bar.is_none()) {
            return None;
        }
        let mut counts = [0u16; 4];
        let mut hits = [0u16; 4];
        let mut strengths = [0.0f64; 4];
        let mut offset_sums = [0.0f64; 4];
        for beat in beats {
            let slot = beat.beat_in_bar? as usize;
            if slot >= 4 {
                return None;
            }
            counts[slot] = counts[slot].saturating_add(1);
            strengths[slot] += f64::from(beat.onset_support.max(0.0));
            if let Some(offset) = beat.onset_offset_s.filter(|v| v.is_finite()) {
                hits[slot] = hits[slot].saturating_add(1);
                offset_sums[slot] += f64::from(offset);
            }
        }
        if counts.iter().any(|n| *n < 2) {
            return None;
        }
        let raw_strength = std::array::from_fn(|i| (strengths[i] / f64::from(counts[i])) as f32);
        let max_strength = raw_strength.iter().copied().fold(0.0f32, f32::max);
        Some(GrooveSignature {
            beat_strength: if max_strength > 0.0 {
                raw_strength.map(|v| v / max_strength)
            } else {
                [0.0; 4]
            },
            onset_coverage: std::array::from_fn(|i| f32::from(hits[i]) / f32::from(counts[i])),
            mean_onset_offset_ms: std::array::from_fn(|i| {
                (hits[i] > 0).then(|| (offset_sums[i] / f64::from(hits[i]) * 1000.0) as f32)
            }),
            observed_beats: counts,
            raw_downbeat_support: self.analysis.downbeat_confidence,
        })
    }

    /// Best usable local key evidence covering source time `time_s`, if a
    /// selective Level 2 region analysis is ready. This remains distinct
    /// from the whole-track key.
    pub fn regional_key_at(&self, time_s: f64) -> Option<patch_analysis::key::Key> {
        self.analysis
            .deep_regions
            .iter()
            .filter(|r| {
                r.analyzer_version == patch_analysis::region::DEEP_REGION_VERSION
                    && r.contains(time_s)
            })
            .min_by(|a, b| (a.end_s - a.start_s).total_cmp(&(b.end_s - b.start_s)))
            .and_then(|r| r.key)
    }

    /// Cached subdivision timing for a complete source-time window. The
    /// evidence is raw onset phase, not a probability or semantic style tag.
    pub fn subdivision_groove_over(
        &self,
        start_s: f64,
        end_s: f64,
    ) -> Option<&patch_analysis::region::GrooveEvidence> {
        if !start_s.is_finite() || !end_s.is_finite() || start_s < 0.0 || end_s <= start_s {
            return None;
        }
        self.analysis
            .deep_regions
            .iter()
            .filter(|region| {
                region.analyzer_version == patch_analysis::region::DEEP_REGION_VERSION
                    && region.start_s <= start_s
                    && region.end_s >= end_s
            })
            .min_by(|a, b| (a.end_s - a.start_s).total_cmp(&(b.end_s - b.start_s)))
            .and_then(|region| region.groove.as_ref())
            .filter(|groove| {
                groove.analyzer_version == patch_analysis::region::GROOVE_EVIDENCE_VERSION
                    && groove.beat_count >= 8
                    && groove.offbeat_coverage >= 0.2
            })
    }

    /// Local, higher-resolution beat verification from a selective Level 2
    /// source-time analysis. Missing or stale data stays unknown.
    pub fn verified_rhythm_at(
        &self,
        time_s: f64,
    ) -> Option<&patch_analysis::region::RegionRhythmCheck> {
        if self.analysis.human_grid.is_some() {
            return None;
        }
        self.analysis
            .deep_regions
            .iter()
            .filter(|region| {
                region.analyzer_version == patch_analysis::region::DEEP_REGION_VERSION
                    && region.contains(time_s)
            })
            .min_by(|a, b| (a.end_s - a.start_s).total_cmp(&(b.end_s - b.start_s)))
            .and_then(|region| region.rhythm_check.as_ref())
            .filter(|check| check.analyzer_version == patch_analysis::region::REGION_RHYTHM_VERSION)
    }

    /// Summarize verified onsets only over the source-time interval a proposed
    /// performance will use. A cached region outside that interval is neutral.
    pub fn verified_rhythm_over(
        &self,
        start_s: f64,
        end_s: f64,
    ) -> Option<patch_analysis::region::RegionRhythmCheck> {
        if self.analysis.human_grid.is_some()
            || !start_s.is_finite()
            || !end_s.is_finite()
            || end_s <= start_s
        {
            return None;
        }
        let check = self
            .analysis
            .deep_regions
            .iter()
            .filter(|region| {
                region.analyzer_version == patch_analysis::region::DEEP_REGION_VERSION
                    && region.start_s <= start_s
                    && region.end_s >= end_s
            })
            .min_by(|a, b| (a.end_s - a.start_s).total_cmp(&(b.end_s - b.start_s)))?
            .rhythm_check
            .as_ref()?;
        if check.analyzer_version != patch_analysis::region::REGION_RHYTHM_VERSION {
            return None;
        }
        let beats: Vec<_> = check
            .beats
            .iter()
            .filter(|beat| beat.grid_time_s >= start_s && beat.grid_time_s < end_s)
            .cloned()
            .collect();
        if beats.is_empty() {
            return None;
        }
        let offsets: Vec<_> = beats
            .iter()
            .filter_map(|beat| {
                beat.offset_ms
                    .map(|offset| (beat.grid_time_s, f64::from(offset)))
            })
            .collect();
        let coverage = offsets.len() as f32 / beats.len() as f32;
        let mean = (!offsets.is_empty())
            .then(|| offsets.iter().map(|(_, offset)| offset).sum::<f64>() / offsets.len() as f64);
        let jitter = mean.map(|mean| {
            (offsets
                .iter()
                .map(|(_, offset)| (offset - mean).powi(2))
                .sum::<f64>()
                / offsets.len() as f64)
                .sqrt() as f32
        });
        let drift = (offsets.len() >= 4).then(|| {
            let x_mean = offsets.iter().map(|(time, _)| time).sum::<f64>() / offsets.len() as f64;
            let y_mean =
                offsets.iter().map(|(_, offset)| offset).sum::<f64>() / offsets.len() as f64;
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
        Some(patch_analysis::region::RegionRhythmCheck {
            analyzer_version: check.analyzer_version,
            beats,
            observed_coverage: coverage,
            mean_offset_ms: mean.map(|value| value as f32),
            offset_jitter_ms: jitter,
            phase_drift_ms: drift,
        })
    }


    /// Best-ranked raw chord templates for a prepared source-time frame.
    pub fn regional_chords_at(
        &self,
        time_s: f64,
    ) -> Option<&[patch_analysis::region::ChordHypothesis]> {
        self.analysis
            .deep_regions
            .iter()
            .filter(|r| {
                r.analyzer_version == patch_analysis::region::DEEP_REGION_VERSION
                    && r.contains(time_s)
            })
            .min_by(|a, b| (a.end_s - a.start_s).total_cmp(&(b.end_s - b.start_s)))?
            .harmony_frames
            .iter()
            .find(|frame| time_s >= frame.start_s && time_s < frame.end_s)
            .map(|frame| frame.chords.as_slice())
    }
}

/// Where the set should go next (shared with the command layer).
pub use patch_core::intent::Direction;

/// What is playing now.
#[derive(Clone, Debug)]
pub struct Playing {
    pub deck: Deck,
    pub track: TrackInfo,
    /// Playhead, seconds.
    pub position: f64,
    /// Test sessions: earliest outgoing transition in source-track seconds.
    /// None retains normal musical timing.
    pub earliest_mix_out: Option<f64>,
    /// Where in the track it became audible, seconds (a mix-in point).
    /// The play-length setting counts from here; None: from its start.
    pub entered_at: Option<f64>,
    /// Tempo as played (analysis BPM × speed).
    pub bpm: f64,
    /// Key as played (after any key shift), when known.
    pub key: Option<patch_analysis::key::Key>,
}

#[derive(Clone, Debug)]
pub struct Context<'a> {
    pub playing: Playing,
    /// The deck the next track goes on.
    pub next_deck: Deck,
    pub direction: Direction,
    /// Track ids played recently (never suggested again).
    pub history: &'a [String],
    /// Techniques of the last transitions, newest first (for variety).
    pub recent: &'a [String],
}

/// One proposal, with everything the DJ needs to judge it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Suggestion {
    pub track_id: String,
    pub path: String,
    pub title: String,
    pub artist: String,
    /// 0…1 overall fit.
    pub score: f32,
    /// Uncalibrated evidence support index (0…1), not a probability.
    /// Kept as a numeric index until an annotated calibration set exists.
    pub confidence: f32,
    pub reasons: Vec<String>,
    pub style: TransitionStyle,
    /// Seconds on the playing track where the transition starts.
    pub mix_out: f64,
    /// Seconds on the next track where it enters.
    pub mix_in: f64,
    pub plan: TransitionPlan,
    /// The factors behind `score` (for AI views).
    #[serde(default)]
    pub factors: select::Factors,
    /// Source-referenced proposal. It may include a strictly gated short vocal
    /// overlay; the normal AI DJ Prepare/Run surface still executes `plan` only.
    #[serde(default)]
    pub arrangement: Option<patch_core::performance::PerformanceGraph>,
    /// Optional third-source excerpts that fit this proposal's tempo/key.
    /// They are candidates for richer arrangement planning, not executable
    /// automation and not a request to prepare more stems.
    #[serde(default)]
    pub material_options: Vec<materials::CompatibleMaterial>,
    /// Why this pair would gain from stems not ready yet (see
    /// `mix::stems_wanted`): the engine prepares just these two tracks.
    #[serde(default)]
    pub stems_wanted: Option<String>,
    /// Both tracks sing together through the blend (see
    /// `mix::vocals_need_stems`): stems wanted whatever the rehearsal hears.
    #[serde(default)]
    pub stems_vocals: Option<String>,
}

/// Raw, region-local 4/4 accent and timing measurements. The normalized beat
/// strengths are relative to the strongest beat position in this region.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GrooveSignature {
    pub beat_strength: [f32; 4],
    pub onset_coverage: [f32; 4],
    pub mean_onset_offset_ms: [Option<f32>; 4],
    pub observed_beats: [u16; 4],
    /// Existing raw downbeat score used to gate this 4/4 view; not a probability.
    pub raw_downbeat_support: f32,
}

/// Per-second mean band levels from a detailed waveform.
pub fn bands_per_second(d: &patch_analysis::detail::Detail) -> Vec<[u8; 3]> {
    let per = d.columns_per_second.max(1) as usize;
    d.columns
        .chunks(per)
        .map(|c| {
            let mut sum = [0u32; 3];
            for col in c {
                for b in 0..3 {
                    sum[b] += col[b] as u32;
                }
            }
            sum.map(|v| (v / c.len() as u32) as u8)
        })
        .collect()
}

/// Suggestions for the next track: none in this build.
pub fn suggest(_ctx: &Context, _library: &[TrackInfo], _n: usize) -> Vec<Suggestion> {
    Vec::new()
}

/// Arrangement candidates the planner turned down, by reason: none here.
pub fn arrangement_rejections() -> Vec<(&'static str, usize)> {
    Vec::new()
}

/// The decision state of the AI DJ (v1 stops at Prepared: a human runs
/// the transition).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Nothing playing, or AI off.
    #[default]
    Idle,
    /// A track plays; waiting until the mix-out comes in range.
    Listening,
    /// Choosing the next track.
    Selecting,
    /// A suggestion is shown.
    Suggested,
    /// The next deck is loaded, cued and synced.
    Prepared,
    /// A validated arrangement is being prepared in background workers.
    Preparing,
    /// A human (or a human-approved plan) is mixing.
    Mixing,
    /// The DJ took over: the AI steps back until asked again.
    ManualOverride,
}

/// When to start choosing: this long before the mix-out point.
pub const LOOKAHEAD_S: f64 = 60.0;

/// The next state given what happened.
pub fn step(
    state: State,
    playing: bool,
    seconds_to_mix_out: Option<f64>,
    has_suggestion: bool,
    prepared: bool,
    human_touched: bool,
) -> State {
    if human_touched {
        return State::ManualOverride;
    }
    if !playing {
        return State::Idle;
    }
    match state {
        State::ManualOverride | State::Mixing | State::Preparing => state,
        _ if prepared => State::Prepared,
        _ if has_suggestion => State::Suggested,
        _ if seconds_to_mix_out.is_some_and(|s| s <= LOOKAHEAD_S) => State::Selecting,
        _ => State::Listening,
    }
}
