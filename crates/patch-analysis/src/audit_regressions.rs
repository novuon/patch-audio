//! Reproductions from the October 2026 audit; all inputs are synthetic.
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "patch-audit-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn kicks(&self, name: &str) -> PathBuf {
        self.shifted_kicks(name, 0)
    }
    fn shifted_kicks(&self, name: &str, shift: u32) -> PathBuf {
        let rate = 22_050u32;
        let samples: Vec<i16> = (0..rate * 20)
            .map(|i| {
                let t = ((i + rate / 2 - shift) % (rate / 2)) as f64 / rate as f64;
                (24_000.0 * (std::f64::consts::TAU * 65.0 * t).sin() * (-40.0 * t).exp()) as i16
            })
            .collect();
        let size = samples.len() as u32 * 2;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + size).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&size.to_le_bytes());
        for sample in samples {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
        let path = self.0.join(name);
        std::fs::write(&path, wav).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn window_grid_recomputes_when_grid_parameters_change() {
    // Requires ffmpeg and nice, both present in the Arch CI environment.
    let f = Fixture::new();
    let path = f.kicks("cached.wav");
    let original = window_grid(&path, 120.0, 0.0, 0.0, 20.0).expect("synthetic kick grid");
    for (index, (bpm, offset)) in [(120.0, 0.125), (121.0, 0.0)].into_iter().enumerate() {
        let fresh_path = f.0.join(format!("fresh-{index}.wav"));
        std::fs::copy(&path, &fresh_path).unwrap();
        let fresh = window_grid(&fresh_path, bpm, offset, 0.0, 20.0);
        assert_ne!(
            fresh,
            Some(original),
            "fixture must distinguish the changed grid"
        );
        assert_eq!(
            window_grid(&path, bpm, offset, 0.0, 20.0),
            fresh,
            "a cached window must use its requested BPM and offset"
        );
    }
}

#[test]
fn a_record_is_only_used_for_the_bytes_it_was_made_from() {
    // In a child process with its own cache: analysis writes the cache.
    let f = Fixture::new();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "audit_regressions::source_identity_child", "--nocapture"])
        .env("PATCH_AUDIT_DIR", &f.0)
        .env("XDG_CACHE_HOME", f.0.join("cache"))
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn source_identity_child() {
    let Some(dir) = std::env::var_os("PATCH_AUDIT_DIR").map(PathBuf::from) else {
        return;
    };
    let eventually = |what: &str, check: &dyn Fn() -> bool| {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !check() {
            assert!(std::time::Instant::now() < until, "{what}");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    };
    let f = Fixture(dir.clone());
    let a = f.kicks("a.wav");
    let b = dir.join("b.wav");
    let mut bytes = std::fs::read(&a).unwrap();
    bytes[200_000] ^= 0x40; // outside the three sampled regions
    std::fs::write(&b, &bytes).unwrap();
    let len = bytes.len() as u64;
    assert_eq!(content_key(&a, len), content_key(&b, len), "one sampled key");

    let made = analyse_file(&a).unwrap();
    assert!(made.source.is_some());
    assert!(cached(&a).is_some());
    // b's bytes differ: once its hash is known, a's record is not b's.
    eventually("b recognised as other bytes", &|| cache_needs_analysis(&b));
    assert!(cached(&b).is_none());

    let remade = analyse_file(&b).unwrap();
    assert_ne!(remade.source.as_ref().unwrap().sha256, made.source.unwrap().sha256);
    assert!(cached(&b).is_some());
    assert!(cached(&a).is_none(), "the record now belongs to b");

    // Moved: same bytes, new revision; verified once, then free again.
    let moved = dir.join("moved.wav");
    std::fs::rename(&b, &moved).unwrap();
    eventually("a moved file keeps its analysis", &|| cached(&moved).is_some());
    std::mem::forget(f); // the parent removes the directory
}

#[test]
fn kick_key_tracks_exact_tempo_and_source_revision() {
    let f = Fixture::new();
    let path = f.kicks("kick-key.wav");
    let a = KickKey::new(&path, 120.0).unwrap();
    let b = KickKey::new(&path, 120.1).unwrap();
    assert!(a != b, "sub-half-BPM changes must not alias");
    let replacement = f.kicks("replacement.wav");
    std::fs::rename(replacement, &path).unwrap();
    let changed = KickKey::new(&path, 120.0).unwrap();
    assert!(
        a != changed,
        "same-size source replacement must invalidate the memo"
    );
}

#[test]
fn pruning_removes_obsolete_pairs_but_keeps_future_and_inflight_files() {
    let f = Fixture::new();
    let old = f.0.join("old.json");
    let current = f.0.join("current.json");
    let future = f.0.join("future.json");
    for (path, version) in [
        (&old, VERSION - 1),
        (&current, VERSION),
        (&future, VERSION + 1),
    ] {
        std::fs::write(path, format!("{{\"version\":{version}}}")).unwrap();
    }
    let old_detail = old.with_extension("wfd");
    std::fs::write(&old_detail, b"obsolete detail").unwrap();
    let orphan = f.0.join("inflight.wfd");
    let temporary = f.0.join("inflight.pending");
    std::fs::write(&orphan, b"complete but not yet referenced").unwrap();
    std::fs::write(&temporary, b"in progress").unwrap();
    let settled = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 24 * 60 * 60);
    let mut stale = Vec::new();
    for name in ["stale.wfd", "stale.json.1-0.pending", "stale.tmp"] {
        let path = f.0.join(name);
        std::fs::write(&path, b"left behind").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(settled))
            .unwrap();
        stale.push(path);
    }
    assert_eq!(prune_cache_dir(&f.0).0, 5);
    assert!(!old.exists() && !old_detail.exists());
    assert!(current.exists() && future.exists());
    assert!(orphan.exists() && temporary.exists());
    assert!(stale.iter().all(|p| !p.exists()));
}

#[test]
fn reanalysing_one_track_leaves_other_tracks_work_valid() {
    let f = Fixture::new();
    let a = f.kicks("a.wav");
    let b = f.kicks("b.wav");
    let (a_before, b_before) = (memo_epoch(&a), memo_epoch(&b));
    forget(&a);
    assert_ne!(memo_epoch(&a), a_before);
    assert_eq!(memo_epoch(&b), b_before);
}

#[test]
fn window_grid_invalidates_same_size_replacement_with_preserved_mtime() {
    let f = Fixture::new();
    let path = f.kicks("cached.wav");
    let stamp = std::fs::metadata(&path).unwrap();
    let original = window_grid(&path, 120.0, 0.0, 0.0, 20.0);
    assert!(original.is_some());
    let replacement = f.shifted_kicks("replacement.wav", 2205);
    let expected = window_grid(&replacement, 120.0, 0.0, 0.0, 20.0);
    assert_ne!(expected, original);
    std::fs::File::options()
        .write(true)
        .open(&replacement)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(stamp.modified().unwrap()))
        .unwrap();
    assert_eq!(std::fs::metadata(&replacement).unwrap().len(), stamp.len());
    std::fs::rename(replacement, &path).unwrap();
    assert_eq!(window_grid(&path, 120.0, 0.0, 0.0, 20.0), expected);
}

#[test]
fn a_stalled_decoder_is_stopped_and_frees_its_permit() {
    // A fake ffmpeg that never writes, first on PATH, in a child process.
    let f = Fixture::new();
    let bin = f.0.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let fake = bin.join("ffmpeg");
    std::fs::write(&fake, "#!/bin/sh\nexec sleep 30\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let started = std::time::Instant::now();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "audit_regressions::stalled_decoder_child", "--nocapture"])
        .env("PATCH_AUDIT_DIR", &f.0)
        .env("PATCH_DECODE_STALL_S", "1")
        .env("PATH", path)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
}

#[test]
fn stalled_decoder_child() {
    let Some(dir) = std::env::var_os("PATCH_AUDIT_DIR").map(PathBuf::from) else {
        return;
    };
    let f = Fixture(dir);
    let track = f.kicks("t.wav");
    // More stalls than permits: each must give its permit back.
    for _ in 0..MAX_DECODE_JOBS + 1 {
        let error = decode(&track).unwrap_err();
        assert!(error.contains("stalled"), "{error}");
    }
    std::mem::forget(f);
}

#[test]
fn long_audio_waits_for_the_one_long_turn() {
    // In test builds "long" is over 5 s; the fixture is 20 s.
    let f = Fixture::new();
    let track = f.kicks("long.wav");
    let turn = LongAudioPermit::acquire();
    let (done, finished) = std::sync::mpsc::channel();
    let worker = {
        let track = track.clone();
        std::thread::spawn(move || {
            let decoded = decode(&track);
            done.send(()).unwrap();
            decoded
        })
    };
    assert!(
        finished.recv_timeout(std::time::Duration::from_millis(800)).is_err(),
        "a long decode must wait while the turn is taken"
    );
    drop(turn);
    finished.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
    let pcm = worker.join().unwrap().unwrap();
    assert!(pcm.len() >= 19 * RATE as usize * 2);
}

#[test]
fn decode_turns_are_shared_between_processes() {
    let f = Fixture::new();
    let turns: &'static Turns = Box::leak(Box::new(Turns::at(Some(f.0.clone()), "decode", 2)));
    let (one, two) = (turns.take(), turns.take());
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "audit_regressions::decode_turn_child", "--nocapture"])
        .env("PATCH_AUDIT_DIR", &f.0)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(child.try_wait().unwrap().is_none(), "both turns are taken here");
    drop(one);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(std::time::Instant::now() < until, "a freed turn reaches the other process");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    drop(two);
}

#[test]
fn decode_turn_child() {
    let Some(dir) = std::env::var_os("PATCH_AUDIT_DIR").map(PathBuf::from) else {
        return;
    };
    let turns: &'static Turns = Box::leak(Box::new(Turns::at(Some(dir), "decode", 2)));
    drop(turns.take());
}

#[test]
fn background_work_leaves_the_interactive_decode_turn_free() {
    let f = Fixture::new();
    let turns: &'static Turns = Box::leak(Box::new(Turns::reserving(Some(f.0.clone()), "decode", 3, 1)));
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "audit_regressions::background_turns_child", "--nocapture"])
        .env("PATCH_AUDIT_DIR", &f.0)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    // And the reserved turn is there for this (interactive) process.
    let _interactive = turns.take();
}

#[test]
fn background_turns_child() {
    let Some(dir) = std::env::var_os("PATCH_AUDIT_DIR").map(PathBuf::from) else {
        return;
    };
    set_background();
    let turns: &'static Turns = Box::leak(Box::new(Turns::reserving(Some(dir), "decode", 3, 1)));
    let (a, b) = (turns.take(), turns.take());
    // A third would wait: background work never takes the reserved turn.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let third = turns.take();
        let _ = tx.send(());
        third
    });
    assert!(rx.recv_timeout(std::time::Duration::from_millis(300)).is_err());
    drop((a, b));
}
