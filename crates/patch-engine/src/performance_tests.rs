use super::*;
use patch_core::performance::{
    ActiveLayer, MaterialKind, PerformanceSource, PerformanceState, PerformanceTempoSegment,
};
use patch_core::stems::{StemArtifact, StemSet};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn live_source_target_maps_to_engine_sample_clock() {
    assert_eq!(
        source_target_sample(1_000.0, 1.25, 4_000, 1_125.0),
        Ok(4_100)
    );
    assert!(source_target_sample(1_125.0, 1.0, 4_000, 1_125.0).is_err());
    assert!(source_target_sample(1_000.0, 0.0, 4_000, 1_125.0).is_err());
}

#[test]
fn live_source_keeps_its_physical_deck_and_releases_the_logical_lane() {
    let make_lane = |lane, source_id: &str, path: &str| EntryLane {
        lane,
        source_id: source_id.into(),
        path: PathBuf::from(path),
        source_start_s: 10.0,
        source_end_s: 40.0,
        tempo_adjustment: 0.0,
        original_gain: 1.0,
        starts_at_entry: true,
        live: false,
        start_offset_samples: 0,
        stem_gains: BTreeMap::new(),
        stem_set_id: None,
    };
    let mut lanes = vec![
        make_lane(0, "playing", "/music/playing.flac"),
        make_lane(1, "prepared", "/music/prepared.flac"),
    ];
    bind_live_entry_lanes(
        &mut lanes,
        &[None, Some("/music/playing.flac".into())],
        &BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(lanes[0].lane, 1);
    assert!(lanes[0].live);
    assert_eq!(lanes[1].lane, 0);
    assert!(!lanes[1].live);
}

#[test]
fn live_c_deck_moves_the_next_source_to_d() {
    let mut current = EntryLane {
        lane: 0,
        source_id: "current".into(),
        path: PathBuf::from("/music/current.flac"),
        source_start_s: 10.0,
        source_end_s: 40.0,
        tempo_adjustment: 0.0,
        original_gain: 1.0,
        starts_at_entry: true,
        live: false,
        start_offset_samples: 0,
        stem_gains: BTreeMap::new(),
        stem_set_id: None,
    };
    let next = EntryLane {
        lane: 2,
        source_id: "next".into(),
        path: PathBuf::from("/music/next.flac"),
        source_start_s: 0.0,
        source_end_s: 30.0,
        tempo_adjustment: 0.0,
        original_gain: 0.0,
        starts_at_entry: false,
        live: false,
        start_offset_samples: 0,
        stem_gains: BTreeMap::new(),
        stem_set_id: None,
    };
    let mut lanes = vec![current.clone(), next];
    bind_live_entry_lanes(
        &mut lanes,
        &[None, None, Some("/music/current.flac".into()), None],
        &BTreeSet::new(),
    )
    .unwrap();
    current = lanes[0].clone();
    assert_eq!(current.lane, 2);
    assert!(current.live);
    assert_eq!(lanes[1].lane, 3);
}

#[test]
fn future_sources_skip_decks_with_human_owned_stems() {
    let entry = EntryLane {
        lane: 0,
        source_id: "entry".into(),
        path: PathBuf::from("/music/entry.flac"),
        source_start_s: 10.0,
        source_end_s: 40.0,
        tempo_adjustment: 0.0,
        original_gain: 1.0,
        starts_at_entry: true,
        live: false,
        start_offset_samples: 0,
        stem_gains: BTreeMap::new(),
        stem_set_id: None,
    };
    let future = EntryLane {
        lane: 2,
        source_id: "future".into(),
        path: PathBuf::from("/music/future.flac"),
        source_start_s: 0.0,
        source_end_s: 30.0,
        tempo_adjustment: 0.0,
        original_gain: 0.0,
        starts_at_entry: false,
        live: false,
        start_offset_samples: 0,
        stem_gains: BTreeMap::new(),
        stem_set_id: None,
    };
    let mut lanes = vec![entry.clone(), future.clone()];
    bind_live_entry_lanes(&mut lanes, &[None, None, None, None], &BTreeSet::from([2])).unwrap();
    assert_eq!(lanes[1].lane, 3);

    let mut lanes = vec![entry, future];
    let protected = BTreeSet::from([2, 3]);
    assert!(
        bind_live_entry_lanes(&mut lanes, &[None, None, None, None], &protected)
            .unwrap_err()
            .contains("no free engine lane")
    );
}

fn fixture(path: &Path) -> PerformanceGraph {
    PerformanceGraph {
        entry: "start".into(),
        tempo: vec![PerformanceTempoSegment {
            start_beat: 0.0,
            end_beat: None,
            bpm: 120.0,
        }],
        sources: vec![PerformanceSource {
            id: "track-a".into(),
            content_hash: Some(patch_stems::hash_file(path).unwrap()),
            tempo_bpm: Some(120.0),
            duration_s: 60.0,
        }],
        states: vec![PerformanceState {
            id: "start".into(),
            at_beat: 0.0,
            layers: vec![ActiveLayer {
                id: "original".into(),
                source_id: "track-a".into(),
                material: MaterialKind::OriginalMix,
                stem_set_id: None,
                source_start_s: 0.0,
                source_end_s: 60.0,
                gain: 1.0,
            }],
        }],
        edges: vec![],
    }
}

fn layer(id: &str, source_id: &str, material: MaterialKind) -> ActiveLayer {
    ActiveLayer {
        id: id.into(),
        source_id: source_id.into(),
        material,
        stem_set_id: None,
        source_start_s: 0.0,
        source_end_s: 60.0,
        gain: 1.0,
    }
}

fn lane_graph() -> PerformanceGraph {
    use patch_core::performance::{
        ArrangementOperation as Op, PerformanceEdge, PerformanceTempoSegment,
    };
    let a = layer("a-original", "a", MaterialKind::OriginalMix);
    let a_drums = ActiveLayer {
        id: "a-drums".into(),
        source_id: "a".into(),
        material: MaterialKind::Stem("drums".into()),
        stem_set_id: Some("set-a".into()),
        ..a.clone()
    };
    let b = layer("b-original", "b", MaterialKind::OriginalMix);
    let c = layer("c-original", "c", MaterialKind::OriginalMix);
    let d = layer("d-original", "d", MaterialKind::OriginalMix);
    let sources = ["a", "b", "c", "d"]
        .into_iter()
        .map(|id| PerformanceSource {
            id: id.into(),
            content_hash: Some(format!("hash-{id}")),
            tempo_bpm: Some(120.0),
            duration_s: 60.0,
        })
        .collect();
    let state = |id: &str, at_beat: f64, layers| PerformanceState {
        id: id.into(),
        at_beat,
        layers,
    };
    PerformanceGraph {
        entry: "a".into(),
        tempo: vec![PerformanceTempoSegment {
            start_beat: 0.0,
            end_beat: None,
            bpm: 120.0,
        }],
        sources,
        states: vec![
            state("a", 0.0, vec![a.clone()]),
            state("ab", 4.0, vec![a.clone(), b.clone()]),
            state("ab-stems", 8.0, vec![a_drums.clone(), b.clone()]),
            state("abc", 12.0, vec![a_drums.clone(), b.clone(), c.clone()]),
            state("abc-clean", 16.0, vec![b.clone(), c.clone()]),
            state("bcd", 20.0, vec![b, c, d.clone()]),
        ],
        edges: vec![
            PerformanceEdge {
                from: "a".into(),
                to: "ab".into(),
                at_beat: 4.0,
                operation: Op::IntroduceLayer {
                    layer_id: "b-original".into(),
                },
            },
            PerformanceEdge {
                from: "ab".into(),
                to: "ab-stems".into(),
                at_beat: 8.0,
                operation: Op::ReplaceLayer {
                    remove_id: "a-original".into(),
                    introduce_id: "a-drums".into(),
                },
            },
            PerformanceEdge {
                from: "ab-stems".into(),
                to: "abc".into(),
                at_beat: 12.0,
                operation: Op::IntroduceLayer {
                    layer_id: "c-original".into(),
                },
            },
            PerformanceEdge {
                from: "abc".into(),
                to: "abc-clean".into(),
                at_beat: 16.0,
                operation: Op::RemoveLayer {
                    layer_id: "a-drums".into(),
                },
            },
            PerformanceEdge {
                from: "abc-clean".into(),
                to: "bcd".into(),
                at_beat: 20.0,
                operation: Op::IntroduceLayer {
                    layer_id: "d-original".into(),
                },
            },
        ],
    }
}

fn temp_file() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("patch-performance-{nonce}.audio"));
    std::fs::write(&path, b"source bytes").unwrap();
    path
}

fn write_test_wav(path: &Path, seconds: usize, seed: usize) {
    use std::io::Write;
    const RATE: u32 = 48_000;
    let frames = seconds * RATE as usize;
    let mut data = Vec::with_capacity(frames * 4);
    let frequency = 180.0 + seed as f32 * 37.0;
    for frame in 0..frames {
        let sample =
            (std::f32::consts::TAU * frequency * frame as f32 / RATE as f32).sin() * 0.2;
        let pcm = (sample * i16::MAX as f32) as i16;
        data.extend_from_slice(&pcm.to_le_bytes());
        data.extend_from_slice(&pcm.to_le_bytes());
    }
    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&(36u32 + data.len() as u32).to_le_bytes());
    header.extend_from_slice(b"WAVEfmt ");
    header.extend_from_slice(&16u32.to_le_bytes());
    header.extend_from_slice(&1u16.to_le_bytes());
    header.extend_from_slice(&2u16.to_le_bytes());
    header.extend_from_slice(&RATE.to_le_bytes());
    header.extend_from_slice(&(RATE * 4).to_le_bytes());
    header.extend_from_slice(&4u16.to_le_bytes());
    header.extend_from_slice(&16u16.to_le_bytes());
    header.extend_from_slice(b"data");
    header.extend_from_slice(&(data.len() as u32).to_le_bytes());
    let mut file = std::fs::File::create(path).unwrap();
    file.write_all(&header).unwrap();
    file.write_all(&data).unwrap();
}

fn temp_cache(source_hash: &str) -> (patch_stems::cache::Cache, PathBuf, String) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("patch-stem-lease-{nonce}"));
    let cache =
        patch_stems::cache::Cache::new(root.join("audio"), root.join("metadata"), 1024 * 1024)
            .unwrap();
    let set_id = patch_stems::digest(format!("set-{nonce}").as_bytes());
    let staging = root.join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let audio = b"prepared vocals";
    std::fs::write(staging.join("vocals.flac"), audio).unwrap();
    let frames = 48_000 * 60;
    let stem = StemArtifact {
        chunks: vec![],
        id: "vocals".into(),
        cache_ref: "vocals.flac".into(),
        codec: "flac".into(),
        checksum: patch_stems::digest(audio),
        bytes: audio.len() as u64,
        frames,
        start_frame: 0,
        peak: 0.5,
        activity_hz: 4,
        activity: vec![],
        low_activity: vec![],
        waveform: vec![],
    };
    let set = StemSet {
        version: 1,
        id: set_id.clone(),
        source_hash: source_hash.into(),
        track_id: "track-a".into(),
        model_id: "test-separator".into(),
        model_version: "1".into(),
        weights_digest: "test-weights".into(),
        profile: "test".into(),
        pipeline_version: 1,
        backend: "cpu".into(),
        sample_rate: 48_000,
        frames,
        duration: 60.0,
        layout: vec!["vocals".into()],
        stems: BTreeMap::from([("vocals".into(), stem)]),
        created: 1,
    };
    cache.publish(&set, &staging).unwrap();
    (cache, root, set_id)
}

fn publish_test_stem_set(
    cache: &patch_stems::cache::Cache,
    root: &Path,
    source_hash: &str,
    track_id: &str,
    stem_id: &str,
) -> String {
    let set_id = patch_stems::digest(format!("{track_id}-{stem_id}").as_bytes());
    let staging = root.join(format!("staging-{track_id}-{stem_id}"));
    std::fs::create_dir_all(&staging).unwrap();
    let filename = format!("{stem_id}.wav");
    let audio_path = staging.join(&filename);
    write_test_wav(&audio_path, 16, track_id.len() + stem_id.len());
    let audio = std::fs::read(&audio_path).unwrap();
    let frames = 48_000 * 16;
    let artifact = StemArtifact {
        chunks: vec![],
        id: stem_id.into(),
        cache_ref: filename,
        codec: "wav".into(),
        checksum: patch_stems::digest(&audio),
        bytes: audio.len() as u64,
        frames,
        start_frame: 0,
        peak: 0.5,
        activity_hz: 4,
        activity: vec![],
        low_activity: vec![],
        waveform: vec![],
    };
    let set = StemSet {
        version: 1,
        id: set_id.clone(),
        source_hash: source_hash.into(),
        track_id: track_id.into(),
        model_id: "test-separator".into(),
        model_version: "1".into(),
        weights_digest: "test-weights".into(),
        profile: "test".into(),
        pipeline_version: 1,
        backend: "cpu".into(),
        sample_rate: 48_000,
        frames,
        duration: 16.0,
        layout: vec![stem_id.into()],
        stems: BTreeMap::from([(stem_id.into(), artifact)]),
        created: 1,
    };
    cache.publish(&set, &staging).unwrap();
    set_id
}

#[test]
fn preparation_binds_current_file_and_schedule_to_content_identity() {
    let path = temp_file();
    let graph = fixture(&path);
    let paths = BTreeMap::from([("track-a".into(), path.clone())]);
    let prepared = prepare_with_probe(&graph, &paths, 48_000, |_| {
        Ok(crate::deck::pcm::SourceInfo {
            rate: 48_000,
            channels: 2,
            duration: 60.0,
            ..Default::default()
        })
    })
    .unwrap();
    assert_eq!(
        prepared.sources[0].content_hash,
        graph.sources[0].content_hash.clone().unwrap()
    );
    assert_eq!(prepared.schedule.sample_rate, 48_000);
    verify_fresh(&prepared).unwrap();

    std::fs::write(&path, b"replacement audio").unwrap();
    assert!(
        verify_fresh(&prepared)
            .unwrap_err()
            .contains("changed after preparation")
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn preparation_rejects_missing_extra_and_changed_sources() {
    let path = temp_file();
    let graph = fixture(&path);
    assert!(prepare_with_probe(&graph, &BTreeMap::new(), 48_000, |_| unreachable!()).is_err());
    let wrong_paths = BTreeMap::from([("other-track".into(), path.clone())]);
    assert!(prepare_with_probe(&graph, &wrong_paths, 48_000, |_| unreachable!()).is_err());
    let paths = BTreeMap::from([("track-a".into(), path.clone())]);
    assert!(
        prepare_with_probe(&graph, &paths, 48_000, |_| {
            Ok(crate::deck::pcm::SourceInfo {
                rate: 48_000,
                channels: 2,
                duration: 10.0,
                ..Default::default()
            })
        })
        .unwrap_err()
        .contains("shorter than its planned regions")
    );
    std::fs::write(&path, b"changed").unwrap();
    assert!(
        prepare_with_probe(&graph, &paths, 48_000, |_| unreachable!())
            .unwrap_err()
            .contains("changed since planning")
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn stem_layer_binds_and_leases_exact_verified_cache_artifact() {
    let source_path = temp_file();
    let source_hash = patch_stems::hash_file(&source_path).unwrap();
    let (cache, root, set_id) = temp_cache(&source_hash);
    let mut graph = fixture(&source_path);
    let layer = &mut graph.states[0].layers[0];
    layer.material = MaterialKind::Stem("vocals".into());
    layer.stem_set_id = Some(set_id.clone());
    layer.source_start_s = 2.0;
    layer.source_end_s = 8.0;
    let source = BoundSource {
        id: "track-a".into(),
        path: source_path.clone(),
        content_hash: source_hash.clone(),
        duration_s: 60.0,
    };
    let (stems, leases) =
        bind_stems(&graph, std::slice::from_ref(&source), Some(&cache)).unwrap();
    assert_eq!(leases.len(), 1);
    assert_eq!(stems.len(), 1);
    assert_eq!(stems[0].source_id, "track-a");
    assert_eq!(stems[0].stem_set_id, set_id);
    assert_eq!(stems[0].stem_id, "vocals");
    assert_eq!(
        patch_stems::hash_file(&stems[0].path).unwrap(),
        stems[0].checksum
    );
    std::fs::write(&stems[0].path, b"replaced stem").unwrap();
    assert!(
        verify_bound_stems(&stems)
            .unwrap_err()
            .contains("changed after preparation")
    );

    let mut wrong_source = source;
    wrong_source.content_hash = "0".repeat(64);
    assert!(
        bind_stems(&graph, &[wrong_source], Some(&cache))
            .err()
            .unwrap()
            .contains("does not belong to source")
    );
    drop(leases);
    let _ = std::fs::remove_file(source_path);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn entry_projection_groups_selected_stems_on_one_source_lane() {
    use patch_core::performance::{
        ActiveLayer, PerformanceSource, PerformanceState, PerformanceTempoSegment,
    };
    let set_id = "set-track-a".to_string();
    let graph = PerformanceGraph {
        entry: "opening".into(),
        tempo: vec![PerformanceTempoSegment {
            start_beat: 0.0,
            end_beat: None,
            bpm: 120.0,
        }],
        sources: vec![PerformanceSource {
            id: "track-a".into(),
            content_hash: Some("content-hash".into()),
            tempo_bpm: Some(120.0),
            duration_s: 60.0,
        }],
        states: vec![PerformanceState {
            id: "opening".into(),
            at_beat: 0.0,
            layers: vec![ActiveLayer {
                id: "original".into(),
                source_id: "track-a".into(),
                material: MaterialKind::OriginalMix,
                stem_set_id: None,
                source_start_s: 12.0,
                source_end_s: 48.0,
                gain: 0.0,
            }]
            .into_iter()
            .chain(["drums", "vocals"].into_iter().map(|stem| ActiveLayer {
                id: stem.into(),
                source_id: "track-a".into(),
                material: MaterialKind::Stem(stem.into()),
                stem_set_id: Some(set_id.clone()),
                source_start_s: 12.0,
                source_end_s: 48.0,
                gain: if stem == "vocals" { 0.75 } else { 1.0 },
            }))
            .collect(),
        }],
        edges: vec![],
    };
    let prepared = PreparedPerformance {
        schedule: graph.compile(48_000).unwrap(),
        sources: vec![BoundSource {
            id: "track-a".into(),
            path: PathBuf::from("/music/track-a.flac"),
            content_hash: "content-hash".into(),
            duration_s: 60.0,
        }],
        stems: vec![],
        lane_assignments: allocate_lanes(&graph, &BTreeMap::new()).unwrap(),
        _stem_leases: vec![],
    };
    let lanes = entry_lanes(&graph, &prepared).unwrap();
    assert_eq!(lanes.len(), 1);
    assert_eq!(lanes[0].lane, 0);
    assert_eq!(lanes[0].source_start_s, 12.0);
    assert_eq!(lanes[0].stem_set_id.as_deref(), Some("set-track-a"));
    assert_eq!(lanes[0].original_gain, 0.0);
    assert_eq!(lanes[0].stem_gains["drums"], 1.0);
    assert_eq!(lanes[0].stem_gains["vocals"], 0.75);
}

#[test]
fn future_original_source_gets_silent_reserved_lane_and_sample_start() {
    use patch_core::performance::{
        ArrangementOperation, PerformanceEdge, PerformanceSource, PerformanceState,
        PerformanceTempoSegment,
    };
    let a = layer("a", "track-a", MaterialKind::OriginalMix);
    let b = layer("b", "track-b", MaterialKind::OriginalMix);
    let graph = PerformanceGraph {
        entry: "opening".into(),
        tempo: vec![PerformanceTempoSegment {
            start_beat: 0.0,
            end_beat: None,
            bpm: 120.0,
        }],
        sources: vec![
            PerformanceSource {
                id: "track-a".into(),
                content_hash: Some("hash-a".into()),
                tempo_bpm: Some(120.0),
                duration_s: 60.0,
            },
            PerformanceSource {
                id: "track-b".into(),
                content_hash: Some("hash-b".into()),
                tempo_bpm: Some(120.0),
                duration_s: 60.0,
            },
        ],
        states: vec![
            PerformanceState {
                id: "opening".into(),
                at_beat: 0.0,
                layers: vec![a.clone()],
            },
            PerformanceState {
                id: "blend".into(),
                at_beat: 8.0,
                layers: vec![a, b],
            },
        ],
        edges: vec![PerformanceEdge {
            from: "opening".into(),
            to: "blend".into(),
            at_beat: 4.0,
            operation: ArrangementOperation::IntroduceLayer {
                layer_id: "b".into(),
            },
        }],
    };
    let prepared = PreparedPerformance {
        schedule: graph.compile(48_000).unwrap(),
        sources: vec![
            BoundSource {
                id: "track-a".into(),
                path: PathBuf::from("/music/a.flac"),
                content_hash: "hash-a".into(),
                duration_s: 60.0,
            },
            BoundSource {
                id: "track-b".into(),
                path: PathBuf::from("/music/b.flac"),
                content_hash: "hash-b".into(),
                duration_s: 60.0,
            },
        ],
        stems: vec![],
        lane_assignments: allocate_lanes(&graph, &BTreeMap::new()).unwrap(),
        _stem_leases: vec![],
    };
    let lanes = performance_lanes(&graph, &prepared).unwrap();
    let a = lanes
        .iter()
        .find(|lane| lane.source_id == "track-a")
        .unwrap();
    let b = lanes
        .iter()
        .find(|lane| lane.source_id == "track-b")
        .unwrap();
    assert_eq!(a.lane, 0);
    assert!(a.starts_at_entry);
    assert_eq!(b.lane, 2);
    assert!(!b.starts_at_entry);
    assert_eq!(b.start_offset_samples, 96_000);
    assert_eq!(b.original_gain, 0.0);
    let automation = compile_entry_automation(&graph, &prepared, 10_000).unwrap();
    assert_eq!(automation.len(), 1);
    assert_eq!(automation[0].stem_id, None);
    assert_eq!(automation[0].lane, 2);
    assert_eq!(automation[0].at_sample, 106_000);
}

#[test]
fn final_source_release_is_timed_after_its_last_graph_edge() {
    use patch_core::performance::{
        ArrangementOperation as Op, PerformanceEdge, PerformanceSource, PerformanceState,
        PerformanceTempoSegment,
    };
    let path_a = temp_file();
    let path_b = temp_file();
    let source = |id: &str, path: &Path| PerformanceSource {
        id: id.into(),
        content_hash: Some(patch_stems::hash_file(path).unwrap()),
        tempo_bpm: Some(120.0),
        duration_s: 60.0,
    };
    let layer_a = layer("a", "a", MaterialKind::OriginalMix);
    let layer_b = layer("b", "b", MaterialKind::OriginalMix);
    let graph = PerformanceGraph {
        entry: "start".into(),
        tempo: vec![PerformanceTempoSegment {
            start_beat: 0.0,
            end_beat: None,
            bpm: 120.0,
        }],
        sources: vec![source("a", &path_a), source("b", &path_b)],
        states: vec![
            PerformanceState {
                id: "start".into(),
                at_beat: 0.0,
                layers: vec![layer_a.clone()],
            },
            PerformanceState {
                id: "blend".into(),
                at_beat: 4.0,
                layers: vec![layer_a, layer_b.clone()],
            },
            PerformanceState {
                id: "finish".into(),
                at_beat: 8.0,
                layers: vec![layer_b],
            },
        ],
        edges: vec![
            PerformanceEdge {
                from: "start".into(),
                to: "blend".into(),
                at_beat: 4.0,
                operation: Op::IntroduceLayer {
                    layer_id: "b".into(),
                },
            },
            PerformanceEdge {
                from: "blend".into(),
                to: "finish".into(),
                at_beat: 8.0,
                operation: Op::RemoveLayer {
                    layer_id: "a".into(),
                },
            },
        ],
    };
    let prepared = PreparedPerformance {
        schedule: graph.compile(48_000).unwrap(),
        sources: vec![
            BoundSource {
                id: "a".into(),
                path: path_a.clone(),
                content_hash: patch_stems::hash_file(&path_a).unwrap(),
                duration_s: 60.0,
            },
            BoundSource {
                id: "b".into(),
                path: path_b.clone(),
                content_hash: patch_stems::hash_file(&path_b).unwrap(),
                duration_s: 60.0,
            },
        ],
        stems: vec![],
        lane_assignments: allocate_lanes(&graph, &BTreeMap::new()).unwrap(),
        _stem_leases: vec![],
    };
    let mut lanes = performance_lanes(&graph, &prepared).unwrap();
    lanes
        .iter_mut()
        .find(|lane| lane.source_id == "a")
        .unwrap()
        .lane = 1;
    lanes
        .iter_mut()
        .find(|lane| lane.source_id == "a")
        .unwrap()
        .live = true;
    let pauses = compile_final_source_pauses(&graph, &prepared, &lanes, 10_000).unwrap();
    assert_eq!(
        pauses,
        vec![ScheduledDeckPause {
            lane: 1,
            source_id: "a".into(),
            at_sample: 202_000,
        }]
    );
    let _ = std::fs::remove_file(path_a);
    let _ = std::fs::remove_file(path_b);
}

#[test]
fn lanes_are_staged_at_the_performance_tempo() {
    let path = temp_file();
    let mut graph = fixture(&path);
    graph.tempo[0].bpm = 126.0;
    graph.sources[0].tempo_bpm = Some(120.0);
    let schedule = graph.compile(48_000).unwrap();
    let prepared = PreparedPerformance {
        schedule,
        sources: vec![BoundSource {
            id: "track-a".into(),
            path: path.clone(),
            content_hash: patch_stems::hash_file(&path).unwrap(),
            duration_s: 60.0,
        }],
        stems: vec![],
        lane_assignments: allocate_lanes(&graph, &BTreeMap::new()).unwrap(),
        _stem_leases: vec![],
    };
    let lanes = performance_lanes(&graph, &prepared).unwrap();
    assert!((lanes[0].tempo_adjustment - 0.05).abs() < 1e-9);
    let _ = std::fs::remove_file(path);
}

#[test]
fn stem_gain_events_compile_to_absolute_engine_sample_ramps() {
    use patch_core::performance::{
        ActiveLayer, ArrangementOperation, PerformanceEdge, PerformanceSource,
        PerformanceState, PerformanceTempoSegment,
    };
    let drums = ActiveLayer {
        stem_set_id: Some("set-track-a".into()),
        ..layer("drums", "track-a", MaterialKind::Stem("drums".into()))
    };
    let vocals = ActiveLayer {
        stem_set_id: Some("set-track-a".into()),
        ..layer("vocals", "track-a", MaterialKind::Stem("vocals".into()))
    };
    let quiet_drums = ActiveLayer {
        gain: 0.5,
        ..drums.clone()
    };
    let graph = PerformanceGraph {
        entry: "opening".into(),
        tempo: vec![PerformanceTempoSegment {
            start_beat: 0.0,
            end_beat: None,
            bpm: 120.0,
        }],
        sources: vec![PerformanceSource {
            id: "track-a".into(),
            content_hash: Some("hash-a".into()),
            tempo_bpm: Some(120.0),
            duration_s: 60.0,
        }],
        states: vec![
            PerformanceState {
                id: "opening".into(),
                at_beat: 0.0,
                layers: vec![drums.clone(), vocals.clone()],
            },
            PerformanceState {
                id: "next".into(),
                at_beat: 8.0,
                layers: vec![quiet_drums, vocals.clone()],
            },
            PerformanceState {
                id: "final".into(),
                at_beat: 16.0,
                layers: vec![drums, vocals.clone()],
            },
        ],
        edges: vec![
            PerformanceEdge {
                from: "opening".into(),
                to: "next".into(),
                at_beat: 4.0,
                operation: ArrangementOperation::SetGain {
                    layer_id: "drums".into(),
                    to: 0.5,
                    over_beats: 4.0,
                },
            },
            PerformanceEdge {
                from: "next".into(),
                to: "final".into(),
                at_beat: 12.0,
                operation: ArrangementOperation::SetGain {
                    layer_id: "drums".into(),
                    to: 1.0,
                    over_beats: 4.0,
                },
            },
        ],
    };
    let prepared = PreparedPerformance {
        schedule: graph.compile(48_000).unwrap(),
        sources: vec![BoundSource {
            id: "track-a".into(),
            path: PathBuf::from("/music/track-a.flac"),
            content_hash: "hash-a".into(),
            duration_s: 60.0,
        }],
        stems: vec![],
        lane_assignments: allocate_lanes(&graph, &BTreeMap::new()).unwrap(),
        _stem_leases: vec![],
    };
    let moves = compile_entry_automation(&graph, &prepared, 5_000).unwrap();
    assert_eq!(moves.len(), 2);
    assert_eq!(moves[0].lane, 0);
    assert_eq!(moves[0].stem_id.as_deref(), Some("drums"));
    assert_eq!(moves[0].at_sample, 101_000);
    assert_eq!(moves[0].duration_samples, 96_000);
    assert_eq!(moves[0].to_gain, 0.5);
    assert_eq!(moves[1].at_sample, 293_000);
    assert_eq!(moves[1].duration_samples, 96_000);
    assert_eq!(moves[1].to_gain, 1.0);
}

#[test]
fn separate_track_vocal_and_drum_mashup_binds_c_d_and_schedules_together() {
    use patch_core::performance::{
        ArrangementOperation as Op, PerformanceEdge, PerformanceState, PerformanceTempoSegment,
    };
    let a = layer("a-original", "a", MaterialKind::OriginalMix);
    let b = layer("b-original", "b", MaterialKind::OriginalMix);
    let vocal = ActiveLayer {
        id: "vocal-layer".into(),
        source_id: "c".into(),
        material: MaterialKind::Stem("vocals".into()),
        stem_set_id: Some("set-c".into()),
        source_start_s: 16.0,
        source_end_s: 24.0,
        gain: 1.0,
    };
    let drums = ActiveLayer {
        id: "drum-layer".into(),
        source_id: "d".into(),
        material: MaterialKind::Stem("drums".into()),
        stem_set_id: Some("set-d".into()),
        source_start_s: 72.0,
        source_end_s: 80.0,
        gain: 0.7,
    };
    let mut sources = Vec::new();
    for id in ["a", "b", "c", "d"] {
        sources.push(PerformanceSource {
            id: id.into(),
            content_hash: Some(format!("hash-{id}")),
            tempo_bpm: Some(120.0),
            duration_s: 240.0,
        });
    }
    let state = |id: &str, at_beat, layers| PerformanceState {
        id: id.into(),
        at_beat,
        layers,
    };
    let graph = PerformanceGraph {
        entry: "start".into(),
        tempo: vec![PerformanceTempoSegment {
            start_beat: 0.0,
            end_beat: None,
            bpm: 120.0,
        }],
        sources,
        states: vec![
            state("start", 0.0, vec![a.clone(), b.clone()]),
            state("vocal", 8.0, vec![a.clone(), b.clone(), vocal.clone()]),
            state(
                "both",
                8.0,
                vec![a.clone(), b.clone(), vocal.clone(), drums.clone()],
            ),
            state("drums", 24.0, vec![a.clone(), b.clone(), drums.clone()]),
            state("end", 24.0, vec![a, b]),
        ],
        edges: vec![
            PerformanceEdge {
                from: "start".into(),
                to: "vocal".into(),
                at_beat: 8.0,
                operation: Op::IntroduceLayer {
                    layer_id: vocal.id.clone(),
                },
            },
            PerformanceEdge {
                from: "vocal".into(),
                to: "both".into(),
                at_beat: 8.0,
                operation: Op::IntroduceLayer {
                    layer_id: drums.id.clone(),
                },
            },
            PerformanceEdge {
                from: "both".into(),
                to: "drums".into(),
                at_beat: 24.0,
                operation: Op::RemoveLayer {
                    layer_id: vocal.id.clone(),
                },
            },
            PerformanceEdge {
                from: "drums".into(),
                to: "end".into(),
                at_beat: 24.0,
                operation: Op::RemoveLayer {
                    layer_id: drums.id.clone(),
                },
            },
        ],
    };
    let schedule = graph.compile(48_000).unwrap();
    let prepared = PreparedPerformance {
        schedule,
        sources: ["a", "b", "c", "d"]
            .into_iter()
            .map(|id| BoundSource {
                id: id.into(),
                path: PathBuf::from(format!("/music/{id}.flac")),
                content_hash: format!("hash-{id}"),
                duration_s: 240.0,
            })
            .collect(),
        stems: vec![],
        lane_assignments: allocate_lanes(&graph, &BTreeMap::new()).unwrap(),
        _stem_leases: vec![],
    };
    let lanes = performance_lanes(&graph, &prepared).unwrap();
    assert_eq!(
        lanes
            .iter()
            .find(|lane| lane.source_id == "c")
            .unwrap()
            .lane,
        2
    );
    assert_eq!(
        lanes
            .iter()
            .find(|lane| lane.source_id == "d")
            .unwrap()
            .lane,
        3
    );
    let automation = compile_entry_automation(&graph, &prepared, 10_000).unwrap();
    let introductions = automation
        .iter()
        .filter(|move_| move_.to_gain > 0.0)
        .collect::<Vec<_>>();
    assert_eq!(introductions.len(), 2);
    assert_eq!(introductions[0].at_sample, introductions[1].at_sample);
    assert_eq!(
        introductions
            .iter()
            .map(|move_| move_.lane)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([2, 3])
    );
    let removals = automation
        .iter()
        .filter(|move_| move_.to_gain == 0.0)
        .collect::<Vec<_>>();
    assert_eq!(removals.len(), 2);
    assert_eq!(removals[0].at_sample, removals[1].at_sample);
}

#[test]
fn mashup_preparation_verifies_and_leases_both_source_stemsets() {
    use patch_core::performance::{
        ArrangementOperation as Op, PerformanceEdge, PerformanceState, PerformanceTempoSegment,
    };
    let mut paths = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    for (seed, id) in ["a", "b", "c", "d"].into_iter().enumerate() {
        let path = std::env::temp_dir().join(format!(
            "patch-mashup-source-{}-{id}.wav",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        write_test_wav(&path, 16, seed);
        hashes.insert(id, patch_stems::hash_file(&path).unwrap());
        paths.insert(id.to_string(), path);
    }
    let root = std::env::temp_dir().join(format!(
        "patch-mashup-cache-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let cache = patch_stems::cache::Cache::new(
        root.join("audio"),
        root.join("metadata"),
        4 * 1024 * 1024,
    )
    .unwrap();
    let vocal_set =
        publish_test_stem_set(&cache, &root, &hashes["c"], "vocal-source", "vocals");
    let drum_set = publish_test_stem_set(&cache, &root, &hashes["d"], "drum-source", "drums");
    let mut a = layer("a-original", "a", MaterialKind::OriginalMix);
    a.source_end_s = 16.0;
    let mut b = layer("b-original", "b", MaterialKind::OriginalMix);
    b.source_end_s = 16.0;
    let vocal = ActiveLayer {
        id: "vocal-layer".into(),
        source_id: "c".into(),
        material: MaterialKind::Stem("vocals".into()),
        stem_set_id: Some(vocal_set),
        source_start_s: 1.0,
        source_end_s: 3.0,
        gain: 1.0,
    };
    let drums = ActiveLayer {
        id: "drum-layer".into(),
        source_id: "d".into(),
        material: MaterialKind::Stem("drums".into()),
        stem_set_id: Some(drum_set),
        source_start_s: 3.0,
        source_end_s: 5.0,
        gain: 0.7,
    };
    let graph = PerformanceGraph {
        entry: "start".into(),
        tempo: vec![PerformanceTempoSegment {
            start_beat: 0.0,
            end_beat: None,
            bpm: 120.0,
        }],
        sources: ["a", "b", "c", "d"]
            .into_iter()
            .map(|id| PerformanceSource {
                id: id.into(),
                content_hash: Some(hashes[id].clone()),
                tempo_bpm: Some(120.0),
                duration_s: 16.0,
            })
            .collect(),
        states: vec![
            PerformanceState {
                id: "start".into(),
                at_beat: 0.0,
                layers: vec![a.clone(), b.clone()],
            },
            PerformanceState {
                id: "vocal".into(),
                at_beat: 8.0,
                layers: vec![a.clone(), b.clone(), vocal.clone()],
            },
            PerformanceState {
                id: "both".into(),
                at_beat: 8.0,
                layers: vec![a.clone(), b.clone(), vocal.clone(), drums.clone()],
            },
            PerformanceState {
                id: "drums".into(),
                at_beat: 24.0,
                layers: vec![a.clone(), b.clone(), drums.clone()],
            },
            PerformanceState {
                id: "end".into(),
                at_beat: 24.0,
                layers: vec![a, b],
            },
        ],
        edges: vec![
            PerformanceEdge {
                from: "start".into(),
                to: "vocal".into(),
                at_beat: 8.0,
                operation: Op::IntroduceLayer {
                    layer_id: vocal.id.clone(),
                },
            },
            PerformanceEdge {
                from: "vocal".into(),
                to: "both".into(),
                at_beat: 8.0,
                operation: Op::IntroduceLayer {
                    layer_id: drums.id.clone(),
                },
            },
            PerformanceEdge {
                from: "both".into(),
                to: "drums".into(),
                at_beat: 24.0,
                operation: Op::RemoveLayer { layer_id: vocal.id },
            },
            PerformanceEdge {
                from: "drums".into(),
                to: "end".into(),
                at_beat: 24.0,
                operation: Op::RemoveLayer { layer_id: drums.id },
            },
        ],
    };
    let prepared = prepare_with_probe_and_cache(&graph, &paths, 48_000, &cache, |_| {
        Ok(crate::deck::pcm::SourceInfo {
            rate: 48_000,
            channels: 2,
            duration: 16.0,
            ..Default::default()
        })
    })
    .unwrap();
    assert_eq!(prepared.stems.len(), 4);
    assert_eq!(prepared._stem_leases.len(), 2);
    assert_eq!(
        prepared
            .stems
            .iter()
            .map(|stem| (stem.source_id.as_str(), stem.stem_set_id.as_str()))
            .collect::<BTreeSet<_>>()
            .len(),
        2
    );
    verify_bound_stems(&prepared.stems).unwrap();
    let lanes = performance_lanes(&graph, &prepared).unwrap();
    assert_eq!(
        lanes
            .iter()
            .find(|lane| lane.source_id == "c")
            .unwrap()
            .lane,
        2
    );
    assert_eq!(
        lanes
            .iter()
            .find(|lane| lane.source_id == "d")
            .unwrap()
            .lane,
        3
    );
    assert_ne!(
        lanes
            .iter()
            .find(|lane| lane.source_id == "c")
            .unwrap()
            .stem_set_id,
        lanes
            .iter()
            .find(|lane| lane.source_id == "d")
            .unwrap()
            .stem_set_id
    );
    let automation = compile_entry_automation(&graph, &prepared, 12_000).unwrap();
    let introductions = automation
        .iter()
        .filter(|movement| movement.to_gain > 0.0)
        .collect::<Vec<_>>();
    assert_eq!(introductions.len(), 2);
    assert_eq!(introductions[0].at_sample, introductions[1].at_sample);
    assert_eq!(
        introductions
            .iter()
            .map(|movement| movement.lane)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([2, 3])
    );

    // Exercise the actual preparation handoff: decode the ordinary tracks
    // and both cached stem artifacts, attach them to C/D deck workers, and
    // wait for the workers to report the selected stem layouts ready.
    let params = std::sync::Arc::new(crate::Params::new(&[
        "bus", "deck-a", "deck-b", "deck-c", "deck-d",
    ]));
    for (index, worker) in params.decks.iter().enumerate() {
        worker.spawn(&format!("mashup-test-{index}"));
    }
    verify_fresh(&prepared).unwrap();
    let entry_lanes = stage_entry(&graph, &prepared, &params).unwrap();
    assert_eq!(entry_lanes.len(), 4);
    for (source_id, stem_id) in [("c", "vocals"), ("d", "drums")] {
        let lane = entry_lanes
            .iter()
            .find(|lane| lane.source_id == source_id)
            .unwrap();
        let worker = &params.decks[lane.lane];
        assert!(
            worker
                .stems
                .ready
                .load(std::sync::atomic::Ordering::Acquire)
        );
        assert_eq!(worker.stems.snapshot()["controls"][0]["id"], stem_id);
        assert_eq!(
            worker.status.lock().unwrap().path.as_ref(),
            lane.path.to_string_lossy().as_ref()
        );
    }
    let shared_start = 4_096;
    let realtime_moves =
        compile_entry_automation_on_lanes(&graph, &prepared, &entry_lanes, shared_start)
            .unwrap();
    assert_eq!(realtime_moves.len(), 4);
    schedule_entry_automation(&params, &realtime_moves).unwrap();
    for id in ["a", "b", "c", "d"] {
        params.set_gain(params.channel(&format!("deck-{id}")).unwrap(), 1.0);
    }
    // The decoder runs at idle priority: on a busy two-core CI runner it
    // can take a while. Wait for every loaded deck to hold its whole
    // track before the clock starts, so audibility tests the mix, not
    // the scheduler.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while params.decks.iter().any(|d| {
        let st = d.status.lock().unwrap();
        st.loaded && st.decoded < 1.0
    }) && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    start_entry_at(&params, &entry_lanes, shared_start).unwrap();

    // Drive the same RT mixer used by live audio. Before the phrase
    // boundary only the two original tracks are audible; after it, both
    // independently prepared stem decks must contribute to the program.
    let mut rt = crate::Rt::new(params.clone());
    let mut before_energy = 0.0f64;
    let mut after_energy = 0.0f64;
    let mut before_samples = 0usize;
    let mut after_samples = 0usize;
    let mut post_phrase_audio = Vec::new();
    for cycle in 0..220u64 {
        let mut left = [0.0f32; 1024];
        let mut right = [0.0f32; 1024];
        let clock = crate::Clock {
            position: cycle * 1024,
            nsec: cycle * 21_333_333,
            rate: 48_000,
            frames: 1024,
            delay: 0,
            ..Default::default()
        };
        // Faster than real time: wait for rendering decks to have this
        // cycle buffered (a slow CI runner's workers otherwise underrun,
        // and the decks read as silent).
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
        while params.decks.iter().any(|d| {
            d.rendering.load(std::sync::atomic::Ordering::Acquire) && d.buffered_frames() < 1024
        }) && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        rt.run(&clock, &[], [&mut left, &mut right], None);
        if (100..120).contains(&cycle) {
            before_energy += left
                .iter()
                .chain(&right)
                .map(|x| f64::from(*x) * f64::from(*x))
                .sum::<f64>();
            before_samples += left.len() + right.len();
        } else if (210..220).contains(&cycle) {
            after_energy += left
                .iter()
                .chain(&right)
                .map(|x| f64::from(*x) * f64::from(*x))
                .sum::<f64>();
            after_samples += left.len() + right.len();
            post_phrase_audio.extend_from_slice(&left);
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let before_rms = (before_energy / before_samples as f64).sqrt();
    let after_rms = (after_energy / after_samples as f64).sqrt();
    assert!(
        before_rms > 0.02,
        "original tracks were not audible: {before_rms}"
    );
    assert!(
        after_rms > before_rms * 1.2,
        "both scheduled stem layers should raise the mixed energy: before={before_rms}, after={after_rms}"
    );
    let component_amplitude = |frequency: f32| {
        let (sine, cosine) = post_phrase_audio.iter().enumerate().fold(
            (0.0f64, 0.0f64),
            |(sine, cosine), (index, sample)| {
                let phase =
                    std::f64::consts::TAU * f64::from(frequency) * index as f64 / 48_000.0;
                (
                    sine + f64::from(*sample) * phase.sin(),
                    cosine + f64::from(*sample) * phase.cos(),
                )
            },
        );
        2.0 * sine.hypot(cosine) / post_phrase_audio.len() as f64
    };
    for (stem, frequency) in [("vocals", 846.0), ("drums", 772.0)] {
        let amplitude = component_amplitude(frequency);
        assert!(
            amplitude > 0.03,
            "scheduled {stem} stem was not audible in the RT mix: {amplitude}"
        );
    }

    drop(prepared);
    drop(cache);
    for path in paths.values() {
        let _ = std::fs::remove_file(path);
    }
    let _ = std::fs::remove_dir_all(root);
}

/// A source handed from an arrangement lane (C) to a free A/B deck
/// plays on without a seam: same click spacing, same level, through
/// the RT mixer, with keylock and a tempo change (the stretcher in use).
#[test]
fn drums_alone_near_their_tempo_are_resampled_everything_else_keeps_its_key() {
    assert!(!lane_keylock(["drums"].into_iter(), 0.015));
    assert!(lane_keylock(["drums"].into_iter(), 0.03));
    assert!(lane_keylock(["drums", "bass"].into_iter(), 0.0));
    assert!(lane_keylock(["vocals"].into_iter(), 0.01));
    // The original mix (no stems) is tonal.
    assert!(lane_keylock(std::iter::empty(), 0.01));
}

#[test]
fn the_track_that_plays_on_goes_back_to_the_deck_the_outgoing_track_left() {
    let lane = |lane: usize, id: &str| EntryLane {
        lane,
        source_id: id.into(),
        path: PathBuf::from(format!("/{id}.flac")),
        source_start_s: 0.0,
        source_end_s: 60.0,
        tempo_adjustment: 0.0,
        original_gain: 1.0,
        starts_at_entry: lane != 0,
        live: lane == 0,
        start_offset_samples: 0,
        stem_gains: BTreeMap::new(),
        stem_set_id: None,
    };
    let pause = |lane: usize, id: &str, at: u64| ScheduledDeckPause {
        lane,
        source_id: id.into(),
        at_sample: at,
    };
    // Outgoing on A, incoming on C, a third track's drums on D.
    let lanes = [lane(0, "out"), lane(2, "in"), lane(3, "drums")];
    let pauses = [pause(3, "drums", 900), pause(0, "out", 500)];
    let (l, from, to, after) = hand_back_plan(&lanes, &pauses).unwrap();
    assert_eq!((l.source_id.as_str(), from, to, after), ("in", 2, 0, 900));
    // The incoming already on an A/B deck, or two left playing: no hand-back.
    assert!(hand_back_plan(&[lane(0, "out"), lane(1, "in")], &[pause(0, "out", 5)]).is_none());
    assert!(hand_back_plan(&lanes, &[pause(0, "out", 5)]).is_none());
    // No A/B deck released.
    assert!(hand_back_plan(&lanes[1..], &[pause(3, "drums", 9)]).is_none());
}

#[test]
fn a_hand_back_moves_the_music_to_another_deck_without_a_seam() {
    use crate::deck::transport::Cmd;
    let rate = 48_000usize;
    let period = 12_000usize;
    let audio: Vec<f32> = (0..rate * 20)
        .flat_map(|i| {
            let v = if i % period < 48 { 0.8 } else { 0.0 } + 0.05 * (i as f32 * 0.03).sin();
            [v, v]
        })
        .collect();
    let pcm = crate::deck::pcm::Pcm::from_samples(rate as u32, audio);
    let params = std::sync::Arc::new(crate::Params::new(&[
        "bus", "deck-a", "deck-b", "deck-c", "deck-d",
    ]));
    for (index, worker) in params.decks.iter().enumerate() {
        worker.spawn(&format!("hand-back-test-{index}"));
        worker
            .rate
            .store(rate as u32, std::sync::atomic::Ordering::Release);
    }
    for id in ["a", "b", "c", "d"] {
        params.set_gain(params.channel(&format!("deck-{id}")).unwrap(), 1.0);
    }
    let c = &params.decks[2];
    c.send(Cmd::Load(pcm.clone()));
    c.send(Cmd::Keylock(true));
    c.send(Cmd::Tempo(0.02));
    c.send(Cmd::Play);
    // A slow runner's worker may not have taken its commands yet.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !c.status.lock().unwrap().playing && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let mut rt = crate::Rt::new(params.clone());
    let mut out: Vec<f32> = vec![];
    let at = 4 * rate as u64;
    let mut handed = false;
    for cycle in 0..400u64 {
        let mut left = [0.0f32; 1024];
        let mut right = [0.0f32; 1024];
        let clock = crate::Clock {
            position: cycle * 1024,
            nsec: cycle * 21_333_333,
            rate: rate as u32,
            frames: 1024,
            delay: 0,
            ..Default::default()
        };
        // The loop runs ~20x faster than real time; give a rendering
        // deck the time real time would (a debug build's stretcher can't
        // keep up otherwise, and its underruns read as a seam).
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
        while params.decks.iter().any(|d| {
            d.rendering.load(std::sync::atomic::Ordering::Acquire) && d.buffered_frames() < 1024
        }) && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        rt.run(&clock, &[], [&mut left, &mut right], None);
        out.extend_from_slice(&left);
        if !handed && cycle * 1024 >= 2 * rate as u64 {
            hand_back_at(&params, 2, 1, Some(pcm.clone()), at).unwrap();
            handed = true;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    // Clicks from 1 s before to 2 s after the hand-back.
    let (lo, hi) = (at as usize - rate, at as usize + 2 * rate);
    let onsets: Vec<usize> = (lo + 1..hi)
        .filter(|&i| out[i] > 0.5 && out[i - 1] <= 0.5)
        .filter(|&i| out[i.saturating_sub(2000)..i].iter().all(|v| *v <= 0.5))
        .collect();
    let gaps: Vec<i64> = onsets.windows(2).map(|w| (w[1] - w[0]) as i64).collect();
    let want = (period as f64 / 1.02) as i64;
    // (No deck status reads: with the RT loop stopped, a worker waiting
    // for ring space may hold its status lock.)
    assert!(
        gaps.len() >= 9 && gaps.iter().all(|g| (g - want).abs() <= 24),
        "click spacing across the hand-back (want {want}): {gaps:?} at {onsets:?}"
    );
    let level = |a: usize, b: usize| {
        (out[a..b].iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / (b - a) as f64).sqrt()
    };
    let (before, after) = (
        level(lo, at as usize),
        level(at as usize, at as usize + rate),
    );
    assert!(
        (20.0 * (after / before).log10()).abs() < 0.5,
        "level {before} → {after}"
    );
}

#[test]
fn original_mix_graph_gain_compiles_without_stems() {
    use patch_core::performance::{
        ActiveLayer, ArrangementOperation, PerformanceEdge, PerformanceSource,
        PerformanceState, PerformanceTempoSegment,
    };
    let loud = ActiveLayer {
        gain: 1.0,
        ..layer("mix", "track-a", MaterialKind::OriginalMix)
    };
    let quiet = ActiveLayer {
        gain: 0.35,
        ..loud.clone()
    };
    let graph = PerformanceGraph {
        entry: "opening".into(),
        tempo: vec![PerformanceTempoSegment {
            start_beat: 0.0,
            end_beat: None,
            bpm: 120.0,
        }],
        sources: vec![PerformanceSource {
            id: "track-a".into(),
            content_hash: Some("hash-a".into()),
            tempo_bpm: Some(120.0),
            duration_s: 60.0,
        }],
        states: vec![
            PerformanceState {
                id: "opening".into(),
                at_beat: 0.0,
                layers: vec![loud],
            },
            PerformanceState {
                id: "next".into(),
                at_beat: 8.0,
                layers: vec![quiet],
            },
        ],
        edges: vec![PerformanceEdge {
            from: "opening".into(),
            to: "next".into(),
            at_beat: 4.0,
            operation: ArrangementOperation::SetGain {
                layer_id: "mix".into(),
                to: 0.35,
                over_beats: 4.0,
            },
        }],
    };
    let prepared = PreparedPerformance {
        schedule: graph.compile(48_000).unwrap(),
        sources: vec![BoundSource {
            id: "track-a".into(),
            path: PathBuf::from("/music/track-a.flac"),
            content_hash: "hash-a".into(),
            duration_s: 60.0,
        }],
        stems: vec![],
        lane_assignments: allocate_lanes(&graph, &BTreeMap::new()).unwrap(),
        _stem_leases: vec![],
    };
    let moves = compile_entry_automation(&graph, &prepared, 5_000).unwrap();
    assert_eq!(moves.len(), 1);
    assert_eq!(moves[0].lane, 0);
    assert_eq!(moves[0].stem_id, None);
    assert_eq!(moves[0].at_sample, 101_000);
    assert_eq!(moves[0].duration_samples, 96_000);
    assert_eq!(moves[0].to_gain, 0.35);
    let mut live_lanes = performance_lanes(&graph, &prepared).unwrap();
    live_lanes[0].lane = 1;
    live_lanes[0].live = true;
    let live_moves =
        compile_entry_automation_on_lanes(&graph, &prepared, &live_lanes, 5_000).unwrap();
    assert_eq!(live_moves[0].lane, 1);
}

#[test]
fn return_to_original_schedules_on_the_stem_source_lane() {
    use patch_core::performance::{
        ActiveLayer, ArrangementOperation, PerformanceEdge, PerformanceSource,
        PerformanceState, PerformanceTempoSegment,
    };
    let path = temp_file();
    let hash = patch_stems::hash_file(&path).unwrap();
    let original = ActiveLayer {
        id: "original".into(),
        source_id: "track-a".into(),
        material: MaterialKind::OriginalMix,
        stem_set_id: None,
        source_start_s: 0.0,
        source_end_s: 60.0,
        gain: 0.0,
    };
    let drums = ActiveLayer {
        stem_set_id: Some("set-a".into()),
        ..layer("drums", "track-a", MaterialKind::Stem("drums".into()))
    };
    let graph = PerformanceGraph {
        entry: "instrumental".into(),
        tempo: vec![PerformanceTempoSegment {
            start_beat: 0.0,
            end_beat: None,
            bpm: 120.0,
        }],
        sources: vec![PerformanceSource {
            id: "track-a".into(),
            content_hash: Some(hash.clone()),
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
    let prepared = PreparedPerformance {
        schedule: graph.compile(48_000).unwrap(),
        sources: vec![BoundSource {
            id: "track-a".into(),
            path: path.clone(),
            content_hash: hash,
            duration_s: 60.0,
        }],
        stems: vec![],
        lane_assignments: allocate_lanes(&graph, &BTreeMap::new()).unwrap(),
        _stem_leases: vec![],
    };
    let moves = compile_entry_automation(&graph, &prepared, 1_000).unwrap();
    assert_eq!(moves.len(), 2);
    assert_eq!(moves[0].lane, 0);
    assert_eq!(moves[0].stem_id, None);
    assert_eq!(moves[0].to_gain, 1.0);
    assert_eq!(moves[1].stem_id.as_deref(), Some("drums"));
    assert_eq!(moves[1].to_gain, 0.0);
    assert_eq!(moves[0].at_sample, 97_000);
    assert_eq!(moves[0].duration_samples, 96_000);
    let _ = std::fs::remove_file(path);
}

#[test]
fn lane_allocator_keeps_each_track_synchronized_and_reuses_released_lanes() {
    let graph = lane_graph();
    graph.validate().unwrap();
    let assignments = allocate_lanes(&graph, &BTreeMap::new()).unwrap();
    let lane = |state: &str, layer: &str| {
        assignments
            .iter()
            .find(|item| item.state_id == state && item.layer_id == layer)
            .unwrap()
            .lane
    };
    assert_eq!(lane("abc", "a-drums"), 0);
    assert_eq!(lane("abc", "b-original"), 1);
    assert_eq!(lane("abc", "c-original"), 2);
    assert_eq!(lane("bcd", "b-original"), 1);
    assert_eq!(lane("bcd", "c-original"), 2);
    assert_eq!(lane("bcd", "d-original"), 0);

    let preferred = BTreeMap::from([("a".into(), 1), ("b".into(), 0)]);
    let assignments = allocate_lanes(&graph, &preferred).unwrap();
    assert_eq!(
        assignments
            .iter()
            .find(|item| item.state_id == "ab" && item.source_id == "a")
            .unwrap()
            .lane,
        1
    );
    assert!(allocate_lanes(&graph, &BTreeMap::from([("a".into(), 4)])).is_err());
}
