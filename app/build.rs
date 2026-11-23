//! Embeds the git commit into the binary (`PATCH_GIT`), handed to
//! `patch_core::set_git` at start. Here, not in patch-core: `.git/index`
//! changes with every `git status`, and a rerun in the base crate rebuilt
//! the whole workspace.

use std::process::Command;

fn main() {
    let describe = Command::new("git")
        .args(["describe", "--always", "--dirty", "--exclude", "*"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("cargo:rustc-env=PATCH_GIT={describe}");
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/index");
}
