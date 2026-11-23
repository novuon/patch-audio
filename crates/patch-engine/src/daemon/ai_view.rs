//! Not available in this build.
use super::*;

pub(super) fn automix_json(_s: &Shared) -> Value {
    Value::Null
}

pub(super) fn ai_state(_s: &Shared) -> patch_core::aivis::VisualState {
    Default::default()
}

#[derive(Default)]
#[allow(dead_code)]
pub(super) struct AutoState {
    pub(super) prepared: Option<(usize, String)>,
    pub(super) next: Option<Value>,
    pub(super) pending: Option<patch_core::intent::TransitionPlan>,
    pub(super) tempo_settle: bool,
    pub(super) tempo_glide: Option<(usize, f64, f64)>,
}

#[derive(Clone, Default)]
#[allow(dead_code)]
pub(super) struct Eval {
    pub(super) candidates: Vec<patch_ai::Suggestion>,
    pub(super) locked: Option<String>,
    pub(super) rejected: Vec<String>,
    pub(super) human_at: Option<std::time::Instant>,
}
