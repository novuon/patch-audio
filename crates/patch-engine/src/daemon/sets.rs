//! Not available in this build.
use super::*;

pub(super) fn set_json(_set: &crate::session::Set) -> Value {
    Value::Null
}

pub(super) fn workspace_session(
    _s: &Shared,
    _request: patch_core::dj_workspace::SessionAction,
) -> Result<Value, String> {
    Err(patch_ai::NOT_IN_THIS_BUILD.to_string())
}

pub(super) fn set_start(_s: &Shared, _req: &Value) -> Result<Value, String> {
    Err(patch_ai::NOT_IN_THIS_BUILD.to_string())
}

pub(super) fn say(_s: &Shared, _text: &str) -> Result<Value, String> {
    Err(patch_ai::NOT_IN_THIS_BUILD.to_string())
}
