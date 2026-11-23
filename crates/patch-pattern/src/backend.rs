//! The pattern engine contract.
//!
//! Patch owns time, scheduling, delivery and the event types; an engine
//! only answers "which events are in this span?" for a definition it was
//! given. That is the whole interface, so an engine can be an external
//! process (the Strudel adapter), Patch's Rust reference engine, or a future
//! native engine, and nothing above it changes.

use crate::event::Event;
use crate::program::Program;
use crate::time::{Span, Time};
use serde::{Deserialize, Serialize};

/// What a running pattern is defined as.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Definition {
    /// Patch data: layers of targets and pattern ASTs (what the AI director, the
    /// Sequencer and the API build). Every engine must accept it.
    Program { program: Program },
    /// Live code in an engine's own language (a user-facing compatibility
    /// mode): "strudel", or "patch" for the reference engine's language.
    Source {
        language: String,
        code: String,
        #[serde(default = "four")]
        cycle_beats: Time,
    },
}

fn four() -> Time {
    Time::whole(4)
}

impl Definition {
    pub fn cycle_beats(&self) -> Time {
        match self {
            Definition::Program { program } => program.cycle_beats,
            Definition::Source { cycle_beats, .. } => *cycle_beats,
        }
    }
}

/// An error in a definition, located when the engine can say where.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PatternError {
    #[serde(default)]
    pub line: usize,
    #[serde(default)]
    pub col: usize,
    pub message: String,
}

impl PatternError {
    pub fn new(message: impl Into<String>) -> PatternError {
        PatternError {
            line: 0,
            col: 0,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PatternError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        if self.line > 0 {
            write!(f, "{}:{}: {}", self.line, self.col, self.message)
        } else {
            f.write_str(&self.message)
        }
    }
}

impl From<crate::dsl::DslError> for PatternError {
    fn from(e: crate::dsl::DslError) -> PatternError {
        PatternError {
            line: e.line,
            col: e.col,
            message: e.message,
        }
    }
}

/// One layer as the engine understood it (for the surface and AI).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerInfo {
    pub name: String,
    pub target: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Loaded {
    pub layers: Vec<LayerInfo>,
    /// How long the engine took to evaluate it.
    pub eval_us: u64,
    /// A tempo the code sets (Tidal's `setcps`), in BPM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bpm: Option<f64>,
    /// What loaded but won't sound as written (an unknown sound name).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// What an engine can do: callers check, never assume.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    pub engine: String,
    pub version: String,
    /// Source languages it evaluates ("strudel", "patch").
    pub languages: Vec<String>,
    /// Accepts a Patch `Program` (AST) definition.
    pub program: bool,
    pub euclidean: bool,
    pub polymeter: bool,
    pub probability: bool,
    /// The same definition and span always give the same events.
    pub deterministic: bool,
    /// Events carry the source position they came from.
    pub source_locations: bool,
    /// Live code can't touch the host (files, processes, network).
    pub sandboxed: bool,
    /// Runs in another process (a crash doesn't take Patch down).
    pub out_of_process: bool,
    /// Patch AST nodes it can't translate (each a `Pat`/`Transform` name).
    pub unsupported: Vec<String>,
}

pub trait Backend: Send {
    fn capabilities(&self) -> Capabilities;

    /// Evaluate `def` into `slot` (replacing what was there). On error the
    /// slot is untouched: whatever was playing keeps playing.
    fn load(&mut self, slot: u32, def: &Definition) -> Result<Loaded, Vec<PatternError>>;

    /// The events that start in `span` (exact times), in time order.
    fn query(&mut self, slot: u32, span: Span) -> Result<Vec<Event>, PatternError>;

    fn unload(&mut self, slot: u32);
}

/// Patch's Rust reference engine: the AST and the `patch` language,
/// in-process. Deterministic; used by tests, headless tools, and as the
/// fallback when no external engine is installed.
#[derive(Default)]
pub struct Reference {
    slots: std::collections::HashMap<u32, crate::program::Compiled>,
}

impl Backend for Reference {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            engine: "patch-reference".into(),
            version: patch_core::VERSION.into(),
            languages: vec!["patch".into()],
            program: true,
            euclidean: true,
            polymeter: true,
            probability: true,
            deterministic: true,
            source_locations: false,
            sandboxed: true,
            out_of_process: false,
            unsupported: vec![],
        }
    }

    fn load(&mut self, slot: u32, def: &Definition) -> Result<Loaded, Vec<PatternError>> {
        let t0 = std::time::Instant::now();
        let program = match def {
            Definition::Program { program } => {
                program.validate().map_err(|e| vec![PatternError::new(e)])?;
                program.clone()
            }
            Definition::Source {
                language,
                code,
                cycle_beats,
            } => {
                if language != "patch" {
                    return Err(vec![PatternError::new(format!(
                        "the reference engine speaks 'patch', not '{language}'"
                    ))]);
                }
                let mut p = crate::dsl::parse(code)
                    .map_err(|e| e.into_iter().map(PatternError::from).collect::<Vec<_>>())?;
                if !code.lines().any(|l| l.trim_start().starts_with("cycle")) {
                    p.cycle_beats = *cycle_beats;
                }
                p
            }
        };
        let layers = program
            .layers
            .iter()
            .map(|l| LayerInfo {
                name: l.name.clone(),
                target: l.target.label(),
            })
            .collect();
        self.slots.insert(slot, program.compile());
        Ok(Loaded {
            layers,
            eval_us: t0.elapsed().as_micros() as u64,
            ..Default::default()
        })
    }

    fn query(&mut self, slot: u32, span: Span) -> Result<Vec<Event>, PatternError> {
        let c = self
            .slots
            .get(&slot)
            .ok_or_else(|| PatternError::new("nothing loaded"))?;
        Ok(c.events(span))
    }

    fn unload(&mut self, slot: u32) {
        self.slots.remove(&slot);
    }
}
