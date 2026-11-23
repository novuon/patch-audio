//! Not available in this build.
use super::*;

pub(super) fn drop_plan(_s: &Shared, _why: &str) -> bool {
    false
}

pub(super) fn plan_json(_s: &Shared) -> Value {
    Value::Null
}

pub(super) fn planner(_s: Arc<Shared>) {}

pub(super) fn ai_exec(_s: &Shared, _c: patch_core::command::Command) -> Result<(), String> {
    Err(patch_ai::NOT_IN_THIS_BUILD.to_string())
}

#[allow(dead_code)]
pub(super) struct RunningPlan {
    pub(super) timeline: VecDeque<patch_core::intent::Timed>,
    pub(super) from: usize,
    pub(super) start_beat: f64,
    pub(super) spec: patch_core::intent::TransitionPlan,
}
