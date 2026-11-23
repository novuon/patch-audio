//! Not available in this build.
use super::*;

pub(super) fn ai_log(_msg: &str) {}

pub(super) fn start_plan(
    _s: &Shared,
    _plan: &patch_core::intent::TransitionPlan,
    _level: patch_core::intent::ControlLevel,
    _origin: patch_core::command::Origin,
    _prepared: bool,
) -> Result<Value, String> {
    Err(patch_ai::NOT_IN_THIS_BUILD.to_string())
}

pub(super) fn transition_now(_s: &Shared) -> Result<Value, String> {
    Err(patch_ai::NOT_IN_THIS_BUILD.to_string())
}

pub(super) fn mix_now_pending(_s: &Shared) {}

pub(super) fn automix_tick(_s: &Shared) {}
