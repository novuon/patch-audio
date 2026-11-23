//! Settings rows for module keys: none in this build.

/// A module's line in Settings › Modules while its key is missing.
pub(super) fn locked_detail(_coming_soon: bool) -> String {
    "Needs a key".into()
}

pub(super) fn rows() -> Vec<patch_host::SettingRow> {
    Vec::new()
}

pub(super) fn action(_action: &str) {}
