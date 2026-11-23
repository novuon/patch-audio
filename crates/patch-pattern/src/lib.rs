//! Pattern Unit: algorithmic, temporal control for any Patch unit.
//!
//! Patch owns the contract; pattern engines sit behind it. What the rest of
//! Patch sees:
//!
//! * [`time`]: exact musical time (rational cycles) and spans.
//! * [`ast`], [`program`]: a pattern definition as Patch data (layers, each
//!   a target and a pattern AST) — what the AI director, the Sequencer and the API
//!   build and mutate.
//! * [`event`]: the typed events a running pattern produces, and how each
//!   becomes a Patch action (a Sequencer voice, a derived sample, a
//!   validated command, a visual parameter).
//! * [`backend`]: the engine interface and its capabilities. The Strudel
//!   adapter ([`strudel`], a sidecar process) is the prototype engine; the
//!   Rust reference engine ([`pattern`], [`mini`], [`dsl`]) implements the
//!   same contract for tests and offline use.
//! * [`tempo`], [`scheduler`], [`rt`]: Patch's own clock, lookahead and the
//!   real-time hand-off. Engines never own time.
//! * [`tidal`]: TidalCycles' language, native (the Tidal Unit's engine), and
//!   [`instrument`]: the sounds and controls its events play with.

pub mod ai;
pub mod ast;
pub mod backend;
pub mod dsl;
pub mod event;
pub mod instrument;
pub mod mini;
pub mod pattern;
pub mod program;
pub mod rt;
pub mod samples;
pub mod scheduler;
pub mod strudel;
pub mod tempo;
pub mod tidal;
pub mod time;

pub use ast::{Pat, Transform, Value};
pub use event::{Action, Event, Target};
pub use program::{Layer, Program};
pub use time::{Span, Time};
