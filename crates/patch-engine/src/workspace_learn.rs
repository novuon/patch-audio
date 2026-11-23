//! Not available in this build.
use super::*;

pub(super) fn process(_job: &mut Value, _cancel: &AtomicBool) -> Result<(), String> {
    Err(patch_ai::NOT_IN_THIS_BUILD.to_string())
}
