//! Recorder telemetry and directory work, called only by its background poller.
use serde_json::{Value, json};
pub fn read(request: &Value) -> Result<Value, String> {
    if request["op"] == "stop" {
        let result = patch_record::daemon::stop_and_wait()?;
        return Ok(json!({"message":"Take saved.", "status":null, "result":result}));
    }
    if request["op"] == "pause" {
        if !patch_record::daemon::pause(request["paused"].as_bool() == Some(true)) {
            return Err("Recorder did not acknowledge pause.".into());
        }
        return Ok(json!({"message":"", "status":patch_record::daemon::status()}));
    }
    if request["op"] == "start" {
        let take = patch_record::daemon::Take {
            source: patch_record::daemon::Source::parse(
                request["source"].as_str().unwrap_or_default(),
            )
            .ok_or("Invalid recording source")?,
            label: request["label"].as_str().unwrap_or_default().to_owned(),
            format: if request["float"] == true {
                patch_record::wav::SampleFormat::F32
            } else {
                patch_record::wav::SampleFormat::S24
            },
            rate: request["rate"].as_u64().unwrap_or(48_000) as u32,
            path: request["path"].as_str().unwrap_or_default().into(),
        };
        patch_record::daemon::start(&std::env::current_exe().map_err(|e| e.to_string())?, &take)?;
        return Ok(
            json!({"message":format!("Recording {} · {}", take.label, take.format.label()), "status":patch_record::daemon::status()}),
        );
    }
    if request["op"] == "status" {
        return Ok(patch_record::daemon::status().unwrap_or(Value::Null));
    }
    let dir = std::path::PathBuf::from(request["dir"].as_str().unwrap_or_default());
    let mut takes: Vec<_> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "wav"))
        .filter_map(|e| {
            let metadata = e.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            Some((
                metadata.modified().ok()?,
                json!({
                    "name": e.file_name().to_string_lossy().trim_end_matches(".wav"),
                    "path": e.path(), "detail": format!("{:.1} MB", metadata.len() as f64 / 1e6)
                }),
            ))
        })
        .collect();
    takes.sort_by_key(|t| std::cmp::Reverse(t.0));
    let takes: Vec<_> = takes.into_iter().take(50).map(|(_, t)| t).collect();
    let parent = dir
        .ancestors()
        .find(|p| p.is_dir())
        .unwrap_or(std::path::Path::new("/"));
    let free_bytes = std::ffi::CString::new(parent.as_os_str().as_encoded_bytes())
        .ok()
        .and_then(|path| {
            let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
            // SAFETY: path is NUL-terminated and stats points to writable storage.
            if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
                return None;
            }
            // SAFETY: statvfs succeeded and initialized the structure.
            let stats = unsafe { stats.assume_init() };
            Some(stats.f_bavail.saturating_mul(stats.f_frsize))
        });
    let free = free_bytes
        .map(|b| format!("{:.1} GB", b as f64 / 1e9))
        .unwrap_or_else(|| "?".into());
    let result = std::fs::read(patch_record::daemon::socket_path().with_extension("result.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .unwrap_or(Value::Null);
    Ok(json!({"takes": takes, "free": free, "free_bytes":free_bytes, "result": result}))
}
