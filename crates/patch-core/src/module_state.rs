//! Where modules live on this computer and what state they are in: what
//! the module worker installed (its files), the downloads queued or
//! running. Read by `omarchy-patch modules`, the installer and the
//! controller (Settings › Modules); only files, no network, so it is quick
//! enough for a window to ask.

use crate::modules::Module;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

pub fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).unwrap_or_else(|| home().join(fallback))
}

/// Patch's data folder (`~/.local/share/omarchy-patch`).
pub fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share").join("omarchy-patch")
}

pub fn state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state").join("omarchy-patch/modules")
}

pub fn cache_dir() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache").join("omarchy-patch/downloads")
}

/// Where a payload unpacks.
pub fn dest_root(dest: &str, id: &str) -> PathBuf {
    match dest {
        "themes" => data_dir().join("themes"),
        _ => data_dir().join("modules").join(id),
    }
}

// ---------------------------------------------------------------- records

/// What the worker installed: per module, its files (so removing it
/// deletes those and nothing of the user's beside them).
#[derive(Default, Serialize, Deserialize)]
pub struct Installed {
    pub modules: BTreeMap<String, Record>,
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Record {
    pub version: String,
    /// Payload file → its SHA-256 (an update with the same hash is skipped).
    pub payloads: BTreeMap<String, String>,
    pub files: Vec<PathBuf>,
    pub bytes: u64,
}

pub fn installed_path() -> PathBuf {
    data_dir().join("modules/installed.json")
}

pub fn load_installed() -> Installed {
    std::fs::read_to_string(installed_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}


/// Whether a download module's files are here: installed by the worker, or
/// by an older installer that copied everything (0.7.x).
pub fn present(m: &Module, installed: &Installed) -> bool {
    if let Some(r) = installed.modules.get(&m.id) {
        // Its files, not only the record (an uninstall takes the themes).
        return r.files.iter().all(|f| f.exists());
    }
    let themes = data_dir().join("themes");
    match m.id.as_str() {
        // Downloaded or copied by an older installer; or a system package's.
        "themes" => {
            themes.join("patch-light/colors.toml").is_file()
                || std::path::Path::new("/usr/share/omarchy-patch/themes/patch-light/colors.toml").is_file()
        }
        "wallpapers" => std::fs::read_dir(themes.join("patch-dark/backgrounds/intros")).is_ok_and(|mut d| d.next().is_some()),
        "stems" => stems_runtime().join("bin/python").exists(),
        _ => !m.download && m.step.is_none(),
    }
}

pub fn stems_runtime() -> PathBuf {
    data_dir().join("stems/runtime")
}

/// On: its switch is on (for a switch) and its files are here.
pub fn is_on(m: &Module, s: &crate::setup::Setup, installed: &Installed) -> bool {
    let switch = !m.switch || s.get(&m.id) == Some(true);
    switch && present(m, installed)
}

// ---------------------------------------------------------------- jobs

#[derive(Default, Serialize, Deserialize)]
pub struct Jobs {
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub jobs: Vec<Job>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    /// queued, downloading, verifying, installing, done, failed, cancelled
    pub state: String,
    #[serde(default)]
    pub done: u64,
    #[serde(default)]
    pub total: u64,
    /// Bytes a second, while downloading.
    #[serde(default)]
    pub rate: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub tries: u32,
}

impl Job {
    pub fn active(&self) -> bool {
        matches!(self.state.as_str(), "queued" | "downloading" | "verifying" | "installing")
    }
}

pub fn jobs_path() -> PathBuf {
    state_dir().join("jobs.json")
}

pub fn load_jobs() -> Jobs {
    std::fs::read_to_string(jobs_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// A module as the controller shows it.
#[derive(Clone, Debug)]
pub struct ModuleState {
    pub id: String,
    pub name: String,
    pub about: String,
    pub preview: bool,
    /// Its switch is on and its files are here.
    pub on: bool,
    /// Chosen (switch on) but waiting: for its files or its licence.
    pub wanted: bool,
    /// A module without its key here.
    pub locked: bool,
    /// Its key isn't available yet.
    pub coming_soon: bool,
    /// Download running or queued: (state, fraction done).
    pub job: Option<(String, f32)>,
    /// The last download failed.
    pub failed: Option<String>,
    /// Bytes still to download to add it (when the release manifest is
    /// kept here), else the catalogue's estimate.
    pub size: u64,
    /// Has files of its own (removing it deletes them).
    pub has_files: bool,
}

/// Every module of the catalogue, in its order, as it stands here.
pub fn states() -> Vec<ModuleState> {
    let c = crate::modules::builtin();
    let saved = crate::setup::load_saved();
    let running = saved.licensed();
    let installed = load_installed();
    let jobs = load_jobs();
    // Sizes from the release's manifest kept here (display only).
    let kept: Option<crate::modules::Catalogue> = std::fs::read_to_string(state_dir().join("manifest.json"))
        .ok()
        .and_then(|t| crate::modules::parse(&t).ok());
    c.modules
        .iter()
        .filter(|m| !m.hidden)
        .map(|m| {
            let job = jobs.jobs.iter().find(|j| j.id == m.id && j.active());
            let failed = jobs.jobs.iter().find(|j| j.id == m.id && j.state == "failed").and_then(|j| j.error.clone());
            let here = present(m, &installed);
            let size = if here { 0 } else { kept.as_ref().unwrap_or(c).download_size(&m.id, crate::modules::arch()) };
            ModuleState {
                id: m.id.clone(),
                name: m.name.clone(),
                about: m.about.clone(),
                preview: m.preview,
                on: is_on(m, &running, &installed),
                wanted: m.switch && saved.get(&m.id) == Some(true) && !is_on(m, &running, &installed),
                locked: !crate::licence::module_unlocked(&m.id),
                coming_soon: crate::licence::for_module(c, &m.id).is_some_and(|l| l.coming_soon),
                job: job.map(|j| (j.state.clone(), if j.total > 0 { j.done as f32 / j.total as f32 } else { 0.0 })),
                failed,
                size,
                has_files: m.download || m.step.is_some(),
            }
        })
        .collect()
}
