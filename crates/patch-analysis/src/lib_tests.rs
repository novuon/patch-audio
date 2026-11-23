/// A kick every beat and a *louder* attack on the off-beat: the
/// detector's first-pass phase can lock to the off-beat (in club music
/// the open hat is the loudest attack there).
fn offbeat_hat_track(bpm: f64, seconds: f64) -> Vec<f32> {
    let rate = RATE as f64;
    let n = (rate * seconds) as usize;
    let mut x = vec![0.0f32; n * 2];
    let beat = 60.0 / bpm;
    let mut place = |at: f64, amp: f64, hz: f64, decay: f64| {
        let start = (at * rate) as usize;
        if start >= n {
            return;
        }
        let len = ((decay * 4.0 * rate) as usize).min(n - start);
        for i in 0..len {
            let t = i as f64 / rate;
            let v = (amp * (-t / decay).exp() * (2.0 * std::f64::consts::PI * hz * t).sin())
                as f32;
            x[start * 2 + i * 2] += v;
            x[start * 2 + i * 2 + 1] += v;
        }
    };
    let mut t = 0.12;
    while t < seconds - 0.5 {
        place(t, 0.5, 65.0, 0.06);
        place(t + beat / 2.0, 0.9, 8_000.0, 0.03);
        t += beat;
    }
    x
}

#[test]
fn recovering_dj_arrays_keeps_the_saved_tempo_grid_and_key() {
    let pcm = offbeat_hat_track(128.0, 12.0);
    let mut saved = analyse(&pcm);
    saved.bpm = Some(126.0);
    saved.beat_offset = 0.23;
    saved.downbeat = Some(2);
    saved.harmony = None;
    saved.waveform.clear();
    saved.rhythm.as_mut().unwrap().beats.clear();
    let before = saved.clone();

    restore_decision_arrays(&mut saved, &pcm);

    assert_eq!(saved.bpm, before.bpm);
    assert_eq!(saved.beat_offset, before.beat_offset);
    assert_eq!(saved.downbeat, before.downbeat);
    assert_eq!(saved.grid, before.grid);
    assert_eq!(saved.key, before.key);
    assert_eq!(saved.lufs, before.lufs);
    assert_eq!(saved.structure, before.structure);
    assert!(saved.harmony.is_some());
    assert!(!saved.waveform.is_empty());
    let beats = &saved.rhythm.as_ref().unwrap().beats;
    assert!(!beats.is_empty());
    for beat in beats {
        let phase = (beat.time_s - 0.23) / (60.0 / 126.0);
        assert!((phase - phase.round()).abs() < 1e-6);
    }
}

/// The stored tempo and first beat must be the measured grid's, the same
/// reading `beats`, bars and `use_grid()` use. The detector's own phase
/// can sit on the off-beat; storing it left the cached grid half a beat
/// from the beats it describes (found by ear: 09/51 clicks on the hat).
#[test]
fn the_stored_first_beat_follows_the_grid_not_the_loudest_attack() {
    let a = analyse(&offbeat_hat_track(124.0, 24.0));
    let (Some(g), Some(bpm)) = (a.grid.as_ref(), a.bpm) else {
        return;
    };
    if (g.bpm / bpm - 1.0).abs() >= 0.01 {
        return;
    }
    assert!(
        (a.beat_offset - g.offset).abs() < 0.02,
        "stored first beat {} s, grid {} s",
        a.beat_offset,
        g.offset
    );
    let first = a.rhythm.as_ref().expect("rhythm").beats[0].time_s;
    assert!(
        (first - a.beat_offset).abs() < 0.03,
        "first beat {first} s, stored {} s",
        a.beat_offset
    );
}

#[test]
fn matching_tempo_tag_seeds_verification_but_disagreement_does_not() {
    let mut analysis = analyse(&vec![0.0; RATE as usize * 2]);
    analysis.bpm = Some(130.0);
    analysis.bpm_confidence = 0.29;
    analysis.tag_bpm = Some(130.0);
    assert_eq!(grid_measure_bpm(&analysis), Some(130.0));

    analysis.tag_bpm = Some(135.0);
    assert_eq!(grid_measure_bpm(&analysis), None);

    analysis.bpm_confidence = 0.31;
    analysis.tag_bpm = None;
    assert_eq!(grid_measure_bpm(&analysis), Some(130.0));
}

#[test]
fn shared_cache_survives_large_working_sets_and_invalidates_replacements() {
    let root = std::env::temp_dir().join(format!("patch-analysis-memo-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let mut analysis = analyse(&vec![0.0; RATE as usize * 2]);
    analysis.lufs = -16.0;
    analysis.lra = 0.0;
    analysis.true_peak = -3.0;
    let text = serde_json::to_string(&analysis).unwrap();
    let mut held = Vec::new();
    for i in 0..100 {
        let p = root.join(format!("{i}.json"));
        std::fs::write(&p, &text).unwrap();
        held.push(cached_file(&p).unwrap());
    }
    for (i, a) in held.iter().enumerate() {
        assert!(std::sync::Arc::ptr_eq(
            a,
            &cached_file(&root.join(format!("{i}.json"))).unwrap()
        ));
    }
    let p = root.join("0.json");
    let mut changed = analysis.clone();
    changed.duration += 1234.0;
    std::fs::write(&p, serde_json::to_string(&changed).unwrap()).unwrap();
    assert_eq!(cached_file(&p).unwrap().duration, changed.duration);
    assert_eq!(held[0].duration, analysis.duration);
    let compact = analysis.for_dj();
    let mut restored = compact.clone();
    restored.waveform = analysis.waveform.clone();
    assert_eq!(restored, analysis);
    assert!(compact.waveform.is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn the_cache_follows_content_not_the_path() {
    let dir = std::env::temp_dir().join(format!("patch-cachekey-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("moved")).unwrap();
    let a = dir.join("a.mp3");
    std::fs::write(
        &a,
        (0..400_000u32)
            .map(|i| (i * 7 % 251) as u8)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let key = cache_path(&a).unwrap();
    let copy = dir.join("moved/renamed.mp3");
    std::fs::copy(&a, &copy).unwrap();
    assert_eq!(
        cache_path(&copy).unwrap(),
        key,
        "a copy or move finds the same entry"
    );
    let other = dir.join("b.mp3");
    std::fs::write(&other, vec![1u8; 400_000]).unwrap();
    assert_ne!(
        cache_path(&other).unwrap(),
        key,
        "other content, other entry"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

use super::*;

#[test]
fn any_stale_analyzer_puts_a_track_back_in_the_library_queue() {
    // The library queues by generation: a record whose analyzers are not
    // all current must read as older, or cache reads would reject it with
    // nothing queued to refresh it.
    let mut a = analyse(&vec![0.0; RATE as usize * 2]);
    assert_eq!(a.generation(), GENERATION);
    a.analyzers.beat -= 1;
    assert!(a.generation() < GENERATION);
    a.analyzers = AnalyzerVersions::CURRENT;
    a.version -= 1;
    assert!(a.generation() < GENERATION);
}

#[test]
fn match_gain_respects_headroom() {
    let a = Analysis {
        version: VERSION,
        analyzers: AnalyzerVersions::CURRENT,
        human_grid: None,
        source: None,
        duration: 1.0,
        bpm: None,
        beat_offset: 0.0,
        bpm_confidence: 0.0,
        key: None,
        key_confidence: 0.0,
        key_source: Source::Audio,
        key_detected: None,
        key_detected_confidence: 0.0,
        tag_bpm: None,
        lufs: -16.0,
        lra: 0.0,
        true_peak: -3.0,
        waveform: vec![],
        downbeat: None,
        downbeat_confidence: 0.0,
        energy: vec![],
        grid: None,
        rhythm: None,
        deep_regions: Vec::new(),
        structure: None,
        harmony: None,
        vocals: None,
    };
    // Wants +5 dB to reach −11, but only 2.5 dB of headroom below −0.5 dBTP.
    assert!((a.match_gain(-11.0, -0.5, 6.0) - 2.5).abs() < 1e-9);
    let loud = Analysis {
        lufs: -7.0,
        true_peak: 0.5,
        ..a
    };
    assert!(
        (loud.match_gain(-11.0, -0.5, 6.0) + 4.0).abs() < 1e-9,
        "cuts a hot master"
    );
}

#[test]
fn human_grid_overrides_runtime_machine_grid_but_is_not_written_to_analysis_cache() {
    let mut analysis = Analysis {
        version: VERSION,
        analyzers: AnalyzerVersions::CURRENT,
        source: None,
        human_grid: Some(HumanGridOverride {
            bpm: 125.0,
            offset: 0.25,
            beats_per_bar: None,
            downbeat_phase: None,
            segments: vec![],
        }),
        duration: 1.0,
        bpm: Some(120.0),
        beat_offset: 0.1,
        bpm_confidence: 0.7,
        key: None,
        key_confidence: 0.0,
        lufs: -14.0,
        lra: 0.0,
        true_peak: -3.0,
        waveform: vec![],
        downbeat: Some(0),
        downbeat_confidence: 0.8,
        energy: vec![],
        key_source: Source::Audio,
        key_detected: None,
        key_detected_confidence: 0.0,
        tag_bpm: None,
        grid: Some(crate::grid::GridQuality {
            click_offset_ms: None,
            method: crate::grid::METHOD,
            precise: true,
            bpm: 120.0,
            offset: 0.1,
            downbeat: Some(0),
            downbeat_confidence: 0.8,
            jitter_ms: 1.0,
            drift_ms: 2.0,
            coverage: 0.9,
            beatless: vec![],
            mixability: 0.9,
            phase_confidence: 0.8,
        }),
        rhythm: None,
        deep_regions: vec![],
        structure: None,
        harmony: None,
        vocals: None,
    };

    assert!(analysis.use_grid());
    assert_eq!(analysis.bpm, Some(125.0));
    assert_eq!(analysis.beat_offset, 0.25);
    assert_eq!(analysis.bpm_confidence, 1.0);
    assert_eq!(
        analysis.downbeat, None,
        "old machine bar phase is not reused"
    );
    assert!(analysis.grid.is_none(), "old kick metrics are not reused");
    let cached = serde_json::to_value(&analysis).unwrap();
    assert!(cached.get("human_grid").is_none());
    let restored: Analysis = serde_json::from_value(cached).unwrap();
    assert_eq!(restored.human_grid, None);
}

#[test]
fn human_bar_phase_is_used_only_when_the_planner_supports_that_meter() {
    let mut analysis = Analysis {
        version: VERSION,
        analyzers: AnalyzerVersions::CURRENT,
        source: None,
        human_grid: Some(HumanGridOverride {
            bpm: 125.0,
            offset: 0.25,
            beats_per_bar: Some(4),
            downbeat_phase: Some(2),
            segments: vec![],
        }),
        duration: 30.0,
        bpm: Some(120.0),
        beat_offset: 0.1,
        bpm_confidence: 0.7,
        key: None,
        key_confidence: 0.0,
        lufs: -14.0,
        lra: 0.0,
        true_peak: -3.0,
        waveform: vec![],
        downbeat: None,
        downbeat_confidence: 0.0,
        energy: vec![],
        key_source: Source::Audio,
        key_detected: None,
        key_detected_confidence: 0.0,
        tag_bpm: None,
        grid: None,
        rhythm: None,
        deep_regions: vec![],
        structure: None,
        harmony: None,
        vocals: None,
    };
    assert!(analysis.use_grid());
    assert_eq!(analysis.downbeat, Some(2));
    assert_eq!(analysis.downbeat_confidence, 1.0);

    analysis.human_grid = Some(HumanGridOverride {
        beats_per_bar: Some(7),
        downbeat_phase: Some(6),
        ..analysis.human_grid.unwrap()
    });
    assert!(analysis.use_grid());
    assert_eq!(analysis.downbeat, None, "7/8 is not forced into 4/4");
    assert_eq!(analysis.downbeat_confidence, 0.0);
}

#[test]
fn human_tempo_map_provides_local_tempo_for_phrase_timing() {
    let mut analysis = Analysis {
        version: VERSION,
        analyzers: AnalyzerVersions::CURRENT,
        source: None,
        human_grid: Some(HumanGridOverride {
            bpm: 120.0,
            offset: 0.0,
            beats_per_bar: Some(4),
            downbeat_phase: Some(0),
            segments: vec![HumanTempoSegment {
                start_s: 0.0,
                end_s: Some(60.0),
                bpm_start: 120.0,
                bpm_end: 130.0,
            }],
        }),
        duration: 60.0,
        bpm: Some(120.0),
        beat_offset: 0.0,
        bpm_confidence: 0.8,
        key: None,
        key_confidence: 0.0,
        lufs: -14.0,
        lra: 0.0,
        true_peak: -3.0,
        waveform: vec![],
        downbeat: None,
        downbeat_confidence: 0.0,
        energy: vec![],
        key_source: Source::Audio,
        key_detected: None,
        key_detected_confidence: 0.0,
        tag_bpm: None,
        grid: None,
        rhythm: None,
        deep_regions: vec![],
        structure: None,
        harmony: None,
        vocals: None,
    };
    assert!(analysis.use_grid());
    assert_eq!(analysis.bpm_at(0.0), Some(120.0));
    assert_eq!(analysis.bpm_at(30.0), Some(125.0));
    assert_eq!(analysis.bpm_at(59.0), Some(129.83333333333334));
    assert_eq!(analysis.bpm_at(60.0), Some(120.0));

    let mut machine = analysis.clone();
    machine.human_grid = None;
    machine.rhythm = Some(tempo::RhythmEvidence {
        analyzer_version: tempo::RHYTHM_EVIDENCE_VERSION,
        beats: vec![],
        regions: vec![tempo::RhythmRegion {
            start_s: 0.0,
            end_s: 30.0,
            beat_count: 64,
            observed_onset_coverage: 0.9,
            mean_onset_support: 1.0,
            onset_jitter_ms: 3.0,
            phase_spread_ms: 6.0,
            local_bpm: Some(128.0),
            tempo_fit_residual_ms: Some(5.0),
        }],
        tempo_hypotheses: vec![],
        meter_hypotheses: vec![],
        selection_strength: None,
    });
    assert_eq!(machine.bpm_at(15.0), Some(128.0));
    assert_eq!(machine.bpm_at(35.0), Some(120.0));
}

#[test]
fn stale_rhythm_refresh_does_not_recompute_key_or_loudness_groups() {
    let pcm = vec![0.0; RATE as usize * 8];
    let mut analysis = analyse(&pcm);
    let retained_key = key::Key {
        tonic: 7,
        minor: true,
    };
    analysis.key = Some(retained_key);
    analysis.key_detected = Some(retained_key);
    analysis.lufs = -17.25;
    analysis.analyzers.beat = 0;
    analysis.analyzers.structure = 0;
    analysis.structure = None;
    let beat = analysis.bpm;
    let refreshed_scores = refresh_stale(&mut analysis, &pcm);
    assert_eq!(analysis.analyzers, AnalyzerVersions::CURRENT);
    assert_eq!(
        analysis.structure.as_ref().map(|s| s.analyzer_version),
        Some(structure::ANALYZER_VERSION)
    );
    assert_eq!(analysis.key, Some(retained_key));
    assert_eq!(analysis.lufs, -17.25);
    assert_eq!(analysis.bpm, beat);
    assert!(refreshed_scores.is_empty());
}

#[test]
fn stale_key_refresh_preserves_beat_sequence_and_levels() {
    let pcm = vec![0.0; RATE as usize * 8];
    let mut analysis = analyse(&pcm);
    let rhythm = analysis.rhythm.clone();
    analysis.lufs = -19.0;
    analysis.analyzers.key = 0;
    let bpm = analysis.bpm;
    refresh_stale(&mut analysis, &pcm);
    assert_eq!(analysis.analyzers, AnalyzerVersions::CURRENT);
    assert_eq!(analysis.bpm, bpm);
    assert_eq!(analysis.rhythm, rhythm);
    assert_eq!(analysis.lufs, -19.0);
}

#[test]
fn stale_harmony_refresh_preserves_the_independent_beat_and_loudness_groups() {
    let pcm = vec![0.0; RATE as usize * 8];
    let mut analysis = analyse(&pcm);
    let rhythm = analysis.rhythm.clone();
    let bpm = analysis.bpm;
    analysis.lufs = -19.0;
    analysis.analyzers.harmony = 0;
    analysis.harmony = None;
    refresh_stale(&mut analysis, &pcm);
    assert_eq!(analysis.analyzers, AnalyzerVersions::CURRENT);
    assert_eq!(analysis.bpm, bpm);
    assert_eq!(analysis.rhythm, rhythm);
    assert_eq!(analysis.lufs, -19.0);
    assert!(analysis.harmony.is_some());
}

#[test]
fn stale_analyzer_records_are_not_exposed_as_current_cache_hits() {
    let file = std::env::temp_dir().join(format!(
        "patch-analysis-stale-{}-{}.json",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let pcm: Vec<f32> = (0..RATE as usize * 10)
        .flat_map(|i| {
            let sample = (std::f32::consts::TAU * 440.0 * i as f32 / RATE as f32).sin() * 0.1;
            [sample, sample]
        })
        .collect();
    let mut analysis = analyse(&pcm);
    analysis.analyzers.key = 0;
    std::fs::write(&file, serde_json::to_vec(&analysis).unwrap()).unwrap();
    let parsed: Analysis = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(parsed.version, VERSION);
    assert!(cached_file(&file).is_none());
    assert!(
        cached_file_any(&file).is_some(),
        "schema={}, stamp={:?}, file={file:?}",
        parsed.version,
        file_stamp(&file)
    );
    let _ = std::fs::remove_file(file);
}
