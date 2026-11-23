use patch_stems::{
    atomic_json, batches,
    cache::{Cache, Memo},
    jobs,
    separator::{Backend, Model, ProcessSeparator},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const FIXTURE: &str = r#"
import json, pathlib, shutil, sys, time, wave

request = json.load(sys.stdin)
source = pathlib.Path(request["source"])
out = pathlib.Path(request["output"])
root = pathlib.Path(__file__).parent
with wave.open(str(source)) as audio:
    frames = audio.getnframes()
with (root / "calls").open("a") as log:
    print(frames, file=log)
while frames == 960 and not (root / "release").exists():
    time.sleep(0.02)
shutil.copyfile(source, out / "vocals.wav")
(out / "output.json").write_text(json.dumps({
    "sample_rate": 48000, "frames": frames, "available": ["vocals"]
}))
"#;

fn cache(root: &Path) -> Cache {
    Cache::new(root.join("audio"), root.join("state"), 16 * 1024 * 1024).unwrap()
}

fn separator(root: &Path) -> jobs::Separator {
    (
        Box::new(ProcessSeparator(Model {
            id: "restart-fixture".into(),
            version: "1".into(),
            weights_digest: "a".repeat(64),
            layout: vec!["vocals".into()],
            profile: "fixture".into(),
            adapter: root.join("fake-separator.py"),
            python: "python3".into(),
            weights: None,
        })),
        Backend {
            id: "fixture".into(),
            device: "cpu".into(),
            precision: "float32".into(),
            threads: 1,
        },
    )
}

#[test]
fn isolated_worker() {
    let Some(root) = std::env::var_os("PATCH_STEM_RESTART_TEST_ROOT").map(PathBuf::from) else {
        return;
    };
    let cache = cache(&root);
    let configured = || {
        if root.join("separator-disabled").exists() {
            Err("Fixture separator is unavailable".into())
        } else {
            Ok(separator(&root))
        }
    };
    let mut records = Memo::default();
    let mut runner = batches::Runner::default();
    batches::recover_legacy(&cache).unwrap();
    let mut recover = true;
    loop {
        if recover {
            recover = jobs::recover(&cache, &mut records, &configured, |_, _| {}).unwrap();
        }
        runner.tick(&cache, &configured).unwrap();
        fs::write(
            root.join(format!("worker-{}", std::process::id())),
            b"ready",
        )
        .unwrap();
        thread::sleep(Duration::from_millis(40));
    }
}

struct Worker(Child);

impl Worker {
    fn start(root: &Path) -> Self {
        let worker = Self(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "isolated_worker", "--nocapture"])
                .env("PATCH_STEM_RESTART_TEST_ROOT", root)
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        wait(|| {
            root.join(format!("worker-{}", worker.0.id()))
                .is_file()
                .then_some(())
        });
        worker
    }

    fn stop(&mut self) {
        self.0.kill().unwrap();
        self.0.wait().unwrap();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "patch-stem-restart-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("fake-separator.py"), FIXTURE).unwrap();
        Self(root)
    }

    fn track(&self, frames: u32) -> batches::Track {
        let source = self.0.join(format!("{frames}.wav"));
        let bytes = frames * 4;
        let mut wave = Vec::new();
        wave.extend(b"RIFF");
        wave.extend((36 + bytes).to_le_bytes());
        wave.extend(b"WAVEfmt ");
        wave.extend(16u32.to_le_bytes());
        wave.extend(1u16.to_le_bytes()); // PCM
        wave.extend(2u16.to_le_bytes());
        wave.extend(48000u32.to_le_bytes());
        wave.extend(192000u32.to_le_bytes());
        wave.extend(4u16.to_le_bytes());
        wave.extend(16u16.to_le_bytes());
        wave.extend(b"data");
        wave.extend(bytes.to_le_bytes());
        wave.resize((44 + bytes) as usize, 0);
        fs::write(&source, wave).unwrap();
        batches::Track {
            source,
            track: frames.to_string(),
        }
    }

    fn calls(&self, frames: u32) -> usize {
        let frames = frames.to_string();
        fs::read_to_string(self.0.join("calls"))
            .unwrap_or_default()
            .lines()
            .filter(|line| *line == frames)
            .count()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn wait<T>(mut predicate: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(result) = predicate() {
            return result;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for stem preparation"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn batch_status(cache: &Cache, id: &str) -> Value {
    batches::summaries(cache, &mut Memo::default(), &jobs::list(cache))
        .into_iter()
        .find(|row| row["id"] == id)
        .unwrap()
}

#[test]
fn batch_survives_process_handover_without_repeating_ready_or_cancelled_work() {
    let fixture = Fixture::new();
    let cache = cache(&fixture.0);
    let tracks = vec![fixture.track(480), fixture.track(960), fixture.track(1440)];
    let (id, joined) = batches::submit(&cache, &[2, 1], tracks.clone(), true, &[]).unwrap();
    assert!(!joined);
    assert_eq!(
        batches::submit(&cache, &[1, 2, 2], tracks, true, &[]).unwrap(),
        (id.clone(), true)
    );
    let (cancelled_id, _) =
        batches::submit(&cache, &[3], vec![fixture.track(1920)], false, &[]).unwrap();
    batches::cancel(&cache, &cancelled_id).unwrap();

    let mut retiring = Worker::start(&fixture.0);
    wait(|| (fixture.calls(960) == 1).then_some(()));
    assert_eq!(batch_status(&cache, &id)["complete"], 1);
    let interrupted = jobs::list(&cache)
        .into_iter()
        .find(|job| job["state"] == "preparing")
        .unwrap();
    assert_ne!(interrupted["resumed"], true);

    // Use an overlapping successor, as in an engine handover. It must wait
    // for the old process to release ownership of the interrupted track.
    let _successor = Worker::start(&fixture.0);
    thread::sleep(Duration::from_millis(250));
    assert_eq!(fixture.calls(960), 1);
    retiring.stop();

    wait(|| (fixture.calls(960) == 2).then_some(()));
    assert!(
        jobs::list(&cache)
            .iter()
            .any(|job| { job["id"] == interrupted["id"] && job["resumed"] == true })
    );
    assert_eq!(batch_status(&cache, &id)["resumed"], true);
    fs::write(fixture.0.join("release"), b"").unwrap();
    wait(|| (batch_status(&cache, &id)["state"] == "ready").then_some(()));
    assert_eq!(batch_status(&cache, &id)["complete"], 3);
    assert_eq!(batch_status(&cache, &cancelled_id)["state"], "cancelled");
    assert_eq!(fixture.calls(480), 1);
    assert_eq!(fixture.calls(1440), 1);
    assert_eq!(fixture.calls(1920), 0);
}

#[test]
fn restart_reuses_published_audio_when_job_and_batch_status_were_not_saved() {
    let fixture = Fixture::new();
    let cache = cache(&fixture.0);
    let (id, _) = batches::submit(&cache, &[1], vec![fixture.track(480)], true, &[]).unwrap();
    let mut worker = Worker::start(&fixture.0);
    wait(|| (batch_status(&cache, &id)["state"] == "ready").then_some(()));
    worker.stop();

    // Leave the on-disk state as it would be immediately after publishing
    // the object directory, before saving its metadata or either status.
    let mut job = jobs::list(&cache).pop().unwrap();
    let job_path = cache
        .metadata
        .join(format!("job-{}.json", job["id"].as_str().unwrap()));
    let set_path = cache
        .metadata
        .join(format!("{}.json", job["key"].as_str().unwrap()));
    fs::remove_file(set_path).unwrap();
    job.as_object_mut().unwrap().remove("set");
    job["state"] = json!("preparing");
    atomic_json(&job_path, &job).unwrap();
    let batch_path = cache.metadata.join(format!("{id}.json"));
    let mut batch: Value = serde_json::from_slice(&fs::read(&batch_path).unwrap()).unwrap();
    batch["state"] = json!("preparing");
    batch["next"] = json!(0);
    batch["complete"] = json!(0);
    atomic_json(&batch_path, &batch).unwrap();
    fs::write(fixture.0.join("separator-disabled"), b"").unwrap();

    let _successor = Worker::start(&fixture.0);
    wait(|| (batch_status(&cache, &id)["state"] == "ready").then_some(()));
    assert_eq!(batch_status(&cache, &id)["resumed"], true);
    assert_eq!(fixture.calls(480), 1);
    assert_eq!(cache.list()[0]["pins"], json!(["480"]));
}

#[test]
fn restart_finishes_cancelling_an_interrupted_batch() {
    let fixture = Fixture::new();
    let cache = cache(&fixture.0);
    let (id, _) = batches::submit(
        &cache,
        &[1],
        vec![fixture.track(960), fixture.track(1440)],
        true,
        &[],
    )
    .unwrap();
    let mut worker = Worker::start(&fixture.0);
    wait(|| (fixture.calls(960) == 1).then_some(()));
    worker.stop();

    // Simulate a crash after the cancellation was durably recorded, before
    // the batch or child was able to commit a terminal status.
    atomic_json(
        &cache.metadata.join("locks").join(format!("cancel-{id}")),
        &json!(true),
    )
    .unwrap();
    let _successor = Worker::start(&fixture.0);
    wait(|| {
        jobs::list(&cache)
            .iter()
            .any(|r| r["state"] == "cancelled")
            .then_some(())
    });
    assert_eq!(batch_status(&cache, &id)["state"], "cancelled");
    assert_eq!(fixture.calls(960), 1);
    assert_eq!(fixture.calls(1440), 0);
}

#[test]
fn legacy_batches_report_missing_tracks_and_resume_admitted_jobs() {
    let fixture = Fixture::new();
    let cache = cache(&fixture.0);
    let track = fixture.track(480);
    atomic_json(
        &cache.metadata.join("job-42.json"),
        &json!({"id":"42", "source":track.source, "track":track.track, "pin":false,
                "state":"queued", "started":1}),
    )
    .unwrap();
    atomic_json(
        &cache.metadata.join("batch-41.json"),
        &json!({"state":"preparing","total":2,"admitted":1,"children":["42"]}),
    )
    .unwrap();

    let _worker = Worker::start(&fixture.0);
    wait(|| {
        jobs::list(&cache)
            .iter()
            .any(|r| r["id"] == "42" && r["state"] == "ready")
            .then_some(())
    });
    let batch = batch_status(&cache, "batch-41");
    assert_eq!(batch["state"], "failed");
    assert_eq!(batch["complete"], 1);
    assert_eq!(batch["failed"], 1);
    assert!(
        batch["error"]
            .as_str()
            .unwrap()
            .contains("Press Prepare stems")
    );
    assert_eq!(fixture.calls(480), 1);
}
