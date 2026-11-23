//! Turning a musical region into exact source frames.
//!
//! The resolver reads the track's analysis — the beat grid ([`RhythmEvidence`]'s
//! beats, their bar and beat-in-bar), the downbeat and the structure's
//! sections and energy — and answers with frames plus the evidence for them.
//! It refuses (with a reason) when there is no usable grid rather than
//! guessing, because a derived asset that lands off the beat is worse than no
//! asset at all.

use crate::model::{Phrase, RegionSpec, Resolved};
use patch_analysis::Analysis;
use patch_analysis::tempo::BeatEvent;

/// A phrase is 32 beats (8 bars), the convention the AI DJ already uses.
pub const PHRASE_BEATS: i64 = 32;
/// 4/4: the meter the resolver supports.
pub const BEATS_PER_BAR: f64 = 4.0;
/// Below this, the tempo is too uncertain to cut on.
pub const MIN_CONFIDENCE: f32 = 0.3;

/// The beat grid, in one place: a beat index to source seconds.
struct Grid<'a> {
    beats: &'a [BeatEvent],
    first_index: i64,
    first_time: f64,
    interval: f64,
    confidence: f32,
}

impl Grid<'_> {
    fn seconds(&self, beat: i64) -> f64 {
        if let Some(b) = self.beats.iter().find(|b| b.beat_index == beat) {
            return b.time_s;
        }
        // Extrapolate beyond the measured window on the straight grid.
        self.first_time + (beat - self.first_index) as f64 * self.interval
    }

    /// The bar and beat-in-bar as the beats measure them, else 4/4 from the
    /// beat index.
    fn bar_of(&self, beat: i64) -> i64 {
        if let Some(b) = self.beats.iter().find(|b| b.beat_index == beat)
            && let Some(bar) = b.bar_index
        {
            return bar;
        }
        (beat - self.first_index).div_euclid(4)
    }
}

fn grid(analysis: &Analysis) -> Result<Grid<'_>, String> {
    let beats = analysis
        .rhythm
        .as_ref()
        .map(|r| r.beats.as_slice())
        .filter(|b| !b.is_empty())
        .ok_or("no beat grid: analyse the track first")?;
    let bpm = analysis
        .bpm
        .filter(|b| b.is_finite() && *b > 0.0)
        .ok_or("no tempo: the track has no usable beat")?;
    if analysis.bpm_confidence < MIN_CONFIDENCE {
        return Err(format!(
            "tempo too uncertain to cut on (confidence {:.2} < {MIN_CONFIDENCE})",
            analysis.bpm_confidence
        ));
    }
    let interval = 60.0 / bpm;
    let first = &beats[0];
    Ok(Grid {
        beats,
        first_index: first.beat_index,
        first_time: first.time_s,
        interval,
        confidence: analysis.bpm_confidence,
    })
}

/// The drop: the largest positive energy jump between adjacent sections.
pub fn drop_time(analysis: &Analysis) -> Option<f64> {
    let sections = analysis.structure.as_ref().map(|s| &s.sections)?;
    let mut best: Option<(f32, f64)> = None;
    for w in sections.windows(2) {
        let delta = w[1].mean_energy - w[0].mean_energy;
        if delta > 0.0 && best.is_none_or(|(d, _)| delta > d) {
            best = Some((delta, w[1].start_s));
        }
    }
    best.map(|(_, t)| t)
}

/// The beat nearest `seconds` on the grid.
fn beat_at(grid: &Grid, seconds: f64) -> i64 {
    grid.first_index + ((seconds - grid.first_time) / grid.interval).round() as i64
}

/// Resolve a musical region to frames at `rate`.
///
/// `analysis_version` records which analysis generation this was resolved
/// against, so a stale recipe can be recognised.
pub fn resolve(spec: &RegionSpec, analysis: &Analysis, rate: u32) -> Result<Resolved, String> {
    let g = grid(analysis)?;
    let (start_beat, count_beats, why) = span(spec, analysis, &g)?;
    apply_sub_range(spec, &g, rate, start_beat, count_beats, why, analysis)
}

/// The beat span of a region, before any beat sub-range.
fn span(spec: &RegionSpec, analysis: &Analysis, g: &Grid) -> Result<(i64, i64, String), String> {
    match spec {
        RegionSpec::Beats { beat, count, .. } => {
            if *count <= 0 {
                return Err("a region needs at least one beat".into());
            }
            Ok((*beat, *count, format!("beats {beat}…{}", beat + count)))
        }
        RegionSpec::Bars { bar, count, .. } => {
            if *count <= 0 {
                return Err("a region needs at least one bar".into());
            }
            let start = bar * 4;
            Ok((start, count * 4, format!("bars {bar}…{}", bar + count)))
        }
        RegionSpec::Phrase { which, .. } => {
            let total = total_beats(analysis, g);
            let last_phrase = (total / PHRASE_BEATS - 1).max(0);
            let start = match which {
                Phrase::Index(i) => {
                    if *i < 0 {
                        return Err("a phrase index is not negative".into());
                    }
                    *i * PHRASE_BEATS
                }
                Phrase::Last => last_phrase * PHRASE_BEATS,
                Phrase::BeforeDrop | Phrase::AfterDrop => {
                    let drop = drop_time(analysis)
                        .ok_or("no drop in the structure: ask for a phrase by index instead")?;
                    let drop_beat = beat_at(g, drop);
                    // The phrase boundary at or after the drop.
                    let boundary = {
                        let rel = drop_beat - g.first_index;
                        g.first_index + rel.div_euclid(PHRASE_BEATS) * PHRASE_BEATS
                    };
                    if matches!(which, Phrase::BeforeDrop) {
                        boundary - PHRASE_BEATS
                    } else {
                        boundary
                    }
                }
            };
            let label = match which {
                Phrase::Index(i) => format!("phrase {i}"),
                Phrase::Last => "the last phrase".into(),
                Phrase::BeforeDrop => "the phrase before the drop".into(),
                Phrase::AfterDrop => "the first phrase after the drop".into(),
            };
            Ok((start, PHRASE_BEATS, label))
        }
        RegionSpec::Section { kind, .. } => {
            let sections = analysis
                .structure
                .as_ref()
                .map(|s| &s.sections)
                .filter(|s| !s.is_empty())
                .ok_or("no structure: ask for a bar or a phrase instead")?;
            let sec = match kind.as_str() {
                "drop" | "after_drop" => {
                    let drop =
                        drop_time(analysis).ok_or("no drop in the structure for this region")?;
                    sections
                        .iter()
                        .find(|s| s.start_s >= drop - 1e-6)
                        .ok_or("the drop is after the last section")?
                }
                "intro" | "low" | "quiet" => sections
                    .iter()
                    .min_by(|a, b| a.mean_energy.total_cmp(&b.mean_energy))
                    .unwrap(),
                "chorus" | "high" | "loud" | "build" => sections
                    .iter()
                    .max_by(|a, b| a.mean_energy.total_cmp(&b.mean_energy))
                    .unwrap(),
                other => return Err(format!("unknown section kind '{other}'")),
            };
            let start = beat_at(g, sec.start_s).min(beat_at(g, sec.end_s));
            let end = beat_at(g, sec.end_s).max(start + 1);
            Ok((
                start,
                end - start,
                format!("section '{kind}' ({:.1}–{:.1} s)", sec.start_s, sec.end_s),
            ))
        }
        RegionSpec::Seconds { start, end } => {
            if !start.is_finite() || !end.is_finite() || end <= start {
                return Err("seconds need start < end".into());
            }
            // Seconds are exact: the beat count is provenance only.
            let beats = ((end - start) / g.interval).round().max(1.0) as i64;
            Ok((0, beats, format!("{start:.3}–{end:.3} s")))
        }
    }
}

fn total_beats(analysis: &Analysis, g: &Grid) -> i64 {
    if let Some(last) = g.beats.last() {
        return last.beat_index - g.first_index + 1;
    }
    (analysis.duration / g.interval) as i64
}

#[allow(clippy::too_many_arguments)]
fn apply_sub_range(
    spec: &RegionSpec,
    g: &Grid,
    rate: u32,
    start_beat: i64,
    count_beats: i64,
    why: String,
    analysis: &Analysis,
) -> Result<Resolved, String> {
    // Seconds are exact: they never go through the beat grid.
    if let RegionSpec::Seconds { start, end } = spec {
        let start_frame = (*start * rate as f64).round().max(0.0) as u64;
        let end_frame = (*end * rate as f64).round().max(0.0) as u64;
        return Ok(Resolved {
            rate,
            start_frame,
            end_frame,
            first_beat: *start,
            beats: count_beats as f64,
            bar: 0,
            phrase: None,
            source_bpm: analysis.bpm,
            source_key: analysis.key.map(|k| k.name()),
            confidence: g.confidence,
            why,
        });
    }
    let (from, to) = sub_range(spec).unwrap_or((0.0, count_beats as f64));
    if !(0.0..=count_beats as f64).contains(&from) || to <= from || to > count_beats as f64 {
        return Err(format!(
            "beat sub-range {from}…{to} is outside the region's {count_beats} beats"
        ));
    }
    let start_seconds = g.seconds(start_beat) + from * g.interval;
    let end_seconds = g.seconds(start_beat) + to * g.interval;
    let start_frame = (start_seconds * rate as f64).round().max(0.0) as u64;
    let end_frame = (end_seconds * rate as f64).round().max(0.0) as u64;
    if end_frame <= start_frame {
        return Err("the region is shorter than one frame".into());
    }
    let bar = g.bar_of(start_beat);
    Ok(Resolved {
        rate,
        start_frame,
        end_frame,
        first_beat: start_seconds,
        beats: to - from,
        bar,
        phrase: Some((start_beat - g.first_index).div_euclid(PHRASE_BEATS)),
        source_bpm: analysis.bpm,
        source_key: analysis.key.map(|k| k.name()),
        confidence: g.confidence,
        why,
    })
}

fn sub_range(spec: &RegionSpec) -> Option<(f64, f64)> {
    match spec {
        RegionSpec::Phrase { beats, .. }
        | RegionSpec::Bars { beats, .. }
        | RegionSpec::Beats { beats, .. }
        | RegionSpec::Section { beats, .. } => *beats,
        RegionSpec::Seconds { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use patch_analysis::key::Key;
    use patch_analysis::structure::{CoarseSection, StructureEvidence};
    use patch_analysis::tempo::{BeatEvent, RhythmEvidence};
    use patch_analysis::{Analysis, AnalyzerVersions, Source};

    const RATE: u32 = 48_000;

    /// A 128 BPM track, beats every 0.46875 s, bars of 4, from `first` beats.
    fn analysis_with(beats: usize) -> Analysis {
        let bpm = 128.0;
        let interval = 60.0 / bpm;
        let evs: Vec<BeatEvent> = (0..beats)
            .map(|i| BeatEvent {
                time_s: i as f64 * interval,
                beat_index: i as i64,
                interval_s: interval as f32,
                bar_index: Some(i as i64 / 4),
                beat_in_bar: Some((i % 4) as u8),
                onset_offset_s: Some(0.0),
                onset_support: 1.0,
            })
            .collect();
        Analysis {
            version: patch_analysis::VERSION,
            analyzers: AnalyzerVersions::CURRENT,
            human_grid: None,
            source: None,
            duration: beats as f64 * interval + 1.0,
            bpm: Some(bpm),
            beat_offset: 0.0,
            bpm_confidence: 0.9,
            key: Some(Key {
                tonic: 9,
                minor: true,
            }),
            key_confidence: 0.8,
            lufs: -8.0,
            lra: 4.0,
            true_peak: -0.3,
            waveform: vec![],
            downbeat: Some(0),
            downbeat_confidence: 0.8,
            energy: vec![],
            structure: Some(StructureEvidence {
                analyzer_version: patch_analysis::structure::ANALYZER_VERSION,
                boundaries: vec![],
                sections: vec![
                    CoarseSection {
                        start_s: 0.0,
                        end_s: 30.0,
                        mean_energy: 20.0,
                        onset_density: 0.4,
                    },
                    CoarseSection {
                        start_s: 30.0,
                        end_s: 60.0,
                        mean_energy: 150.0,
                        onset_density: 0.9,
                    },
                ],
            }),
            key_source: Source::Audio,
            key_detected: Some(Key {
                tonic: 9,
                minor: true,
            }),
            key_detected_confidence: 0.8,
            tag_bpm: None,
            grid: None,
            rhythm: Some(RhythmEvidence {
                analyzer_version: patch_analysis::tempo::RHYTHM_EVIDENCE_VERSION,
                beats: evs,
                regions: vec![],
                tempo_hypotheses: vec![],
                meter_hypotheses: vec![],
                selection_strength: None,
            }),
            deep_regions: vec![],
            harmony: None,
            vocals: None,
        }
    }

    #[test]
    fn beats_and_bars_land_on_exact_frames() {
        let a = analysis_with(128);
        let r = resolve(
            &RegionSpec::Beats {
                beat: 8,
                count: 2,
                beats: None,
            },
            &a,
            RATE,
        )
        .unwrap();
        let interval = 60.0 / 128.0;
        assert_eq!(r.start_frame, (8.0 * interval * RATE as f64).round() as u64);
        assert_eq!(r.beats, 2.0);
        assert_eq!(r.bar, 2);

        let bars = resolve(
            &RegionSpec::Bars {
                bar: 2,
                count: 1,
                beats: None,
            },
            &a,
            RATE,
        )
        .unwrap();
        assert_eq!(bars.start_frame, r.start_frame);
        assert_eq!(
            bars.end_frame - bars.start_frame,
            (4.0 * interval * RATE as f64).round() as u64
        );
    }

    #[test]
    fn a_beat_sub_range_cuts_inside_the_region() {
        let a = analysis_with(128);
        let r = resolve(
            &RegionSpec::Phrase {
                which: Phrase::Index(0),
                beats: Some((2.0, 4.0)),
            },
            &a,
            RATE,
        )
        .unwrap();
        assert_eq!(r.beats, 2.0);
        let interval = 60.0 / 128.0;
        assert_eq!(r.start_frame, (2.0 * interval * RATE as f64).round() as u64);
    }

    #[test]
    fn before_the_drop_is_the_phrase_ending_at_it() {
        let a = analysis_with(128);
        let r = resolve(
            &RegionSpec::Phrase {
                which: Phrase::BeforeDrop,
                beats: None,
            },
            &a,
            RATE,
        )
        .unwrap();
        assert_eq!(r.beats, 32.0);
        // The drop is at 30 s; the boundary rounded to a phrase is beat 64
        // (30/0.46875 = 64 exactly); the phrase before it starts at beat 32.
        let interval = 60.0 / 128.0;
        assert_eq!(
            r.start_frame,
            (32.0 * interval * RATE as f64).round() as u64
        );
        assert!(r.why.contains("before the drop"), "{}", r.why);
    }

    #[test]
    fn a_missing_grid_is_refused_with_a_reason() {
        let mut a = analysis_with(8);
        a.rhythm = None;
        let e = resolve(
            &RegionSpec::Bars {
                bar: 0,
                count: 1,
                beats: None,
            },
            &a,
            RATE,
        )
        .unwrap_err();
        assert!(e.contains("beat grid"), "{e}");

        let mut a = analysis_with(8);
        a.bpm_confidence = 0.1;
        let e = resolve(
            &RegionSpec::Bars {
                bar: 0,
                count: 1,
                beats: None,
            },
            &a,
            RATE,
        )
        .unwrap_err();
        assert!(e.contains("uncertain"), "{e}");
    }

    #[test]
    fn seconds_are_exact_frames() {
        let a = analysis_with(8);
        let r = resolve(
            &RegionSpec::Seconds {
                start: 1.5,
                end: 2.25,
            },
            &a,
            RATE,
        )
        .unwrap();
        assert_eq!(r.start_frame, 72_000);
        assert_eq!(r.end_frame, 108_000);
    }
}
