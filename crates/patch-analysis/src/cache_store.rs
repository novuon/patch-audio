//! Serialized cache updates shared by independent Patch processes.
//! Computation belongs outside the lock; only re-read/merge/replace goes here.
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

struct EntryLock(File);
impl EntryLock {
    fn acquire(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Never unlink lock files: another process may already hold this inode.
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.with_extension("lock"))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // SAFETY: file owns this valid descriptor for the guard's lifetime.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self(file));
            }
            let error = io::Error::last_os_error();
            if !matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                return Err(error);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "analysis cache entry is busy",
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
impl Drop for EntryLock {
    fn drop(&mut self) {
        // SAFETY: the descriptor is still valid, and closing it also releases
        // this advisory lock if unlocking is interrupted.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

struct Pending(PathBuf);
impl Drop for Pending {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn replace(path: &Path, write: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let (pending, mut file) = loop {
        // .pending is deliberately distinct from the legacy .tmp files that
        // startup pruning removes. A crashed writer can leave an orphan;
        // offline/age-aware orphan cleanup is a separate maintenance step.
        let pending = path.with_extension(format!(
            "{}-{}.pending",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&pending)
        {
            Ok(file) => break (Pending(pending), file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    write(&mut file)?;
    file.flush()?;
    drop(file);
    // Atomic publication, not power-loss durability. Cache data is regenerable.
    std::fs::rename(&pending.0, path)
}

pub(crate) fn update(
    path: &Path,
    change: impl FnOnce(Option<Value>) -> Result<Value, String>,
) -> Result<Value, String> {
    let _lock = EntryLock::acquire(path).map_err(|e| e.to_string())?;
    let old = match std::fs::read(path) {
        Ok(bytes) => Some(serde_json::from_slice::<Value>(&bytes).map_err(|e| e.to_string())?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    let next = change(old.clone())?;
    if old.as_ref() != Some(&next) {
        replace(path, |file| {
            serde_json::to_writer(file, &next).map_err(io::Error::other)
        })
        .map_err(|e| e.to_string())?;
    }
    Ok(next)
}

pub(crate) fn write_blob(
    path: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let _lock = EntryLock::acquire(path)?;
    replace(path, write)
}

/// Remove a JSON record and its detail sidecar under the writers' lock.
/// Pruning supplies the revision it inspected: a refreshed entry is skipped.
pub(crate) fn remove(
    path: &Path,
    inspected: Option<crate::source::Revision>,
) -> io::Result<(usize, u64)> {
    let _lock = EntryLock::acquire(path)?;
    if inspected.is_some_and(|old| crate::source::Revision::of(path).ok() != Some(old)) {
        return Ok((0, 0));
    }
    let mut removed = (0, 0);
    for file in [path.with_extension("wfd"), path.to_path_buf()] {
        if let Ok(metadata) = std::fs::metadata(&file)
            && std::fs::remove_file(&file).is_ok()
        {
            removed.0 += 1;
            removed.1 += metadata.len();
        }
    }
    Ok(removed)
}

/// Three-way field merge. Unchanged computed fields retain concurrent edits;
/// overlapping incompatible edits fail instead of silently overwriting data.
/// Objects merge recursively (including analyzer-version fields); arrays are
/// atomic evidence collections, never combined without domain knowledge.
pub(crate) fn merge(base: &Value, computed: &Value, latest: &Value) -> Result<Value, String> {
    if computed == base || computed == latest {
        return Ok(latest.clone());
    }
    if latest == base {
        return Ok(computed.clone());
    }
    if let (Some(b), Some(c), Some(l)) =
        (base.as_object(), computed.as_object(), latest.as_object())
    {
        let mut result = l.clone();
        let keys: std::collections::BTreeSet<_> = b.keys().chain(c.keys()).collect();
        for key in keys {
            let before = b.get(key);
            let after = c.get(key);
            let current = l.get(key);
            if after == before || after == current {
                continue;
            }
            if current == before {
                if let Some(value) = after {
                    result.insert(key.clone(), value.clone());
                } else {
                    result.remove(key);
                }
            } else if let (Some(before), Some(after), Some(current)) = (before, after, current) {
                result.insert(
                    key.clone(),
                    merge(before, after, current).map_err(|e| format!("{key}: {e}"))?,
                );
            } else {
                return Err(format!("concurrent analysis change: {key}"));
            }
        }
        return Ok(Value::Object(result));
    }
    Err("concurrent analysis change".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "patch-cache-store-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn refresh_preserves_independent_updates_and_rejects_conflicts() {
        let base = json!({"bpm":120, "analyzers":{"beat":1,"key":1},"vocals":null,"regions":[]});
        let mut computed = base.clone();
        computed["bpm"] = json!(121);
        computed["analyzers"]["beat"] = json!(2);
        let mut latest = base.clone();
        latest["vocals"] = json!([1, 2]);
        latest["regions"] = json!(["new"]);
        latest["analyzers"]["key"] = json!(2);
        let result = merge(&base, &computed, &latest).unwrap();
        assert_eq!(result["bpm"], 121);
        assert_eq!(result["vocals"], latest["vocals"]);
        assert_eq!(result["regions"], latest["regions"]);
        assert_eq!(result["analyzers"], json!({"beat":2,"key":2}));
        latest["bpm"] = json!(128);
        assert!(merge(&base, &computed, &latest).is_err());
    }

    #[test]
    fn failed_writes_preserve_the_previous_complete_record() {
        let dir = Temp::new();
        let path = dir.0.join("entry.json");
        update(&path, |_| Ok(json!({"old":true}))).unwrap();
        assert!(
            write_blob(&path, |file| {
                file.write_all(b"partial")?;
                Err(io::Error::other("injected"))
            })
            .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"old\":true}");
        assert!(update(&path, |_| Err("rejected change".into())).is_err());
        assert!(
            std::fs::read_dir(&dir.0)
                .unwrap()
                .all(|e| e.unwrap().path().extension().unwrap() != "pending")
        );
    }

    #[test]
    fn pruning_does_not_delete_an_entry_refreshed_since_its_scan() {
        let dir = Temp::new();
        let path = dir.0.join("entry.json");
        update(&path, |_| Ok(json!({"version":1}))).unwrap();
        let old = crate::source::Revision::of(&path).unwrap();
        update(&path, |_| Ok(json!({"version":2}))).unwrap();
        write_blob(&path.with_extension("wfd"), |f| f.write_all(b"detail")).unwrap();
        assert_eq!(remove(&path, Some(old)).unwrap(), (0, 0));
        assert!(path.exists());
        assert!(path.with_extension("wfd").exists());
        let current = crate::source::Revision::of(&path).unwrap();
        assert_eq!(remove(&path, Some(current)).unwrap().0, 2);
        assert!(!path.exists());
        assert!(!path.with_extension("wfd").exists());
    }

    #[test]
    fn independent_process_updates_are_serialized() {
        let dir = Temp::new();
        let path = dir.0.join("entry.json");
        update(&path, |_| Ok(json!({"count":0}))).unwrap();
        let spawn = || {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "cache_store::tests::process_writer",
                    "--nocapture",
                ])
                .env("PATCH_CACHE_TEST_ENTRY", &path)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        };
        let mut a = spawn();
        let mut b = spawn();
        assert!(a.wait().unwrap().success());
        assert!(b.wait().unwrap().success());
        let result: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(result["count"], 128);
    }

    #[test]
    fn process_writer() {
        let Some(path) = std::env::var_os("PATCH_CACHE_TEST_ENTRY") else {
            return;
        };
        for _ in 0..64 {
            update(Path::new(&path), |old| {
                let mut value = old.unwrap();
                let n = value["count"].as_u64().unwrap();
                std::thread::sleep(Duration::from_micros(50));
                value["count"] = json!(n + 1);
                Ok(value)
            })
            .unwrap();
        }
    }
}
