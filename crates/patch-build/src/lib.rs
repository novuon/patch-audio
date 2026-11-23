use std::collections::HashMap;
use std::path::PathBuf;

/// Directory of the shared design system (`@patch-ui/...` in .slint files).
pub fn design_system_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../patch-ui/ui")
}

/// Compile `entry` (relative to the calling crate) with `@patch-ui` available.
pub fn compile(entry: &str) {
    let dir = design_system_dir();
    println!("cargo:rerun-if-changed={}", dir.display());
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent-dark".into())
        .with_library_paths(HashMap::from([("patch-ui".to_string(), dir)]));
    slint_build::compile_with_config(entry, config).expect("slint compile failed");
}
