//! Broadcast Unit: sending Patch's audio somewhere else.
//!
//! One detached process captures a PipeWire source (the same sources the
//! Recorder offers) and hands it to an encoder that pushes it to a server or
//! writes it to a file. It is started by Patch, so a stream keeps running
//! when a window closes, Patch's UI restarts or updates.
//!
//! It is controlled over `$XDG_RUNTIME_DIR/omarchy-patch/broadcast.sock`:
//! one JSON line per request — `status`, `stop` — so the UI and the CLI can
//! both read it without touching audio.

pub mod daemon;
pub mod video;
