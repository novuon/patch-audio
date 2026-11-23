//! Streams the user routed through Patch, by node name, saved to
//! `~/.config/omarchy-patch/routes` so routing survives Patch restarts.

use std::collections::BTreeSet;
use std::path::PathBuf;

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("omarchy-patch/routes"))
}

pub fn load() -> BTreeSet<String> {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

pub fn save(set: &BTreeSet<String>) {
    let Some(p) = path() else { return };
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    let body: String = set.iter().map(|s| format!("{s}\n")).collect();
    let _ = std::fs::write(p, body);
}
