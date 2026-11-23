//! Sample folders: what `s "name"` plays when a folder of that name holds
//! sounds.
//!
//! Tidal's convention (SuperDirt's Dirt-Samples): one folder per sound, its
//! files in name order; `n` (or `name:n`) picks one and wraps, so `bd:3`
//! in a folder of three files plays the first again. Patch reads its own
//! folder first, `~/.local/share/omarchy-patch/samples/<name>/`, then a
//! SuperDirt install's Dirt-Samples if there is one, so code written for
//! SuperDirt plays as written. A folder named like a built-in sound (`bd`)
//! wins over it; the built-in plays when there is no folder.
//!
//! Anything that writes a file into a folder (by hand, a deck's sample
//! maker, a download) is picked up at the next evaluation: the index
//! rescans when a folder's modification time changes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

/// Files a folder may hold (others are ignored).
pub const EXTENSIONS: &[&str] = &[
    "wav", "wave", "flac", "aif", "aiff", "ogg", "oga", "mp3", "opus", "m4a",
];

/// One sound: its files, in the order `n` counts them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Folder {
    pub files: Vec<PathBuf>,
    /// Which root it came from (0: Patch's own).
    pub root: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Index {
    folders: BTreeMap<String, Folder>,
    roots: Vec<PathBuf>,
    /// Every directory read, with its modification time then.
    stamps: Vec<(PathBuf, Option<SystemTime>)>,
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// A name `s "…"` can say: mini-notation words are letters, digits and
/// `_ . -` (no spaces, no `:` — that picks the file).
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        && !name.starts_with('.')
}

fn audio(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        && !p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('.'))
}

impl Index {
    /// Every folder under `roots` (earlier roots win a name).
    pub fn scan(roots: &[PathBuf]) -> Index {
        let mut ix = Index {
            roots: roots.to_vec(),
            ..Index::default()
        };
        for (r, root) in roots.iter().enumerate() {
            ix.stamps.push((root.clone(), mtime(root)));
            let Ok(dirs) = std::fs::read_dir(root) else { continue };
            let mut dirs: Vec<PathBuf> = dirs.flatten().map(|d| d.path()).filter(|p| p.is_dir()).collect();
            dirs.sort();
            for dir in dirs {
                let Some(name) = dir.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
                    continue;
                };
                ix.stamps.push((dir.clone(), mtime(&dir)));
                if !valid_name(&name) || ix.folders.contains_key(&name) {
                    continue;
                }
                let Ok(files) = std::fs::read_dir(&dir) else { continue };
                let mut files: Vec<PathBuf> =
                    files.flatten().map(|f| f.path()).filter(|p| p.is_file() && audio(p)).collect();
                if files.is_empty() {
                    continue;
                }
                // SuperDirt's order: by file name, as bytes.
                files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
                ix.folders.insert(name, Folder { files, root: r });
            }
        }
        ix
    }

    /// A directory it read changed (a file or folder added, removed,
    /// renamed), or a root appeared.
    pub fn stale(&self) -> bool {
        self.stamps.iter().any(|(p, t)| mtime(p) != *t)
            || self
                .roots
                .iter()
                .filter_map(|r| std::fs::read_dir(r).ok())
                .flat_map(|d| d.flatten())
                .filter(|d| d.path().is_dir())
                .any(|d| !self.stamps.iter().any(|(p, _)| *p == d.path()))
    }

    pub fn get(&self, name: &str) -> Option<&Folder> {
        self.folders.get(name)
    }

    /// The file `n` picks in folder `name` (wrapping, as SuperDirt does).
    pub fn file(&self, name: &str, n: f64) -> Option<&Path> {
        let f = self.folders.get(name)?;
        let i = (n.floor() as i64).rem_euclid(f.files.len() as i64) as usize;
        Some(&f.files[i])
    }

    /// Every sound: its name and how many files it has.
    pub fn names(&self) -> impl Iterator<Item = (&str, usize)> {
        self.folders.iter().map(|(k, f)| (k.as_str(), f.files.len()))
    }

    pub fn len(&self) -> usize {
        self.folders.len()
    }

    pub fn is_empty(&self) -> bool {
        self.folders.is_empty()
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }
}

/// Patch's own sample folder (where new samples go).
pub fn own_root() -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    data.join("omarchy-patch/samples")
}

/// Where sounds are looked for, Patch's own first. `OMARCHY_PATCH_SAMPLES`
/// (paths separated by `:`) replaces the list.
pub fn roots() -> Vec<PathBuf> {
    // Tests never read this machine's folders: a root of their own.
    if cfg!(test) {
        return vec![test_root()];
    }
    if let Some(v) = std::env::var_os("OMARCHY_PATCH_SAMPLES") {
        return std::env::split_paths(&v).filter(|p| p.is_absolute()).collect();
    }
    let mut r = vec![own_root()];
    if let Some(h) = std::env::var_os("HOME") {
        let h = PathBuf::from(h);
        for p in [
            ".local/share/SuperCollider/downloaded-quarks/Dirt-Samples",
            ".local/share/SuperCollider/Extensions/Dirt-Samples",
        ] {
            let p = h.join(p);
            if p.is_dir() {
                r.push(p);
            }
        }
    }
    r
}

/// The root the crate's tests read (empty unless a test fills it).
pub fn test_root() -> PathBuf {
    std::env::temp_dir().join(format!("patch-samples-test-{}", std::process::id()))
}

static CURRENT: RwLock<Option<Arc<Index>>> = RwLock::new(None);

/// The index as last scanned (scanned now the first time).
pub fn index() -> Arc<Index> {
    if let Some(ix) = CURRENT.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return ix.clone();
    }
    refresh()
}

/// Rescanned if a folder changed since (cheap otherwise: a stat per
/// folder). Called when code is evaluated, never per event.
pub fn refresh() -> Arc<Index> {
    let roots = roots();
    {
        let cur = CURRENT.read().unwrap_or_else(|e| e.into_inner());
        if let Some(ix) = cur.as_ref()
            && ix.roots == roots
            && !ix.stale()
        {
            return ix.clone();
        }
    }
    let ix = Arc::new(Index::scan(&roots));
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = Some(ix.clone());
    ix
}

/// The sample names a piece of Tidal code mentions: every word inside its
/// strings that names a folder (`"bd*2 [sn cp:3]"` → bd, sn, cp), so the
/// engine can load them before the code plays.
pub fn mentioned<'a>(ix: &'a Index, code: &str) -> Vec<&'a str> {
    let mut out: Vec<&str> = vec![];
    let mut in_str = false;
    let mut word = String::new();
    let flush = |w: &mut String, out: &mut Vec<&'a str>| {
        if !w.is_empty() {
            let name = w.split(':').next().unwrap_or("");
            if let Some((k, _)) = ix.folders.get_key_value(name)
                && !out.contains(&k.as_str())
            {
                out.push(k.as_str());
            }
            w.clear();
        }
    };
    for line in code.lines() {
        let line = line.split("--").next().unwrap_or("");
        for c in line.chars() {
            if c == '"' {
                flush(&mut word, &mut out);
                in_str = !in_str;
            } else if in_str && (c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':')) {
                word.push(c);
            } else {
                flush(&mut word, &mut out);
            }
        }
        flush(&mut word, &mut out);
        in_str = false;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[&str]) -> PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("patch-samples-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for f in files {
            let p = dir.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"RIFF").unwrap();
        }
        dir
    }

    #[test]
    fn folders_files_and_wrapping() {
        let a = tree(&["bd/BT0A0A7.wav", "bd/BT0A0D0.WAV", "bd/notes.txt", "sn/s.flac", ".hidden/x.wav", "empty/readme"]);
        let b = tree(&["bd/other.wav", "arpy/a.wav", "arpy/b.wav", "arpy/c.wav"]);
        let ix = Index::scan(&[a.clone(), b.clone()]);
        let names: Vec<_> = ix.names().collect();
        assert_eq!(names, vec![("arpy", 3), ("bd", 2), ("sn", 1)]);
        // Patch's own folder wins the name.
        assert_eq!(ix.get("bd").unwrap().root, 0);
        assert!(ix.file("bd", 0.0).unwrap().ends_with("BT0A0A7.wav"));
        assert!(ix.file("bd", 1.0).unwrap().ends_with("BT0A0D0.WAV"));
        assert!(ix.file("bd", 2.0).unwrap().ends_with("BT0A0A7.wav"));
        assert!(ix.file("bd", -1.0).unwrap().ends_with("BT0A0D0.WAV"));
        assert!(ix.file("arpy", 4.7).unwrap().ends_with("b.wav"));
        assert!(ix.file("hh", 0.0).is_none());
        assert!(!ix.stale());
        std::fs::create_dir_all(a.join("hh")).unwrap();
        std::fs::write(a.join("hh/1.wav"), b"RIFF").unwrap();
        assert!(ix.stale());
        assert_eq!(Index::scan(std::slice::from_ref(&a)).get("hh").unwrap().files.len(), 1);
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }

    #[test]
    fn code_mentions_its_samples() {
        let a = tree(&["bd/1.wav", "sn/1.wav", "cp/1.wav", "arpy/1.wav", "jvbass/1.wav"]);
        let ix = Index::scan(std::slice::from_ref(&a));
        let code = "d1 $ s \"bd*2 [sn cp:3]\" # n 1\n-- d2 $ s \"arpy\"\nd3 $ sound \"<jvbass ~>\" # vowel \"a\"";
        assert_eq!(mentioned(&ix, code), vec!["bd", "sn", "cp", "jvbass"]);
        let _ = std::fs::remove_dir_all(a);
    }

    #[test]
    fn names_a_folder_may_have() {
        assert!(valid_name("808bd"));
        assert!(valid_name("bass3"));
        assert!(valid_name("my-kick_2"));
        assert!(!valid_name("two words"));
        assert!(!valid_name("bd:3"));
        assert!(!valid_name(".git"));
        assert!(!valid_name(""));
    }
}
