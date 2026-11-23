//! The asset cache: rendered audio and its metadata, under a budget.
//!
//! Modelled on the stem cache (`patch-stems::cache`): content-addressed files
//! (`<id>.f32` audio at the graph rate, `<id>.json` the [`Asset`]), a byte
//! budget from `PATCH_MATERIAL_CACHE_BYTES` (default 512 MB), consumer
//! references, and LRU eviction of unreferenced assets under pressure.
//!
//! Regeneration: a missing `.f32` is re-derived from the recipe by the
//! service. If the source's fingerprint no longer matches the recipe, the
//! asset is **stale** and says so — it is never silently rebuilt from a
//! different file.

use crate::model::{Asset, Policy};
use patch_core::material::AssetId;
use std::path::{Path, PathBuf};

pub const DEFAULT_BUDGET: u64 = 512 * 1024 * 1024;

/// What one cleanup did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub removed: usize,
    pub freed: u64,
    pub kept: usize,
    /// bytes / budget after the cleanup (0…1+).
    pub pressure: f32,
}

/// What the cache holds right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    pub assets: usize,
    pub bytes: u64,
    pub budget: u64,
    pub pressure: f32,
    pub referenced: usize,
}

use serde::{Deserialize, Serialize};

pub struct Cache {
    pub audio: PathBuf,
    pub state: PathBuf,
    pub budget: u64,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Cache {
    pub fn new(audio: PathBuf, state: PathBuf, budget: u64) -> Result<Cache, String> {
        for p in [&audio, &state] {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        Ok(Cache {
            audio,
            state,
            budget,
        })
    }

    /// `$XDG_CACHE_HOME/omarchy-patch/material` and
    /// `$XDG_STATE_HOME/omarchy-patch/material`, `PATCH_MATERIAL_CACHE_BYTES`.
    pub fn default_cache() -> Result<Cache, String> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("HOME is unavailable")?;
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cache"))
            .join("omarchy-patch/material");
        let state = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/state"))
            .join("omarchy-patch/material");
        let budget = std::env::var("PATCH_MATERIAL_CACHE_BYTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_BUDGET);
        Cache::new(cache, state, budget)
    }

    pub fn audio_path(&self, id: &AssetId) -> PathBuf {
        self.audio.join(format!("{}.f32", id.0))
    }

    pub fn meta_path(&self, id: &AssetId) -> PathBuf {
        self.state.join(format!("{}.json", id.0))
    }

    pub fn has(&self, id: &AssetId) -> bool {
        self.audio_path(id).exists() && self.meta_path(id).exists()
    }

    /// Write an asset's audio and metadata. Atomic: the audio lands first, so
    /// a partial metadata write is never read as a complete asset.
    pub fn store(&self, asset: &Asset, audio: &[f32]) -> Result<(), String> {
        let ap = self.audio_path(&asset.id);
        let mut bytes = Vec::with_capacity(audio.len() * 4);
        for s in audio {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let tmp = ap.with_extension("f32.tmp");
        std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &ap).map_err(|e| e.to_string())?;
        let mp = self.meta_path(&asset.id);
        let tmp = mp.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(asset).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &mp).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn load(&self, id: &AssetId) -> Option<Asset> {
        let text = std::fs::read(self.meta_path(id)).ok()?;
        serde_json::from_slice(&text).ok()
    }

    pub fn load_audio(&self, id: &AssetId) -> Result<Vec<f32>, String> {
        let bytes = std::fs::read(self.audio_path(id)).map_err(|e| e.to_string())?;
        if bytes.len() % 4 != 0 {
            return Err("material audio is not f32-aligned".into());
        }
        Ok(bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect())
    }

    /// Keep the asset for `consumer`. Idempotent; bumps the file's mtime so
    /// it reads as in use.
    pub fn reference(&self, id: &AssetId, consumer: &str) -> Result<(), String> {
        let mut asset = self.load(id).ok_or_else(|| format!("{id} is not cached"))?;
        if !asset.meta.consumers.iter().any(|c| c == consumer) {
            asset.meta.consumers.push(consumer.to_string());
        }
        self.write_meta(&asset)
    }

    /// Release `consumer`'s reference. The asset becomes evictable under its
    /// policy when no consumer remains.
    pub fn release(&self, id: &AssetId, consumer: &str) -> Result<(), String> {
        let mut asset = self.load(id).ok_or_else(|| format!("{id} is not cached"))?;
        asset.meta.consumers.retain(|c| c != consumer);
        self.write_meta(&asset)
    }

    fn write_meta(&self, asset: &Asset) -> Result<(), String> {
        let mp = self.meta_path(&asset.id);
        let tmp = mp.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(asset).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &mp).map_err(|e| e.to_string())
    }

    pub fn referenced(&self, id: &AssetId) -> bool {
        self.load(id).is_some_and(|a| !a.meta.consumers.is_empty())
    }

    /// Is `asset` stale against the source's current fingerprint? A mismatch
    /// (file changed, replaced or moved onto another file) is the answer.
    pub fn stale(asset: &Asset, current_fingerprint: Option<&str>) -> bool {
        match current_fingerprint {
            Some(f) => !f.eq_ignore_ascii_case(&asset.recipe.source.fingerprint),
            None => true,
        }
    }

    /// Every cached (asset, bytes, last use) — for stats and cleanup.
    fn entries(&self) -> Vec<(Asset, u64, u64)> {
        let Ok(dir) = std::fs::read_dir(&self.state) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for e in dir.flatten() {
            let p = e.path();
            let Some(id) = p
                .file_stem()
                .and_then(|s| s.to_str())
                .filter(|_| p.extension().and_then(|x| x.to_str()) == Some("json"))
            else {
                continue;
            };
            let id = AssetId(id.to_string());
            let Some(asset) = self.load(&id) else {
                continue;
            };
            let mtime = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0)
                + std::fs::metadata(self.audio_path(&id))
                    .map(|m| m.len())
                    .unwrap_or(0);
            out.push((asset, bytes, mtime));
        }
        out
    }

    pub fn stats(&self) -> Stats {
        let entries = self.entries();
        let bytes: u64 = entries.iter().map(|(_, b, _)| *b).sum();
        Stats {
            assets: entries.len(),
            bytes,
            budget: self.budget,
            pressure: if self.budget > 0 {
                bytes as f32 / self.budget as f32
            } else {
                0.0
            },
            referenced: entries
                .iter()
                .filter(|(a, _, _)| !a.meta.consumers.is_empty())
                .count(),
        }
    }

    /// Whether `asset` may be evicted now: no consumers, and its policy's
    /// window has passed.
    fn evictable(&self, asset: &Asset, last_use: u64, now: u64) -> bool {
        if !asset.meta.consumers.is_empty() {
            return false;
        }
        match asset.meta.policy {
            Policy::WhileReferenced => true,
            Policy::Reuse { seconds } => now.saturating_sub(last_use) as f64 >= seconds,
            Policy::Session | Policy::Keep => false,
        }
    }

    /// Remove unreferenced assets until the cache fits its budget (oldest and
    /// least-kept policy first). Returns what happened.
    pub fn cleanup(&self) -> Report {
        let now = now();
        let mut entries = self.entries();
        let mut used: u64 = entries.iter().map(|(_, b, _)| *b).sum();
        // Keep-heavy policies go last; then least recently used.
        let rank = |a: &Asset| match a.meta.policy {
            Policy::WhileReferenced => 0,
            Policy::Reuse { .. } => 1,
            Policy::Session => 2,
            Policy::Keep => 3,
        };
        entries.sort_by(|a, b| rank(&a.0).cmp(&rank(&b.0)).then(a.2.cmp(&b.2)));
        let (mut removed, mut freed) = (0usize, 0u64);
        for (asset, bytes, last_use) in &entries {
            if used <= self.budget {
                break;
            }
            if !self.evictable(asset, *last_use, now) {
                continue;
            }
            let _ = std::fs::remove_file(self.audio_path(&asset.id));
            let _ = std::fs::remove_file(self.meta_path(&asset.id));
            removed += 1;
            freed += bytes;
            used = used.saturating_sub(*bytes);
        }
        Report {
            removed,
            freed,
            kept: entries.len() - removed,
            pressure: if self.budget > 0 {
                used as f32 / self.budget as f32
            } else {
                0.0
            },
        }
    }

    /// Remove the audio of an asset so it can be re-derived from its recipe.
    pub fn forget_audio(&self, id: &AssetId) -> Result<(), String> {
        match std::fs::remove_file(self.audio_path(id)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// The cache's directories, for callers that log or clean them.
    pub fn dirs(&self) -> (&Path, &Path) {
        (&self.audio, &self.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, Meta, Recipe, Resolved, SourceRef, VERSION};

    fn dir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "patch-material-cache-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn cache(name: &str, budget: u64) -> Cache {
        let root = dir(name);
        Cache::new(root.join("audio"), root.join("state"), budget).unwrap()
    }

    fn asset(n: u8, policy: Policy) -> Asset {
        let recipe = Recipe {
            version: VERSION,
            source: SourceRef {
                track_id: "track-a".into(),
                path: "/music/a.flac".into(),
                fingerprint: format!("{n:064x}"),
                stem: None,
                stem_set: None,
            },
            region: Resolved {
                rate: 48_000,
                start_frame: 0,
                end_frame: 4800,
                first_beat: 0.0,
                beats: 4.0,
                bar: 0,
                phrase: Some(0),
                source_bpm: Some(128.0),
                source_key: None,
                confidence: 0.9,
                why: "bars 0…1".into(),
            },
            kind: Kind::BarLoop,
            transforms: vec![],
            analysis_version: 7,
        };
        Asset::new(
            recipe,
            Meta {
                frames: 4800,
                duration: 0.1,
                policy,
                ..Default::default()
            },
        )
    }

    #[test]
    fn audio_and_metadata_round_trip() {
        let c = cache("roundtrip", DEFAULT_BUDGET);
        let a = asset(1, Policy::WhileReferenced);
        let audio: Vec<f32> = (0..4800).map(|i| (i as f32 / 4800.0) - 0.5).collect();
        c.store(&a, &audio).unwrap();
        assert!(c.has(&a.id));
        assert_eq!(c.load_audio(&a.id).unwrap(), audio);
        assert_eq!(c.load(&a.id).unwrap().id, a.id);
        // A different recipe with the same region is a different file.
        let b = asset(2, Policy::WhileReferenced);
        assert!(!c.has(&b.id));
    }

    #[test]
    fn cleanup_respects_references_and_budget() {
        let c = cache("budget", 1);
        let a = asset(1, Policy::WhileReferenced);
        let b = asset(2, Policy::WhileReferenced);
        c.store(&a, &[0.0f32; 1000]).unwrap();
        c.store(&b, &[0.0f32; 1000]).unwrap();
        c.reference(&a.id, "pattern").unwrap();
        let rep = c.cleanup();
        assert!(rep.removed >= 1);
        assert!(c.has(&a.id), "a referenced asset is never evicted");
        assert_eq!(c.stats().referenced, 1);
    }

    #[test]
    fn a_session_policy_is_not_evicted_mid_session() {
        let c = cache("session", 1);
        let a = asset(1, Policy::Session);
        c.store(&a, &[0.0f32; 1000]).unwrap();
        let rep = c.cleanup();
        assert_eq!(rep.removed, 0);
        assert!(c.has(&a.id));
    }

    #[test]
    fn releasing_lets_it_go_and_stale_is_detected() {
        let c = cache("release", 1);
        let a = asset(1, Policy::WhileReferenced);
        c.store(&a, &vec![0.0f32; 1000]).unwrap();
        c.reference(&a.id, "kit").unwrap();
        c.release(&a.id, "kit").unwrap();
        assert!(!c.referenced(&a.id));
        assert_eq!(c.cleanup().removed, 1);
        assert!(!c.has(&a.id));

        let loaded = {
            let c2 = cache("stale", DEFAULT_BUDGET);
            let a2 = asset(3, Policy::WhileReferenced);
            c2.store(&a2, &[0.0f32; 10]).unwrap();
            c2.load(&a2.id).unwrap()
        };
        let same = format!("{:064x}", 3u8);
        assert!(!Cache::stale(&loaded, Some(&same)), "same file");
        assert!(Cache::stale(&loaded, Some(&"f".repeat(64))), "changed file");
        assert!(Cache::stale(&loaded, None), "missing file is stale");
    }
}
