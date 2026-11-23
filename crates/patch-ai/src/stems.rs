//! Stem transitions (the template names).
use patch_core::{intent::TransitionPlan, stems::StemContext};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Template {
    BassSwap,
    DrumIntroduction,
    VocalIsolation,
    VocalHandoff,
    AcapellaOverInstrumental,
    GradualReplacement,
    PhraseRemoval,
}

/// Compose a stem transition: not available in this build.
pub fn apply(
    _plan: &mut TransitionPlan,
    _template: Template,
    _from: &StemContext,
    _to: &StemContext,
) -> Result<(), String> {
    Err(crate::NOT_IN_THIS_BUILD.into())
}
