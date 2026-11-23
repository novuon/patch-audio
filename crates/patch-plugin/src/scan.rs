//! Finding installed VST3 plugins without loading them: a plugin's code only
//! runs in Patch after a probe in a child process said it loads cleanly.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct BundleInfo {
    pub path: PathBuf,
    pub name: String,
    pub vendor: String,
    pub version: String,
    /// VST3 sub-categories, e.g. "Fx|EQ".
    pub category: String,
    /// The bundle has a binary for this machine.
    pub native: bool,
}

/// Where VST3 bundles live on Linux (the Steinberg list).
pub fn search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(home).join(".vst3"));
    }
    paths.push(PathBuf::from("/usr/lib/vst3"));
    paths.push(PathBuf::from("/usr/local/lib/vst3"));
    paths
}

/// Every `.vst3` bundle in the search paths, by name.
pub fn installed() -> Vec<BundleInfo> {
    let mut found: Vec<BundleInfo> = search_paths()
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "vst3"))
        .map(|path| bundle_info(&path))
        .collect();
    found.sort_by_key(|b| b.name.to_lowercase());
    found.dedup_by(|a, b| a.path == b.path);
    found
}

pub(crate) fn arch_dir() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64-linux"
    } else {
        "x86_64-linux"
    }
}

/// The shared object a bundle holds for this machine.
pub fn binary(bundle: &Path) -> Option<PathBuf> {
    let dir = bundle.join("Contents").join(arch_dir());
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "so"))
}

/// Name, vendor and version from the bundle's `moduleinfo.json` (VST3.7+),
/// else the bundle's file name.
pub fn bundle_info(bundle: &Path) -> BundleInfo {
    let stem = bundle
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut info = BundleInfo {
        path: bundle.to_path_buf(),
        name: stem,
        native: binary(bundle).is_some(),
        ..Default::default()
    };
    // A bundle's own file, read only up to a size a real one has.
    let path = bundle.join("Contents/Resources/moduleinfo.json");
    let Ok(text) = std::fs::metadata(&path)
        .ok()
        .filter(|m| m.len() <= 1 << 20)
        .map(|_| std::fs::read_to_string(&path))
        .transpose()
        .map(Option::unwrap_or_default)
    else {
        return info;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&strip_trailing_commas(&text)) else {
        return info;
    };
    let audio = json["Classes"]
        .as_array()
        .and_then(|c| c.iter().find(|c| c["Category"] == "Audio Module Class"));
    // The bundle's words, shown as they are: no control characters, and no
    // longer than a name is.
    let text = |v: &serde_json::Value| v.as_str().map(plain).filter(|s| !s.is_empty());
    if let Some(name) = audio.and_then(|c| text(&c["Name"])).or_else(|| text(&json["Name"])) {
        info.name = name;
    }
    info.vendor = audio
        .and_then(|c| text(&c["Vendor"]))
        .or_else(|| text(&json["Factory Info"]["Vendor"]))
        .unwrap_or_default();
    info.version = audio
        .and_then(|c| text(&c["Version"]))
        .or_else(|| text(&json["Version"]))
        .unwrap_or_default();
    info.category = audio
        .and_then(|c| c["Sub Categories"].as_array())
        .map(|subs| subs.iter().filter_map(|s| s.as_str()).take(8).map(plain).collect::<Vec<_>>().join("|"))
        .unwrap_or_default();
    info
}

/// Text from a plugin's own file, fit to show.
fn plain(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(80).collect::<String>().trim().to_string()
}

/// `moduleinfo.json` is JSON5-flavoured: trailing commas before `}`/`]`.
fn strip_trailing_commas(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if in_string {
            out.push(c);
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        if c == '"' {
            in_string = true;
        } else if c == ','
            && chars[i + 1..]
                .iter()
                .find(|n| !n.is_whitespace())
                .is_some_and(|n| *n == '}' || *n == ']')
        {
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_info_with_trailing_commas_parses() {
        let text = r#"{ "Name": "A, b", "Classes": [ { "Category": "Audio Module Class", "Name": "Fifth Order", "Vendor": "m.media", "Sub Categories": ["Fx", "EQ",], }, ], }"#;
        let json: serde_json::Value = serde_json::from_str(&strip_trailing_commas(text)).unwrap();
        assert_eq!(json["Name"], "A, b");
        assert_eq!(json["Classes"][0]["Sub Categories"][1], "EQ");
    }

    #[test]
    fn a_bundles_own_words_are_shown_plain_and_short() {
        let dir = std::env::temp_dir().join(format!("patch-scan-{}", std::process::id()));
        let bundle = dir.join("Odd.vst3");
        std::fs::create_dir_all(bundle.join("Contents/Resources")).unwrap();
        let long = "x".repeat(5000);
        let info = format!(
            r#"{{"Name": "Odd", "Classes": [{{"Category": "Audio Module Class", "Name": "<b>Bold</b>\u001b[31m", "Vendor": "{long}"}}]}}"#
        );
        std::fs::write(bundle.join("Contents/Resources/moduleinfo.json"), info).unwrap();
        let b = bundle_info(&bundle);
        // Markup-shaped text stays text (Slint draws it as it is); escapes go.
        assert_eq!(b.name, "<b>Bold</b>[31m");
        assert_eq!(b.vendor.chars().count(), 80);
        // A moduleinfo.json too big to be one is not read.
        std::fs::write(bundle.join("Contents/Resources/moduleinfo.json"), " ".repeat(2 << 20)).unwrap();
        assert_eq!(bundle_info(&bundle).name, "Odd");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
