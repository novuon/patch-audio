//! Not available in this build.
use super::*;

#[allow(clippy::type_complexity)]
pub(super) fn evaluate(
    _s: &Shared,
) -> Result<(usize, Vec<patch_ai::Suggestion>, Vec<patch_ai::TrackInfo>, Vec<String>), String> {
    Err(patch_ai::NOT_IN_THIS_BUILD.to_string())
}
