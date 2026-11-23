//! Persistent content-addressed audio cache. Leases and eviction are background I/O.
use crate::{atomic_json, now};
use patch_core::stems::StemSet;
use serde_json::{Value, json};
use std::{
    fs::File,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
};
#[derive(Clone)]
pub struct Cache {
    pub audio: PathBuf,
    pub metadata: PathBuf,
    pub budget: u64,
}
pub struct Lease {
    pub set: StemSet,
    pub directory: PathBuf,
    _lock: File,
}
impl Lease {
    /// Keep this cache object leased for another consumer. Duplicated file
    /// descriptors share the flock lifetime, so cleanup remains blocked until
    /// every consumer has released its lease.
    pub fn try_clone(&self) -> Result<Self, String> {
        Ok(Self {
            set: self.set.clone(),
            directory: self.directory.clone(),
            _lock: self._lock.try_clone().map_err(|error| error.to_string())?,
        })
    }
}
fn lock(path: &Path, exclusive: bool, nonblocking: bool) -> std::io::Result<File> {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    let op = if exclusive {
        libc::LOCK_EX
    } else {
        libc::LOCK_SH
    } | if nonblocking { libc::LOCK_NB } else { 0 };
    // SAFETY: valid owned file descriptor; lock released when the file is closed.
    if unsafe { libc::flock(f.as_raw_fd(), op) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(f)
}
fn read(path: &Path) -> Result<Value, String> {
    serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
/// Parsed metadata records, each kept as long as its file is unchanged
/// (same size and mtime). Set manifests and job records run to megabytes
/// each (activity curves): a catalogue pass over hundreds of them must not
/// parse them all again.
pub struct Memo<T> {
    files: std::collections::HashMap<PathBuf, (u64, std::time::SystemTime, Option<T>)>,
}

impl<T> Default for Memo<T> {
    fn default() -> Self {
        Memo { files: Default::default() }
    }
}

impl<T: Clone> Memo<T> {
    /// `f` of each metadata file `keep` accepts (by name), in directory
    /// order; a file is read again only when it changed. Files gone are
    /// forgotten.
    pub fn list(
        &mut self,
        dir: &Path,
        keep: impl Fn(&Path) -> bool,
        f: impl Fn(&Value) -> Option<T>,
    ) -> Vec<T> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if !keep(&path) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let stamp = (meta.len(), meta.modified().unwrap_or(std::time::UNIX_EPOCH));
            let value = match self.files.get(&path) {
                Some((len, at, v)) if (*len, *at) == stamp => v.clone(),
                _ => {
                    let v = read(&path).ok().and_then(|v| f(&v));
                    self.files.insert(path.clone(), (stamp.0, stamp.1, v.clone()));
                    v
                }
            };
            seen.insert(path);
            out.extend(value);
        }
        self.files.retain(|p, _| seen.contains(p));
        out
    }
}

/// A stem set's record file (`<64 hex>.json`), as [`Cache::list`] reads.
pub fn is_set_file(path: &Path) -> bool {
    path.extension().is_some_and(|x| x == "json")
        && path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| valid_key(s).is_ok())
}

pub fn valid_key(key: &str) -> Result<(), String> {
    if key.len() != 64 || !key.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("Invalid stem cache key".into());
    }
    Ok(())
}
impl Cache {
    pub(crate) fn work_lock(&self, name: &str) -> Result<Option<File>, String> {
        match lock(&self.metadata.join("locks").join(name), true, true) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    pub(crate) fn batch_lock(&self) -> Result<File, String> {
        lock(&self.metadata.join("locks/batch-catalog"), true, false).map_err(|e| e.to_string())
    }
    pub fn default_cache() -> Result<Self, String> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("HOME is unavailable")?;
        Self::new(
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".cache"))
                .join("omarchy-patch/stems"),
            std::env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/state"))
                .join("omarchy-patch/stems"),
            std::env::var("PATCH_STEM_CACHE_BYTES")
                .ok()
                .and_then(|s| s.parse().ok())
                // About 150 tracks' stems (95-135 MB each). 1 GB held 8, and
                // 4 GB filled up with stems pinned by "Prepare stems" (never
                // evicted: 12 GB on 2026-10-24), so every set the AI asked
                // for a coming transition was evicted as soon as it was made.
                // For now: room for both. Managing the cache (and telling the
                // DJ about it) is on docs/roadmap.md.
                .unwrap_or(20 * 1024 * 1024 * 1024),
        )
    }
    pub fn new(audio: PathBuf, metadata: PathBuf, budget: u64) -> Result<Self, String> {
        for p in [&audio, &metadata, &metadata.join("locks")] {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        Ok(Self {
            audio,
            metadata,
            budget,
        })
    }
    fn catalog_lock(&self) -> Result<File, String> {
        lock(&self.metadata.join("catalog.lock"), true, false).map_err(|e| e.to_string())
    }
    pub fn job_lock(&self, key: &str) -> Result<File, String> {
        valid_key(key)?;
        lock(
            &self.metadata.join("locks").join(format!("{key}.job")),
            true,
            true,
        )
        .map_err(|e| e.to_string())
    }
    fn object_lock(&self, key: &str, exclusive: bool, nonblocking: bool) -> Result<File, String> {
        lock(
            &self.metadata.join("locks").join(key),
            exclusive,
            nonblocking,
        )
        .map_err(|e| e.to_string())
    }
    pub fn acquire(&self, key: &str) -> Result<Lease, String> {
        valid_key(key)?;
        let _catalog = self.catalog_lock()?;
        let guard = self.object_lock(key, false, false)?;
        let directory = self.audio.join(key);
        if std::fs::symlink_metadata(&directory)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("Unsafe cache directory".into());
        }
        let path = self.metadata.join(format!("{key}.json"));
        let mut meta = read(&path).unwrap_or(json!({}));
        let recovered = !meta["set"].is_object() || meta["resident"] != true;
        let set: StemSet = if recovered {
            serde_json::from_value(read(&directory.join("manifest.json"))?)
        } else {
            serde_json::from_value(meta["set"].clone())
        }
        .map_err(|e| e.to_string())?;
        if set.id != key {
            return Err("Mismatched stem manifest".into());
        }
        for s in set.stems.values() {
            if s.cache_ref.contains('/')
                || s.cache_ref.contains('\\')
                || s.cache_ref.starts_with('.')
                || std::fs::symlink_metadata(directory.join(&s.cache_ref))
                    .map_or(true, |m| !m.is_file() || m.file_type().is_symlink())
            {
                return Err("Stem audio is unavailable".into());
            }
        }
        if recovered {
            meta["set"] = json!(set);
            meta["resident"] = json!(true);
        }
        meta["last_used"] = json!(now());
        atomic_json(&path, &meta)?;
        Ok(Lease {
            set,
            directory,
            _lock: guard,
        })
    }
    pub fn publish(&self, set: &StemSet, staging: &Path) -> Result<(), String> {
        valid_key(&set.id)?;
        let _catalog = self.catalog_lock()?;
        let _object = self.object_lock(&set.id, true, true)?;
        let target = self.audio.join(&set.id);
        if target.exists() {
            return Err("Stem cache revision already exists".into());
        }
        atomic_json(
            &staging.join("manifest.json"),
            &serde_json::to_value(set).map_err(|e| e.to_string())?,
        )?;
        std::fs::rename(staging, &target).map_err(|e| e.to_string())?;
        let old = read(&self.metadata.join(format!("{}.json", set.id))).unwrap_or(json!({}));
        let result = atomic_json(
            &self.metadata.join(format!("{}.json", set.id)),
            &json!({"set":set,"pins":old["pins"].as_array().cloned().unwrap_or_default(),"last_used":now(),"resident":true}),
        );
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&target);
        }
        result
    }
    pub fn pin(&self, key: &str, reference: &str, pinned: bool) -> Result<(), String> {
        valid_key(key)?;
        if reference.is_empty() || reference.len() > 256 {
            return Err("Missing pin reference".into());
        }
        let _catalog = self.catalog_lock()?;
        let path = self.metadata.join(format!("{key}.json"));
        let mut meta = read(&path).unwrap_or(json!({}));
        let mut pins: Vec<String> =
            serde_json::from_value(meta["pins"].clone()).unwrap_or_default();
        pins.retain(|p| p != reference);
        if pinned {
            pins.push(reference.into());
        }
        meta["pins"] = json!(pins);
        atomic_json(&path, &meta)
    }
    /// Stingy pins: release every track pin ("Prepare stems") whose track
    /// is not in `keep`; other references (`kept_refs`: the AI's hold until
    /// a transition has played) stay. The sets stay on disk until the
    /// budget needs their room (`cleanup`). How many pins were released.
    pub fn release_pins_except(
        &self,
        keep: &std::collections::HashSet<String>,
        kept_refs: &[&str],
    ) -> Result<usize, String> {
        let _catalog = self.catalog_lock()?;
        let mut released = 0;
        for path in std::fs::read_dir(&self.metadata)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_set_file(p))
        {
            let Ok(mut meta) = read(&path) else { continue };
            let mut pins: Vec<String> =
                serde_json::from_value(meta["pins"].clone()).unwrap_or_default();
            let before = pins.len();
            pins.retain(|p| kept_refs.contains(&p.as_str()) || keep.contains(p));
            if pins.len() != before {
                released += before - pins.len();
                meta["pins"] = json!(pins);
                atomic_json(&path, &meta)?;
            }
        }
        Ok(released)
    }
    pub fn associate(&self, key: &str, track: &str) -> Result<(), String> {
        valid_key(key)?;
        let _catalog = self.catalog_lock()?;
        let path = self.metadata.join(format!("{key}.json"));
        let mut meta = read(&path)?;
        let mut aliases: Vec<String> =
            serde_json::from_value(meta["aliases"].clone()).unwrap_or_default();
        if !aliases.iter().any(|a| a == track) {
            aliases.push(track.into());
            meta["aliases"] = json!(aliases);
            atomic_json(&path, &meta)?;
        }
        Ok(())
    }
    pub fn free_bytes(&self) -> Result<u64, String> {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let path = CString::new(self.audio.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: NUL-terminated existing directory and writable statvfs storage.
        if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // SAFETY: successful statvfs initialized the structure.
        let stat = unsafe { stat.assume_init() };
        Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
    }
    pub fn list(&self) -> Vec<Value> {
        std::fs::read_dir(&self.metadata)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| is_set_file(&e.path()))
            .filter_map(|e| read(&e.path()).ok())
            .filter(|v| v["set"].is_object())
            .collect()
    }
    pub fn cleanup(&self) -> Result<Value, String> {
        let _catalog = self.catalog_lock()?;
        let mut working_bytes = 0u64;
        for entry in std::fs::read_dir(&self.audio)
            .map_err(|e| e.to_string())?
            .flatten()
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry
                .file_type()
                .map_or(true, |t| !t.is_dir() || t.is_symlink())
            {
                continue;
            }
            if valid_key(&name).is_ok() {
                let path = self.metadata.join(format!("{name}.json"));
                let mut record = read(&path).unwrap_or(json!({}));
                if (!record["set"].is_object() || record["resident"] != true)
                    && let Ok(manifest) = read(&entry.path().join("manifest.json"))
                    && manifest["id"] == name
                {
                    record["set"] = manifest;
                    record["resident"] = json!(true);
                    record["last_used"] = json!(now());
                    atomic_json(&path, &record)?;
                }
            } else if let Some(attempt) = name.strip_prefix(".staging-")
                && let id = attempt.split('.').next().unwrap_or_default()
                && id.bytes().all(|b| b.is_ascii_digit() || b == b'-')
            {
                let old = entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|d| d.as_secs() > 3600);
                let key = read(&self.metadata.join(format!("job-{id}.json")))
                    .ok()
                    .and_then(|v| v["key"].as_str().map(str::to_owned));
                if old
                    && let Some(key) = key
                    && let Ok(_job) = self.job_lock(&key)
                {
                    std::fs::remove_dir_all(entry.path()).map_err(|e| e.to_string())?;
                    continue;
                }
                working_bytes += std::fs::read_dir(entry.path())
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|e| e.metadata().ok())
                    .filter(|m| m.is_file())
                    .map(|m| m.len())
                    .sum::<u64>();
            }
        }
        let mut rows = self.list();
        rows.sort_by_key(|v| v["last_used"].as_u64().unwrap_or(0));
        let actual = |key: &str| -> u64 {
            std::fs::read_dir(self.audio.join(key))
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        };
        let mut used = rows
            .iter()
            .filter_map(|v| v["set"]["id"].as_str())
            .map(actual)
            .sum::<u64>();
        let mut removed = 0;
        for mut row in rows {
            if used <= self.budget {
                break;
            }
            if row["pins"].as_array().is_some_and(|p| !p.is_empty()) {
                continue;
            }
            let Some(key) = row["set"]["id"].as_str().map(str::to_owned) else {
                continue;
            };
            valid_key(&key)?;
            let Ok(_object) = self.object_lock(&key, true, true) else {
                continue;
            };
            if std::fs::symlink_metadata(self.audio.join(&key))
                .is_ok_and(|m| m.file_type().is_symlink())
            {
                continue;
            }
            let bytes = actual(&key);
            if bytes == 0 {
                continue;
            }
            std::fs::remove_dir_all(self.audio.join(&key)).map_err(|e| e.to_string())?;
            used = used.saturating_sub(bytes);
            removed += 1;
            row["resident"] = json!(false);
            atomic_json(&self.metadata.join(format!("{key}.json")), &row)?;
        }
        Ok(
            json!({"bytes":used,"budget":self.budget,"removed":removed,"working_bytes":working_bytes,"over_budget":used>self.budget}),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn memo_reads_a_record_again_only_when_its_file_changed() {
        let dir = std::env::temp_dir().join(format!(
            "patch-stems-memo-{}-{}",
            std::process::id(),
            now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join(format!("{}.json", "a".repeat(64))), dir.join(format!("{}.json", "b".repeat(64))));
        std::fs::write(&a, r#"{"n":1}"#).unwrap();
        std::fs::write(&b, r#"{"n":2}"#).unwrap();
        std::fs::write(dir.join("job-1.json"), r#"{"n":9}"#).unwrap();
        let reads = std::cell::Cell::new(0);
        let mut memo = Memo::default();
        let pass = |memo: &mut Memo<u64>| {
            let mut v = memo.list(&dir, is_set_file, |v| {
                reads.set(reads.get() + 1);
                v["n"].as_u64()
            });
            v.sort();
            v
        };
        assert_eq!(pass(&mut memo), vec![1, 2]);
        assert_eq!(reads.get(), 2, "the job file is not a set");
        assert_eq!(pass(&mut memo), vec![1, 2]);
        assert_eq!(reads.get(), 2, "unchanged: nothing parsed");
        std::fs::write(&a, r#"{"n":10}"#).unwrap();
        assert_eq!(pass(&mut memo), vec![2, 10]);
        assert_eq!(reads.get(), 3, "only the changed one");
        std::fs::remove_file(&b).unwrap();
        assert_eq!(pass(&mut memo), vec![10]);
        assert_eq!(memo.files.len(), 1, "a removed file is forgotten");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn lease_and_pin_protect_eviction_and_metadata_survives() {
        let root = std::env::temp_dir().join(format!(
            "patch-stems-cache-{}-{}",
            std::process::id(),
            now()
        ));
        let c = Cache::new(root.join("audio"), root.join("metadata"), 0).unwrap();
        let key = "a".repeat(64);
        let staging = root.join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("vocals.wv"), [1; 20]).unwrap();
        let set:StemSet=serde_json::from_value(json!({"version":1,"id":key,"source_hash":"source","track_id":"track","model_id":"test","model_version":"1","weights_digest":"test","profile":"test","pipeline_version":1,"backend":"cpu","sample_rate":48000,"frames":1,"duration":1.0/48000.0,"layout":["vocals"],"stems":{"vocals":{"id":"vocals","cache_ref":"vocals.wv","codec":"wavpack","checksum":"x","bytes":20,"frames":1,"start_frame":0,"peak":0.0,"activity":[],"waveform":[]}},"created":0})).unwrap();
        assert_eq!(set.stems["vocals"].activity_hz, 1);
        c.publish(&set, &staging).unwrap();
        let lease = c.acquire(&key).unwrap();
        assert_eq!(c.cleanup().unwrap()["removed"], 0);
        drop(lease);
        c.pin(&key, "set:one", true).unwrap();
        c.pin(&key, "track:one", true).unwrap();
        c.pin(&key, "set:one", false).unwrap();
        assert_eq!(c.cleanup().unwrap()["removed"], 0);
        c.pin(&key, "track:one", false).unwrap();
        assert_eq!(c.cleanup().unwrap()["removed"], 1);
        assert!(!c.list().is_empty());
        assert!(c.acquire(&key).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn releasing_pins_keeps_the_set_being_played_and_the_ai_hold() {
        let root = std::env::temp_dir().join(format!(
            "patch-stems-release-{}-{}",
            std::process::id(),
            now()
        ));
        let c = Cache::new(root.join("audio"), root.join("metadata"), 0).unwrap();
        let (a, b) = ("a".repeat(64), "b".repeat(64));
        for (key, pins) in [(&a, json!(["old-track", "ai-transition"])), (&b, json!(["set-track", "old-track"]))] {
            atomic_json(&c.metadata.join(format!("{key}.json")), &json!({"pins": pins})).unwrap();
        }
        let keep = ["set-track".to_string()].into_iter().collect();
        assert_eq!(c.release_pins_except(&keep, &["ai-transition"]).unwrap(), 2);
        let pins = |k: &str| read(&c.metadata.join(format!("{k}.json"))).unwrap()["pins"].clone();
        assert_eq!(pins(&a), json!(["ai-transition"]));
        assert_eq!(pins(&b), json!(["set-track"]));
        std::fs::remove_dir_all(root).unwrap();
    }
}
