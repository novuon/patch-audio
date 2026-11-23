//! AI DJ boundary: structured intents, never direct control.
//!
//! An AI (or any automation) proposes a [`TransitionPlan`]: which decks,
//! when (in bars of the outgoing deck), for how long, and the steps
//! ([`DeckAction`], [`MixerAction`], [`EffectAction`]) at bar positions.
//! [`compile`] turns a plan into a timeline of ordinary [`Command`]s — ramps
//! become quarter-beat steps — and validates every one for the plan's
//! origin, so an AI-authored plan runs under the AI limits. What an origin
//! may do with a plan is its [`ControlLevel`]: the AI suggests or prepares;
//! executing needs a human (or a plan a human wrote). A human DJ never needs
//! any of this; nothing here touches the real-time engine directly.

use crate::command::{self, Band, Command, Context, Deck, Origin, Rejected, Transport};
use serde::{Deserialize, Serialize};

/// Where the set should go next (the AI DJ's energy direction).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Build,
    #[default]
    Hold,
    Release,
}

/// How far automation may go.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ControlLevel {
    /// Watch and learn; propose nothing.
    Observe,
    /// Show a plan; the DJ may take it.
    Suggest,
    /// Prepare the next deck (load, cue, sync) but don't mix.
    Prepare,
    /// Run the transition.
    Execute,
}

impl ControlLevel {
    /// The highest level an origin may reach (v1: the AI stops at Prepare).
    pub fn ceiling(origin: Origin) -> ControlLevel {
        match origin {
            Origin::User | Origin::Cli => ControlLevel::Execute,
            Origin::Ai => ControlLevel::Prepare,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransitionStyle {
    /// Long blend, EQ swap on the lows.
    Blend,
    /// Filter the outgoing deck away.
    FilterOut,
    /// Hard cut on a phrase boundary.
    Cut,
    /// Loop the outgoing deck while the new one comes in.
    LoopOut,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum DeckAction {
    Play,
    Pause,
    Sync,
    Loop {
        beats: f32,
    },
    LoopExit,
    Jump {
        beats: f32,
    },
    HotCue {
        n: u8,
    },
    /// Load a library track (by path) onto the deck.
    Load {
        path: String,
    },
    /// Put the deck's playhead at `seconds` (its mix-in point).
    Cue {
        seconds: f64,
    },
    /// Transpose the deck (keylock on: tempo unchanged), in semitones.
    Key {
        semitones: i32,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum MixerAction {
    /// Crossfader to `to` (0 = A … 1 = B) over `bars`.
    Crossfader { to: f32, bars: f32 },
    /// Channel fader to `to` (0…1) over `bars`.
    Fader { to: f32, bars: f32 },
    /// EQ knob to `to` (−1 kill … 0 … +1) over `bars`.
    Eq { band: Band, to: f32, bars: f32 },
    /// Filter knob to `to` (−1 … +1) over `bars`.
    Filter { to: f32, bars: f32 },
}

/// Beat FX on a deck (the engine's `fx`): switch an effect on with its
/// length and depth, or off (echo and reverb tails ring out).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum EffectAction {
    On {
        kind: crate::command::FxKind,
        beats: f32,
        depth: f32,
    },
    Off,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "unit", rename_all = "snake_case")]
pub enum Action {
    Stem {
        deck: Deck,
        stem: String,
        to: f32,
        bars: f32,
    },
    Deck {
        deck: Deck,
        action: DeckAction,
    },
    Mixer {
        deck: Option<Deck>,
        action: MixerAction,
    },
    Effect {
        deck: Deck,
        action: EffectAction,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    /// Bars after the plan's start (fractions allowed: 0.5 = two beats).
    pub at: f32,
    pub action: Action,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransitionPlan {
    pub from: Deck,
    pub to: Deck,
    /// Bar of the outgoing deck where the plan starts (its bar count from
    /// the track's first downbeat).
    pub start_bar: f64,
    pub bars: f32,
    pub style: TransitionStyle,
    pub steps: Vec<Step>,
    /// Uncalibrated evidence support index (0…1), plus rationale. It is
    /// shown to the DJ and never acted on as a probability.
    #[serde(default)]
    pub confidence: f32,
    #[serde(default)]
    pub rationale: String,
    /// The technique ("bass swap", "echo out", "loop roll", …).
    #[serde(default)]
    pub technique: String,
}

/// What the AI DJ visualization (and any other frontend) sees: real system
/// state only — beat and bar phase from the engine clock, structure and
/// energy from analysis, compatibility, the plan and its evidence support.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AiView {
    pub decks: Vec<DeckView>,
    /// 0…1: how well the two decks fit (key on the Camelot wheel, tempo).
    pub compatibility: f32,
    pub plan: Option<PlanView>,
    /// −1…1: energy trend of the playing mix (rising = building tension).
    pub tension: f32,
    /// The AI DJ's state (idle, listening, selecting, suggested, prepared,
    /// mixing, manualoverride) and its current suggestion.
    #[serde(default)]
    pub ai_state: String,
    #[serde(default)]
    pub suggestion: Option<SuggestionView>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SuggestionView {
    pub title: String,
    pub artist: String,
    pub style: String,
    pub score: f32,
    /// Uncalibrated evidence support index (0…1), not a probability.
    pub confidence: f32,
    pub reasons: Vec<String>,
    /// Seconds until the transition would start.
    pub starts_in: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeckView {
    pub deck: String,
    pub playing: bool,
    /// Beats since the grid's first downbeat (fractional), as heard.
    pub beat: Option<f64>,
    /// 0…1 through the bar and through the 8-bar phrase.
    pub bar_phase: Option<f32>,
    pub phrase_phase: Option<f32>,
    pub bpm: Option<f64>,
    /// Camelot key, as shifted.
    pub key: Option<String>,
    /// Energy now and over the next 16 s (0…1 per second).
    pub energy: f32,
    pub energy_ahead: Vec<f32>,
    /// Seconds left in the track.
    pub remaining: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanView {
    pub style: String,
    /// 0…1 of the plan's steps done.
    pub progress: f32,
    pub steps_left: usize,
    /// Uncalibrated evidence support index (0…1), not a probability.
    pub confidence: f32,
    pub rationale: String,
}

/// A command due at a beat offset from the plan's start.
#[derive(Clone, Debug, PartialEq)]
pub struct Timed {
    pub beat: f64,
    pub command: Command,
    /// Visual segment ending at this beat: (duration in beats, starting value).
    pub visual_ramp: Option<(f64, f32)>,
}

/// Quarter-beat resolution for ramps.
const RAMP_STEP_BEATS: f64 = 0.25;

fn ramp(
    at_beat: f64,
    over_beats: f64,
    from: f32,
    to: f32,
    mut make: impl FnMut(f32) -> Command,
) -> Vec<Timed> {
    if over_beats <= 0.0 {
        return vec![Timed {
            beat: at_beat,
            command: make(to),
            visual_ramp: None,
        }];
    }
    let n = (over_beats / RAMP_STEP_BEATS).ceil().max(1.0) as usize;
    (1..=n)
        .map(|i| {
            let f = i as f32 / n as f32;
            Timed {
                beat: at_beat + i as f64 * RAMP_STEP_BEATS,
                command: make(from + (to - from) * f),
                visual_ramp: Some((
                    RAMP_STEP_BEATS,
                    from + (to - from) * (i - 1) as f32 / n as f32,
                )),
            }
        })
        .collect()
}

/// Mixer state a plan's ramps start from.
#[derive(Clone, Copy, Debug, Default)]
pub struct MixerNow {
    pub crossfader: f32,
    /// Channel fader per deck (A, B).
    pub fader: [f32; 2],
    /// EQ knobs per deck: [high, mid, low, filter].
    pub eq: [[f32; 4]; 2],
}

/// Compile and validate a plan into a timeline of commands for `origin`
/// at `level`. Any step outside the origin's limits refuses the whole plan.
pub fn compile(
    plan: &TransitionPlan,
    origin: Origin,
    level: ControlLevel,
    now: MixerNow,
) -> Result<Vec<Timed>, Rejected> {
    if level > ControlLevel::ceiling(origin) {
        return Err(Rejected(format!(
            "{origin:?} may go up to {:?}, not {level:?}",
            ControlLevel::ceiling(origin)
        )));
    }
    if level < ControlLevel::Prepare {
        return Ok(vec![]); // observe / suggest: nothing runs
    }
    if plan.from == plan.to {
        return Err(Rejected("a transition needs two different decks".into()));
    }
    if plan.from.index() > 1 || plan.to.index() > 1 {
        return Err(Rejected(
            "the legacy crossfader transition plan targets Decks A/B; use an arrangement graph for other lanes".into(),
        ));
    }
    if !(plan.bars > 0.0 && plan.bars <= 128.0) {
        return Err(Rejected("transitions are 0 … 128 bars".into()));
    }
    let idx = |d: Deck| d.index();
    let mut out = vec![];
    let mut xf = now.crossfader;
    let mut fader = now.fader;
    let mut eq = now.eq;
    let mut steps = plan.steps.clone();
    steps.sort_by(|a, b| a.at.total_cmp(&b.at));
    for s in &steps {
        if s.at < 0.0 || s.at > plan.bars {
            return Err(Rejected(format!(
                "step at bar {} is outside the transition",
                s.at
            )));
        }
        let beat = s.at as f64 * 4.0;
        match &s.action {
            Action::Stem {
                deck,
                stem,
                to,
                bars,
            } => {
                if !bars.is_finite() || *bars < 0.0 || s.at + bars > plan.bars {
                    return Err(Rejected("Stem ramp exceeds transition".into()));
                }
                if level == ControlLevel::Execute {
                    out.push(Timed {
                        beat,
                        command: Command::StemRamp {
                            deck: *deck,
                            stem: stem.clone(),
                            to: *to,
                            beats: *bars as f64 * 4.0,
                        },
                        visual_ramp: None,
                    });
                }
            }
            Action::Deck { deck, action } => {
                // Preparing may cue and sync the incoming deck; only
                // executing may start/stop or loop anything.
                let prep = matches!(
                    action,
                    DeckAction::Sync
                        | DeckAction::HotCue { .. }
                        | DeckAction::Jump { .. }
                        | DeckAction::Load { .. }
                        | DeckAction::Cue { .. }
                        | DeckAction::Key { .. }
                ) && *deck == plan.to;
                if level < ControlLevel::Execute && !prep {
                    continue;
                }
                let cmd = match action {
                    DeckAction::Play => Command::DeckTransport {
                        deck: *deck,
                        action: Transport::Play,
                    },
                    DeckAction::Pause => Command::DeckTransport {
                        deck: *deck,
                        action: Transport::Pause,
                    },
                    DeckAction::Sync => Command::DeckSync { deck: *deck },
                    DeckAction::Key { semitones } => Command::DeckKey {
                        deck: *deck,
                        semitones: *semitones,
                    },
                    DeckAction::Loop { beats } => Command::DeckLoop {
                        deck: *deck,
                        beats: Some(*beats),
                    },
                    DeckAction::LoopExit => Command::DeckLoop {
                        deck: *deck,
                        beats: None,
                    },
                    DeckAction::Jump { beats } => Command::DeckJump {
                        deck: *deck,
                        beats: *beats,
                    },
                    DeckAction::HotCue { n } => Command::HotCue {
                        deck: *deck,
                        n: *n,
                        clear: false,
                    },
                    DeckAction::Load { path } => Command::DeckLoadPath {
                        deck: *deck,
                        path: path.clone(),
                    },
                    DeckAction::Cue { seconds } => Command::DeckSeek {
                        deck: *deck,
                        seconds: *seconds,
                    },
                };
                out.push(Timed {
                    beat,
                    command: cmd,
                    visual_ramp: None,
                });
            }
            Action::Mixer { deck, action } => {
                if level < ControlLevel::Execute {
                    continue; // preparing never mixes
                }
                match action {
                    MixerAction::Crossfader { to, bars } => {
                        out.extend(ramp(beat, *bars as f64 * 4.0, xf, *to, |v| {
                            Command::Crossfader { position: v }
                        }));
                        xf = *to;
                    }
                    MixerAction::Fader { to, bars } => {
                        let d = deck.ok_or_else(|| Rejected("a fader needs a deck".into()))?;
                        let i = idx(d);
                        out.extend(ramp(beat, *bars as f64 * 4.0, fader[i], *to, |v| {
                            Command::DeckFader {
                                deck: d,
                                position: v,
                            }
                        }));
                        fader[i] = *to;
                    }
                    MixerAction::Eq { band, to, bars } => {
                        let d = deck.ok_or_else(|| Rejected("an EQ move needs a deck".into()))?;
                        let (i, slot) = (
                            idx(d),
                            match band {
                                Band::High => 0,
                                Band::Mid => 1,
                                Band::Low => 2,
                            },
                        );
                        let b = *band;
                        out.extend(ramp(beat, *bars as f64 * 4.0, eq[i][slot], *to, |v| {
                            Command::DeckEqKnob {
                                deck: d,
                                band: b,
                                value: v,
                            }
                        }));
                        eq[i][slot] = *to;
                    }
                    MixerAction::Filter { to, bars } => {
                        let d =
                            deck.ok_or_else(|| Rejected("a filter move needs a deck".into()))?;
                        let i = idx(d);
                        out.extend(ramp(beat, *bars as f64 * 4.0, eq[i][3], *to, |v| {
                            Command::DeckFilter { deck: d, amount: v }
                        }));
                        eq[i][3] = *to;
                    }
                }
            }
            Action::Effect { deck, action } => out.push(Timed {
                beat,
                visual_ramp: None,
                command: match action {
                    EffectAction::On { kind, beats, depth } => Command::DeckFx {
                        deck: *deck,
                        kind: Some(*kind),
                        on: Some(true),
                        beats: Some(*beats),
                        depth: Some(*depth),
                    },
                    EffectAction::Off => Command::DeckFx {
                        deck: *deck,
                        kind: None,
                        on: Some(false),
                        beats: None,
                        depth: None,
                    },
                },
            }),
        }
    }
    // Every command within the origin's limits, with ramps judged step by
    // step (their per-step change is what the limits bound).
    let mut ctx = Context {
        crossfader: Some(now.crossfader),
        ..Default::default()
    };
    for t in &out {
        command::validate(t.command.clone(), origin, &ctx)?;
        if let Command::Crossfader { position } = t.command {
            ctx.crossfader = Some(position);
        }
    }
    out.sort_by(|a, b| a.beat.total_cmp(&b.beat));
    Ok(out)
}

/// A 16-bar blend from `from` to `to` as a human might prepare it: sync
/// and play the incoming deck, swap the lows at the halfway phrase, ride
/// the crossfader over the last 8 bars.
pub fn blend(from: Deck, to: Deck, start_bar: f64) -> TransitionPlan {
    let target = if to == Deck::B { 1.0 } else { 0.0 };
    TransitionPlan {
        from,
        to,
        start_bar,
        bars: 16.0,
        style: TransitionStyle::Blend,
        steps: vec![
            Step {
                at: 0.0,
                action: Action::Deck {
                    deck: to,
                    action: DeckAction::Sync,
                },
            },
            Step {
                at: 0.0,
                action: Action::Mixer {
                    deck: Some(to),
                    action: MixerAction::Eq {
                        band: Band::Low,
                        to: -1.0,
                        bars: 0.0,
                    },
                },
            },
            Step {
                at: 0.0,
                action: Action::Deck {
                    deck: to,
                    action: DeckAction::Play,
                },
            },
            Step {
                at: 8.0,
                action: Action::Mixer {
                    deck: Some(from),
                    action: MixerAction::Eq {
                        band: Band::Low,
                        to: -1.0,
                        bars: 0.5,
                    },
                },
            },
            Step {
                at: 8.0,
                action: Action::Mixer {
                    deck: Some(to),
                    action: MixerAction::Eq {
                        band: Band::Low,
                        to: 0.0,
                        bars: 0.5,
                    },
                },
            },
            Step {
                at: 8.0,
                action: Action::Mixer {
                    deck: None,
                    action: MixerAction::Crossfader {
                        to: target,
                        bars: 8.0,
                    },
                },
            },
            Step {
                at: 16.0,
                action: Action::Deck {
                    deck: from,
                    action: DeckAction::Pause,
                },
            },
        ],
        confidence: 1.0,
        rationale: "16-bar blend: lows swapped on the phrase".into(),
        technique: "bass swap".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_human_blend_compiles_to_a_timeline() {
        let plan = blend(Deck::A, Deck::B, 64.0);
        let now = MixerNow {
            crossfader: 0.0,
            fader: [0.909, 0.909],
            ..Default::default()
        };
        let t = compile(&plan, Origin::User, ControlLevel::Execute, now).unwrap();
        assert!(
            t.windows(2).all(|w| w[0].beat <= w[1].beat),
            "in time order"
        );
        assert_eq!(t.first().map(|x| x.beat), Some(0.0));
        assert!(t.iter().any(|x| x.command
            == Command::DeckTransport {
                deck: Deck::B,
                action: Transport::Play
            }));
        let xf: Vec<f32> = t
            .iter()
            .filter_map(|x| {
                if let Command::Crossfader { position } = x.command {
                    Some(position)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(xf.len(), 128, "8 bars = 32 beats of quarter-beat steps");
        let mut previous = 0.0;
        for step in t
            .iter()
            .filter(|t| matches!(t.command, Command::Crossfader { .. }))
        {
            assert_eq!(step.visual_ramp, Some((0.25, previous)));
            if let Command::Crossfader { position } = step.command {
                previous = position;
            }
        }
        assert_eq!(*xf.last().unwrap(), 1.0);
        assert_eq!(t.last().unwrap().beat, 64.0, "ends 16 bars in");
    }

    #[test]
    fn legacy_crossfader_plans_cannot_address_arrangement_lanes() {
        let plan = blend(Deck::A, Deck::C, 0.0);
        let error = compile(
            &plan,
            Origin::User,
            ControlLevel::Execute,
            MixerNow::default(),
        )
        .unwrap_err();
        assert!(error.0.contains("use an arrangement graph"));
    }

    #[test]
    fn the_ai_cannot_execute() {
        let plan = blend(Deck::A, Deck::B, 0.0);
        let r = compile(
            &plan,
            Origin::Ai,
            ControlLevel::Execute,
            MixerNow::default(),
        );
        assert!(r.is_err());
    }

    #[test]
    fn preparing_only_cues_and_syncs_the_incoming_deck() {
        let plan = blend(Deck::A, Deck::B, 0.0);
        let t = compile(
            &plan,
            Origin::Ai,
            ControlLevel::Prepare,
            MixerNow::default(),
        )
        .unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].command, Command::DeckSync { deck: Deck::B });
    }

    #[test]
    fn command_limits_hold_inside_plans() {
        // Every compiled command is validated: a human's instant crossfader
        // slam is allowed; an EQ boost beyond +6 dB is not, for anyone.
        let mut plan = blend(Deck::A, Deck::B, 0.0);
        plan.steps = vec![Step {
            at: 0.0,
            action: Action::Mixer {
                deck: None,
                action: MixerAction::Crossfader { to: 1.0, bars: 0.0 },
            },
        }];
        assert!(
            compile(
                &plan,
                Origin::User,
                ControlLevel::Execute,
                MixerNow::default()
            )
            .is_ok()
        );
        plan.steps = vec![Step {
            at: 0.0,
            action: Action::Mixer {
                deck: Some(Deck::A),
                action: MixerAction::Eq {
                    band: Band::High,
                    to: 1.5,
                    bars: 0.0,
                },
            },
        }];
        assert!(
            compile(
                &plan,
                Origin::User,
                ControlLevel::Execute,
                MixerNow::default()
            )
            .is_err()
        );
    }

    #[test]
    fn effects_compile_for_people_and_are_refused_for_the_ai() {
        let mut plan = blend(Deck::A, Deck::B, 0.0);
        plan.steps.push(Step {
            at: 1.0,
            action: Action::Effect {
                deck: Deck::A,
                action: EffectAction::On {
                    kind: crate::command::FxKind::Echo,
                    beats: 0.5,
                    depth: 0.6,
                },
            },
        });
        let t = compile(
            &plan,
            Origin::User,
            ControlLevel::Execute,
            MixerNow::default(),
        )
        .unwrap();
        assert!(
            t.iter()
                .any(|x| matches!(x.command, Command::DeckFx { on: Some(true), .. }))
        );
        assert!(
            compile(
                &plan,
                Origin::Ai,
                ControlLevel::Execute,
                MixerNow::default()
            )
            .is_err()
        );
    }

    #[test]
    fn plans_are_json() {
        let plan = blend(Deck::B, Deck::A, 32.0);
        let j = serde_json::to_string(&plan).unwrap();
        assert_eq!(serde_json::from_str::<TransitionPlan>(&j).unwrap(), plan);
    }
}
