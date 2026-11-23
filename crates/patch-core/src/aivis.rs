//! The AI DJ's visual state contract (docs/architecture/ADR-007).
//!
//! The Patch service owns the AI DJ and publishes this structure; every AI
//! DJ surface (compact, operator, performance, analysis) only renders it.
//! No surface infers AI state on its own. The structure is small, plain
//! data, versioned by field (unknown fields are ignored, missing ones
//! default), so a renderer can run in another process, from a fixture, or
//! against a future model without changing.
//!
//! Timing: `clock` is the engine's latest stamped cycle; each source's
//! `beat` is at that stamp. A renderer interpolates
//! `beat + (now − clock.nsec) · beats_per_ns` for smooth motion, and delays
//! by `clock.output_latency_ns` to match what is heard. Animation clocks
//! are never musical truth.

use serde::{Deserialize, Serialize};

/// Where the AI stands on acting (explicit, never blurred).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Watches and analyses; changes nothing.
    #[default]
    Observe,
    /// Shows candidates; the human decides everything.
    Suggest,
    /// Prepares the next deck and plan; runs it only when approved.
    Prepare,
    /// Runs transitions itself (switched on by a human; any touch wins).
    Act,
}

/// What the AI is doing inside its mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Nothing to listen to (no deck playing).
    #[default]
    Idle,
    /// Following the music; no decision forming.
    Listening,
    /// Computing candidates (latency visible, never a spinner).
    Evaluating,
    /// Several candidates coexist.
    Comparing,
    /// One candidate chosen; the plan is being built / the deck loaded.
    Preparing,
    /// Plan validated, deck loaded, cued, synced: waiting for its bar or
    /// for approval.
    Ready,
    /// The transition is running.
    Executing,
    /// Watching the result after a transition.
    Monitoring,
    /// A human took over; re-evaluating.
    Recovering,
    /// The AI can't reason (no analysis, service trouble): music plays on.
    Unavailable,
}

/// One musical source (a deck today; stems, samples, inputs later).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Source {
    /// "deck-a", "deck-b", …
    pub id: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    /// How much of the output it is (0…1, fader × crossfader).
    pub presence: f32,
    /// Beats since the first downbeat at `clock`, and the tempo as played.
    pub beat: Option<f64>,
    pub bpm: Option<f64>,
    /// Uncalibrated beat-grid evidence index 0…1 (low: timing geometry turns uncertain).
    pub beat_confidence: f32,
    pub key: Option<String>,
    /// Energy now and over the next seconds (0…1 per second).
    pub energy: f32,
    pub energy_ahead: Vec<f32>,
    /// Seconds played and left.
    pub position: f64,
    pub remaining: f64,
}

/// Why a candidate fits (or doesn't), factor by factor, each −1…1.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Factors {
    pub tempo: f32,
    pub harmony: f32,
    pub energy: f32,
    pub phrase: f32,
    pub history: f32,
}

/// A possible next track.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Candidate {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub bpm: Option<f64>,
    pub key: Option<String>,
    /// Overall fit (0…1) and an uncalibrated evidence support index (0…1).
    pub score: f32,
    /// Uncalibrated evidence support index, not a probability.
    pub confidence: f32,
    pub factors: Factors,
    /// Structured reasons ("+ 9A → 10A on the wheel", "~ energy +4").
    pub reasons: Vec<String>,
    /// Energy change it would bring (−1…1).
    pub energy_delta: f32,
    /// Camelot steps from what plays (0 same, ±1 neighbour, …).
    pub key_steps: i32,
    /// The chosen one; locked by the DJ; rejected by the DJ.
    pub selected: bool,
    pub locked: bool,
    pub rejected: bool,
}

/// The transition the AI intends (from the real TransitionPlan).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transition {
    pub from: String,
    pub to: String,
    /// "blend", "filterout", "cut", "loopout".
    pub style: String,
    pub bars: f32,
    /// Where it starts, in beats of the outgoing source's grid.
    pub start_beat: f64,
    /// Loops or effects it uses (for the trajectory's shape).
    pub loops: bool,
    pub effects: bool,
    /// Steps done / total (execution), and whether it has started.
    pub done: usize,
    pub total: usize,
    pub started: bool,
    /// Prepared and validated: deck loaded, cued, synced, plan compiled.
    pub ready: bool,
    /// Incoming deck's actual transport position, if that deck is loaded.
    pub incoming_position: Option<f64>,
    /// Planned cue position on the incoming track.
    pub cue_seconds: Option<f64>,
    /// Uncalibrated evidence support index, not a probability.
    pub confidence: f32,
}

/// The room, when there are sensors (none yet: the field is absent).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Environment {
    /// Sensors actually in use (e.g. "microphone"); empty = no field.
    pub sensors: Vec<String>,
    /// Crowd/room energy 0…1 and how reliable the reading is 0…1.
    pub energy: f32,
    pub reliability: f32,
}

/// The engine clock the timing refers to.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Clock {
    /// CLOCK_MONOTONIC ns of the stamped cycle.
    pub nsec: u64,
    /// Output latency to the ear (ns) and its level ("reported", "measured").
    pub output_latency_ns: u64,
    pub latency_level: String,
}

/// A decision the AI made (for trails that decay).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Decision {
    pub title: String,
    pub style: String,
    /// Seconds ago.
    pub age_s: f64,
}

/// Everything an AI DJ surface shows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VisualState {
    pub mode: Mode,
    pub phase: Phase,
    /// "service" in production; "demo:<fixture>" for fixtures (always shown).
    pub origin: String,
    /// Where the set is heading: "build", "hold", "release"; and the set's
    /// section when a set runs ("warm-up", "build", "peak", "release").
    pub direction: String,
    pub section: String,
    pub sources: Vec<Source>,
    pub candidates: Vec<Candidate>,
    pub transition: Option<Transition>,
    /// Uncalibrated evidence support index 0…1 and the biggest known
    /// uncertainty. A probability requires future calibration data.
    pub confidence: f32,
    pub uncertainty: Vec<String>,
    pub environment: Option<Environment>,
    pub clock: Clock,
    pub history: Vec<Decision>,
    /// Seconds since a human last took over (None: not recently).
    pub override_age_s: Option<f64>,
    /// How long the last evaluation took (ms) and how long ago (s).
    pub eval_ms: f64,
    pub eval_age_s: f64,
    /// A one-line note ("waiting for a playing deck", …).
    pub note: String,
    /// What the prepared transition rests on (empty when nothing is).
    pub evidence: Evidence,
}

/// The evidence behind the prepared transition, for the AI panel.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Evidence {
    /// Its offline rehearsal (0…100) and the named defects found.
    pub rehearsal_score: Option<f32>,
    pub rehearsal_defects: Vec<String>,
    /// Calibrated probability that both tracks' tempos are right.
    pub tempo_likely: Option<f32>,
    /// The incoming key: "tagged" (the file's) or "detected" (Patch's).
    pub key_source: String,
    /// "conventional" or "drum swap: drums from …", and its state.
    pub arrangement: String,
    pub arrangement_state: String,
    /// The pair's prepared stems: "ready", "preparing", "evidence only", "none".
    pub stems_pair: String,
}

impl VisualState {
    /// The playing source that leads (most present).
    pub fn lead(&self) -> Option<&Source> {
        self.sources
            .iter()
            .filter(|s| s.playing)
            .max_by(|a, b| a.presence.total_cmp(&b.presence))
    }

    pub fn selected(&self) -> Option<&Candidate> {
        self.candidates.iter().find(|c| c.selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_json_fills_in_defaults() {
        let v: VisualState = serde_json::from_str(
            r#"{"mode":"act","phase":"ready","candidates":[{"title":"x","score":0.8}]}"#,
        )
        .unwrap();
        assert_eq!((v.mode, v.phase), (Mode::Act, Phase::Ready));
        assert_eq!(v.candidates[0].title, "x");
        assert!(v.environment.is_none(), "no sensors: no environment field");
    }
}
