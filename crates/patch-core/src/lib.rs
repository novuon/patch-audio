//! Toolkit-independent core of Patch.
//!
//! Nothing in here knows about Slint, Wayland or audio data. It defines the
//! application's identity, its surfaces and layout recipes, the keyboard
//! grammar, and the line protocol a Patch session speaks over its control
//! socket.

/// Patch's version: the workspace version (SemVer, see CHANGELOG.md).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

static GIT: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// The binary's git commit, set once at start (its build script reads it).
pub fn set_git(git: &'static str) {
    let _ = GIT.set(git);
}

/// Git commit this build came from ("" outside a git checkout, or before
/// the binary set it).
pub fn git() -> &'static str {
    GIT.get().copied().unwrap_or("")
}

/// "v0.1.0 (a1b2c3d)" for about boxes, logs and `--version`.
pub fn version_string() -> String {
    match git() {
        "" => format!("v{VERSION}"),
        git => format!("v{VERSION} ({git})"),
    }
}

pub mod aivis;
pub mod app;
pub mod command;
pub mod eq;
pub mod input;
pub mod instance;
pub mod intent;
pub mod ipc;
pub mod keys;
pub mod desk;
pub mod layout;
pub mod licence;
pub mod material;
pub mod midi;
pub mod mix_style;
pub mod module_state;
pub mod modules;
pub mod tempo_plan;
pub mod pack;
pub mod performance;
pub mod pulse;
pub mod setup;
pub mod surface;
pub mod spsc;
pub mod units;
pub mod user_layouts;
pub mod visual_intent;

pub use app::{AppId, AppSpec};
pub use layout::{LayoutRecipe, LayoutStatus, Tile};
pub use surface::{Origin, SurfaceSpec};
pub use units::{UnitKind, UnitSpec, UnitStatus};

pub mod dj_workspace;

pub mod stems;

pub mod mic;

pub mod stream;
