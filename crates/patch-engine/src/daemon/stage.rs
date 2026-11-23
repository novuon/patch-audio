//! Not available in this build.
use super::*;

pub(super) fn performance_stage(
    _params: Arc<Params>,
    _session: Arc<Mutex<crate::session::Session>>,
    _req: &Value,
) -> Result<Value, String> {
    Err(patch_ai::NOT_IN_THIS_BUILD.to_string())
}
