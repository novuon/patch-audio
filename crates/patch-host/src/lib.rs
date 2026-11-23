//! Everything that turns a Unit's Slint surfaces into an Omarchy citizen:
//!
//! * [`session`] — one process per Unit session; each surface is its own
//!   top-level Wayland window sharing the session's state.
//! * [`hypr`]    — Hyprland rules, focus and layout-recipe replay.
//! * [`ipc`]     — the control socket (command line, bar, keybindings).
//! * [`shot`]    — headless software rendering for screenshot review.

pub mod heap;
pub mod hypr;
pub mod ipc;
pub mod session;
pub mod shot;

mod bind;
mod curtain;
mod windows;

pub use windows::Windows;

pub use session::{
    HostOps, HostState, HostedApp, Launch, ProfileRow, Session, SettingRow, SurfaceWindow, run,
};
pub use {patch_core, patch_ui, slint};
