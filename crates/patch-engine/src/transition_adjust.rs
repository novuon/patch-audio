//! Not available in this build.
use super::*;

pub(super) fn status() -> Value {
    Value::Null
}

pub(super) fn adjust(_s: &Shared, _direction: &str) -> Result<Value, String> {
    Err(patch_ai::NOT_IN_THIS_BUILD.to_string())
}
