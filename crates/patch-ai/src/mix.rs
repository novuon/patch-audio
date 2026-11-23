//! Transitions: the shared types and plan helpers. This build composes no
//! transitions itself: every plan is None.
use crate::{Context, TrackInfo};
use patch_core::intent::{Action, DeckAction, EffectAction, MixerAction, TransitionPlan, TransitionStyle};

/// The planner's tunable choices: what `omarchy-patch dj train` adjusts by
/// rehearsing transitions (see the engine's rehearse.rs) and keeps in
/// ~/.config/omarchy-patch/ai-tuning.json. The defaults are the hand-made
/// choices.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Tuning {
    /// Pair mixability needed for 32, 24, 16 and 8-bar layers.
    pub cap_at: [f32; 4],
    /// Added to a cut's score (+: cuts more often, −: blends).
    pub cut_bias: f32,
    /// The incoming track's mids and highs (knobs) as a staggered blend
    /// begins.
    pub in_mid: f32,
    pub in_high: f32,
    /// The playing track's mids and highs while both play (0: untouched).
    pub space_mid: f32,
    pub space_high: f32,
    /// The incoming fader's rise, × the usual bars.
    pub fade_scale: f32,
    /// The playing track's mids after the bass swap (staggered blends).
    pub out_mid: f32,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            cap_at: [0.85, 0.75, 0.6, 0.45],
            cut_bias: 0.0,
            in_mid: -0.6,
            in_high: -0.35,
            space_mid: 0.0,
            space_high: 0.0,
            fade_scale: 1.0,
            out_mid: -0.5,
        }
    }
}

pub struct Mix {
    pub style: TransitionStyle,
    /// The transition as composed ("16-bar intro blend · bass swap ·
    /// highs, then mids · echo out").
    pub technique: String,
    pub mix_out: f64,
    pub mix_in: f64,
    /// Seconds on the outgoing track where it hands over (a phrase start).
    pub pivot: f64,
    pub plan: TransitionPlan,
    pub reasons: Vec<String>,
    /// Why this pair would gain from stems that aren't ready yet (None:
    /// it wouldn't, or they are). The engine prepares just these two
    /// tracks and plans again once they are.
    pub stems_wanted: Option<String>,
    /// Seconds of a layered blend in which both tracks sing (from their
    /// analysed vocals); None for a cut, or when either track's vocals
    /// aren't measured yet.
    pub vocals_together_s: Option<f64>,
}

/// A musical request, rather than a numeric transport edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adjustment {
    Sooner,
    Later,
    Longer,
    Shorter,
    Soonest,
    Latest,
    Longest,
    Shortest,
}

impl Adjustment {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "sooner" => Ok(Self::Sooner),
            "later" => Ok(Self::Later),
            "longer" => Ok(Self::Longer),
            "shorter" => Ok(Self::Shorter),
            "soonest" => Ok(Self::Soonest),
            "latest" => Ok(Self::Latest),
            "longest" => Ok(Self::Longest),
            "shortest" => Ok(Self::Shortest),
            _ => Err("direction must be sooner, later, longer or shorter".into()),
        }
    }

    pub fn satisfied(self, original: &TransitionPlan, revised: &TransitionPlan) -> bool {
        match self {
            Self::Sooner | Self::Soonest => revised.start_bar < original.start_bar - 0.001,
            Self::Later | Self::Latest => revised.start_bar > original.start_bar + 0.001,
            Self::Longer | Self::Longest => revised.bars > original.bars + 0.001,
            Self::Shorter | Self::Shortest => revised.bars < original.bars - 0.001,
        }
    }
}

pub const STEM_LED: &str = "stem-led intro";
pub const PHASE_UNSURE: &str = "where the kick sits in the beat is unsure in one track";

/// The next transition: not in this build.
pub fn plan(_ctx: &Context, _next: &TrackInfo) -> Option<Mix> {
    None
}

/// The safest transition: not in this build.
pub fn conservative_plan(_ctx: &Context, _next: &TrackInfo) -> Option<Mix> {
    None
}

/// A crossfade to end on: not in this build.
pub fn end_crossfade(_ctx: &Context, _next: &TrackInfo, _bars: f32) -> Option<Mix> {
    None
}

/// Alternatives for a musical request: not in this build.
pub fn adjustment_candidates(
    _ctx: &Context,
    _next: &TrackInfo,
    _original: &TransitionPlan,
    _request: Adjustment,
) -> Result<Vec<Mix>, String> {
    Err(crate::NOT_IN_THIS_BUILD.into())
}

/// Why stems would help this pair: not in this build.
pub fn stem_unlock(_ctx: &Context, _next: &TrackInfo, _current: &TransitionPlan) -> Option<String> {
    None
}

/// Whether a precise grid is worth measuring: not in this build.
pub fn precision_worth(_ctx: &Context, _next: &TrackInfo) -> Option<String> {
    None
}

/// The plan without its stem moves, for a deck whose stems are not there
/// after all.
pub fn without_stems(plan: &mut TransitionPlan) {
    use patch_core::intent::Action;
    plan.steps.retain(|s| !matches!(s.action, Action::Stem { .. }));
    plan.technique = plan
        .technique
        .replace(" · stem-led intro", "")
        .replace(" + stem-aware deck blend", "")
        .replace(" + stem bass swap", "");
}

/// Source seconds advanced per second of synchronized playback.
/// The incoming deck follows the playing deck's BPM, so this is target/native.
pub fn synced_source_rate(target_bpm: f64, source_bpm: f64) -> Option<f64> {
    (target_bpm.is_finite() && source_bpm.is_finite() && target_bpm > 0.0 && source_bpm > 0.0)
        .then_some(target_bpm / source_bpm)
}

/// The actual handover, including a crossfader ramp's duration.
pub fn handover_bar(plan: &TransitionPlan) -> Option<f32> {
    let target = if plan.to == patch_core::command::Deck::B {
        1.0
    } else {
        0.0
    };
    plan.steps
        .iter()
        .filter_map(|s| match s.action {
            Action::Mixer {
                action: MixerAction::Crossfader { to, bars },
                ..
            } if (to - target).abs() < 0.01 => Some(s.at + bars),
            _ => None,
        })
        .max_by(f32::total_cmp)
}

/// Keep a scheduled handover on its original phrase when replacing a blend
/// with a shorter design. Only preparation stays at zero; playback waits.
pub fn retain_handover(plan: &mut TransitionPlan, original: &TransitionPlan) -> bool {
    let (Some(old), Some(new)) = (handover_bar(original), handover_bar(plan)) else {
        return false;
    };
    let delay = old - new;
    if delay < -0.001 || plan.bars + delay > 128.0 {
        return false;
    }
    let delay = delay.max(0.0);
    for step in &mut plan.steps {
        let preparation = matches!(step.action, Action::Deck { deck, action: DeckAction::Load { .. } | DeckAction::Cue { .. } | DeckAction::Sync | DeckAction::Key { .. } } if deck == plan.to);
        if !preparation {
            step.at += delay;
        }
    }
    plan.start_bar = original.start_bar;
    plan.bars += delay;
    true
}

/// Try a blend without decorative FX before giving up on the overlap.
pub fn dry_revision(plan: &TransitionPlan) -> Option<TransitionPlan> {
    if !plan.steps.iter().any(|s| {
        matches!(
            s.action,
            Action::Effect {
                action: EffectAction::On { .. },
                ..
            }
        )
    }) {
        return None;
    }
    let mut dry = plan.clone();
    dry.steps.retain(|s| {
        !matches!(
            s.action,
            Action::Effect {
                action: EffectAction::On { .. },
                ..
            }
        )
    });
    dry.technique = format!(
        "{} · dry revision",
        match plan.style {
            TransitionStyle::Blend => "EQ blend",
            TransitionStyle::Cut => "cut in",
            TransitionStyle::LoopOut => "loop out",
            TransitionStyle::FilterOut => "filter out",
        }
    );
    Some(dry)
}

// ── The planner's settings (kept, so a session's choices persist) ─────

static TUNING: std::sync::Mutex<Option<Tuning>> = std::sync::Mutex::new(None);

pub fn tuning_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("omarchy-patch").join("ai-tuning.json"))
}

/// The tuning in use (no planner here: the saved one or the defaults).
pub fn tuning() -> Tuning {
    if let Some(t) = TUNING.lock().unwrap().clone() {
        return t;
    }
    tuning_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Use `t` instead of the saved tuning, or the saved one again.
pub fn set_tuning(t: Option<Tuning>) {
    *TUNING.lock().unwrap() = t;
}

use patch_core::mix_style::MixStyle;
static STYLE: std::sync::RwLock<Option<MixStyle>> = std::sync::RwLock::new(None);

/// How adventurous the planner is (kept for the session's setting).
pub fn mix_style() -> MixStyle {
    STYLE.read().unwrap().unwrap_or_default()
}

pub fn set_mix_style(m: MixStyle) {
    *STYLE.write().unwrap() = Some(m);
}

/// How much the stem mix may trail the plain one and still be kept, in
/// rehearsal points: a stem mix keeps its place when it scores at least
/// plain + this. With stems preferred it may trail by 3 (in blind pairs
/// the ear preferred the stem mix the rehearsal scored a point lower);
/// otherwise it must beat the conventional mix by 2.
pub fn stem_margin() -> f64 {
    if mix_style().prefer_stems { -3.0 } else { 2.0 }
}

/// One report: how many days ago, its verdict and the transition's moves.
pub struct Reported {
    pub age_days: f64,
    pub why: String,
    pub technique: String,
}

/// Learning from reports: nothing to learn without the planner.
pub fn set_learned(_reports: &[Reported]) -> usize {
    0
}
