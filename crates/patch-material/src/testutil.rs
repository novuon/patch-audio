//! Synthetic analyses for tests (no files, no decoding).

use patch_analysis::key::Key;
use patch_analysis::structure::{CoarseSection, StructureEvidence};
use patch_analysis::tempo::{BeatEvent, RhythmEvidence};
use patch_analysis::{Analysis, AnalyzerVersions, Source};

/// A steady 4/4 track: `bpm`, `beats` beats from 0, sections as given.
pub fn analysis(bpm: f64, beats: usize, sections: Vec<CoarseSection>) -> Analysis {
    let interval = 60.0 / bpm;
    let evs: Vec<BeatEvent> = (0..beats)
        .map(|i| BeatEvent {
            time_s: i as f64 * interval,
            beat_index: i as i64,
            interval_s: interval as f32,
            bar_index: Some(i as i64 / 4),
            beat_in_bar: Some((i % 4) as u8),
            onset_offset_s: Some(0.0),
            onset_support: if i % 4 == 0 { 1.0 } else { 0.5 },
        })
        .collect();
    let duration = beats as f64 * interval + 1.0;
    Analysis {
        version: patch_analysis::VERSION,
        analyzers: AnalyzerVersions::CURRENT,
        human_grid: None,
        source: None,
        duration,
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
            sections,
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

pub fn section(start_s: f64, end_s: f64, energy: f32) -> CoarseSection {
    CoarseSection {
        start_s,
        end_s,
        mean_energy: energy,
        onset_density: 0.5,
    }
}
