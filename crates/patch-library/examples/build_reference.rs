use patch_library::{
    AnalysisProvenance, BeatSegment, Cue, EmbeddingVector, Facts, Package, PackageMode, SavedLoop,
    TransitionPlan, TransitionStrategy,
};
use std::path::{Path, PathBuf};

fn wav(path: &Path, bpm: f64, rate: u32) -> std::io::Result<()> {
    let frames = rate as usize;
    let mut data = Vec::with_capacity(frames * 4);
    for i in 0..frames {
        let t = i as f64 / rate as f64;
        let beat = (t * bpm / 60.0 * std::f64::consts::TAU).sin();
        let tone = (t * 440.0 * std::f64::consts::TAU).sin() * 0.18 + beat * 0.12;
        let s = (tone.clamp(-1.0, 1.0) * i16::MAX as f64) as i16;
        data.extend_from_slice(&s.to_le_bytes());
        data.extend_from_slice(&s.to_le_bytes());
    }
    let mut out = Vec::with_capacity(44 + data.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    std::fs::write(path, out)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("docs/patch-dj-format/fixtures/reference.patchdj"));
    if root.exists() {
        return Err(format!("refusing to overwrite {}", root.display()).into());
    }
    let temp = std::env::temp_dir().join(format!("patchdj-fixture-{}", std::process::id()));
    std::fs::create_dir_all(&temp)?;
    let a = temp.join("variable-tempo.wav");
    let b = temp.join("steady-126.wav");
    wav(&a, 122.0, 44_100)?;
    wav(&b, 126.0, 44_100)?;
    let p = Package::create(&root, "Patch DJ Reference Set", PackageMode::Set)?;
    let facts = |title: &str, bpm: f64| Facts {
        title: title.into(),
        artist: "Patch Format Test Ensemble".into(),
        album: "Reference Fixture".into(),
        genre: "test audio".into(),
        codec: "pcm_s16le".into(),
        container: "wav".into(),
        rate: 44_100,
        bits: 16,
        channels: 2,
        duration: 1.0,
        original_tags: serde_json::json!({"fixture":true,"intended_bpm":bpm}),
        ..Default::default()
    };
    let (ta, _) = p.add_audio(&a, |_| Some(facts("Variable Tempo Study", 122.0)), true)?;
    let (tb, _) = p.add_audio(&b, |_| Some(facts("Steady 126", 126.0)), true)?;
    let analysis_a = serde_json::json!({"bpm":122.0,"lufs":-17.8,"energy":0.43,"key":{"tonic":9,"minor":true},"tempo_map":[{"start_sample":0,"bpm":120.0},{"start_sample":22050,"bpm":124.0}],"confidence":0.84});
    let analysis_b = serde_json::json!({"bpm":126.0,"lufs":-18.2,"energy":0.55,"key":{"tonic":4,"minor":false},"confidence":0.91});
    let provenance = AnalysisProvenance {
        analyzer_id: "org.patch.reference-analyzer".into(),
        analyzer_version: "1.0.0".into(),
        schema_version: 1,
        created_at: "2026-01-01T12:00:00Z".into(),
        confidence: Some(0.84),
        input_fingerprint: "fixture-generated-audio".into(),
        parameters: serde_json::json!({"window_seconds":4}),
    };
    for (id, doc) in [(ta, &analysis_a), (tb, &analysis_b)] {
        p.library.set_analysis(id, 1, doc)?;
        p.library
            .store_analysis_result(id, "track_analysis", &provenance, doc)?;
    }
    p.library.put_cue(
        ta,
        &Cue {
            id: "cue-hot-drop".into(),
            kind: "hot".into(),
            sample_position: 22_050,
            sample_rate: 44_100,
            beat_position: Some(1.0),
            label: "Drop".into(),
            color: Some("#ff6a3d".into()),
            comment: "performer-approved cue".into(),
            origin: "performer".into(),
            confidence: None,
            generated: false,
        },
    )?;
    p.library.put_cue(
        ta,
        &Cue {
            id: "cue-memory-break".into(),
            kind: "memory".into(),
            sample_position: 33_075,
            sample_rate: 44_100,
            beat_position: Some(1.5),
            label: "Break".into(),
            color: None,
            comment: String::new(),
            origin: "performer".into(),
            confidence: None,
            generated: false,
        },
    )?;
    p.library.put_loop(
        ta,
        &SavedLoop {
            id: "loop-four-beats".into(),
            start_sample: 22_050,
            end_sample: 57_600,
            sample_rate: 44_100,
            beat_length: Some(4.0),
            label: "Four-beat roll".into(),
            quantized: true,
            temporary: false,
            origin: "performer".into(),
        },
    )?;
    let automatic = BeatSegment {
        start_sample: 0,
        end_sample: Some(44_100),
        bpm_start: 120.0,
        bpm_end: 124.0,
        first_beat_sample: 0,
        beats_per_bar: Some(4),
        downbeat_phase: Some(0),
        confidence: Some(0.84),
    };
    let correction = BeatSegment {
        start_sample: 0,
        end_sample: Some(44_100),
        bpm_start: 122.0,
        bpm_end: 122.0,
        first_beat_sample: 150,
        beats_per_bar: Some(4),
        downbeat_phase: Some(0),
        confidence: None,
    };
    p.library.put_beatgrid(
        ta,
        "grid-auto",
        "automatic",
        Some(&provenance),
        Some((4, 4)),
        &[automatic],
        false,
    )?;
    p.library.put_beatgrid(
        ta,
        "grid-human",
        "human",
        None,
        Some((7, 8)),
        &[correction],
        true,
    )?;
    p.write_waveform(
        ta,
        &patch_library::waveform::Waveform {
            sample_rate: 44_100,
            channels: 2,
            levels: vec![
                patch_library::waveform::Level {
                    bins: (0..128)
                        .map(|i| {
                            let v = ((i as f32 * 0.17).sin() * 0.35).abs();
                            [-v, v, v * 0.65]
                        })
                        .collect(),
                },
                patch_library::waveform::Level {
                    bins: vec![[-0.4, 0.4, 0.22]; 32],
                },
            ],
        },
    )?;
    p.library.store_embedding(
        ta,
        "org.patch.embedding",
        "demo-1",
        "semantic",
        &EmbeddingVector::I8 {
            scale: 0.01,
            values: vec![12, -7, 43, 2, -18, 5, 9, 31],
        },
        "l2",
        "generated-analysis-v1",
    )?;
    let playlist = p.library.create_playlist("Reference tracks", None)?;
    p.library.add_to_playlist(playlist, ta)?;
    p.library.add_to_playlist(playlist, tb)?;
    p.library.save_set("set-friday","Friday at the Lab",&serde_json::json!({"schema_version":1,"track_order":[ta,tb],"alternates":[tb],"notes":"Generated fixture; safe to copy and edit.","energy_target":[0.4,0.6],"transitions":["transition-a-b"]}))?;
    let plan = TransitionPlan {
        source_track: ta,
        destination_track: tb,
        source_mix_out_sample: 39_690,
        destination_mix_in_sample: 0,
        duration_samples: 35_280,
        sample_rate: 44_100,
        strategy: TransitionStrategy {
            phrase_alignment_beats: Some(4),
            tempo_ratio: Some(126.0 / 122.0),
            key_relation: Some("compatible".into()),
            energy_delta: Some(0.12),
            fader_curve: vec![[0.0, 0.0], [0.4, 0.5], [0.8, 1.0]],
            ..Default::default()
        },
        confidence: Some(0.78),
        notes: "structured performer-reviewed transition".into(),
    };
    p.library.save_transition(
        "transition-a-b",
        &plan,
        "planned",
        &serde_json::json!({"origin":"fixture","model":"none"}),
    )?;
    p.library.log_session_event(
        "session-reference",
        "2026-01-01T20:00:00Z",
        "track_loaded",
        Some(ta),
        Some("a"),
        Some(0),
        &serde_json::json!({"deck":"A"}),
    )?;
    p.library.log_session_event(
        "session-reference",
        "2026-01-01T20:00:03Z",
        "transition_completed",
        Some(tb),
        Some("b"),
        Some(44100),
        &serde_json::json!({"transition":"transition-a-b"}),
    )?;
    p.library.set_extension(
        "org.patch.reference-fixture",
        1,
        &serde_json::json!({"fixture_version":1,"non_executable":true}),
    )?;
    p.checkpoint()?;
    let report = p.validate();
    if !report.is_valid() {
        return Err(format!("fixture validation failed: {:?}", report.errors).into());
    }
    drop(p);
    let reopened = Package::open(&root)?;
    let check = reopened.validate();
    if !check.is_valid() {
        return Err(format!("reopen validation failed: {:?}", check.errors).into());
    }
    std::fs::remove_dir_all(temp)?;
    println!("created {}: {} tracks", root.display(), check.tracks);
    Ok(())
}
