//! The installation's kernel profile: which implementation each op uses on
//! this machine, chosen by `omarchy-patch optimize` from verified,
//! measured candidates. Local to the installation (never in `.patchdj`
//! data). Stored at `~/.config/omarchy-patch/kernels.json`.

use crate::abi::Op;
use crate::bench::{Env, Result_};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Choice {
    pub op: Op,
    pub selected: String,
    /// Candidates that passed verification, with their measurements.
    pub measured: Vec<Result_>,
    /// Candidates rejected by verification, with the reason.
    pub rejected: Vec<(String, String)>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Profile {
    pub patch_version: String,
    pub arch: String,
    pub features: Vec<crate::cpu::Feature>,
    pub env: Env,
    pub choices: Vec<Choice>,
}

impl Profile {
    pub fn selected(&self, op: Op) -> Option<&str> {
        self.choices
            .iter()
            .find(|c| c.op == op)
            .map(|c| c.selected.as_str())
    }
}

pub fn path() -> std::path::PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("omarchy-patch")
        .join("kernels.json")
}

/// The profile, if one was written for this architecture. A profile from
/// another machine (different features) is ignored: selection falls back.
pub fn load() -> Option<Profile> {
    let p: Profile = serde_json::from_str(&std::fs::read_to_string(path()).ok()?).ok()?;
    (p.arch == std::env::consts::ARCH).then_some(p)
}

pub fn save(p: &Profile) -> Result<(), String> {
    let file = path();
    if let Some(d) = file.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let tmp = file.with_extension("tmp");
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(p).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(tmp, file).map_err(|e| e.to_string())
}

/// Detect → list → verify → benchmark → select → (caller saves). A
/// candidate is selected only if it passed verification and is at least
/// `MIN_GAIN` faster than the reference over live-audio sizes; otherwise
/// the reference stays (the assembly acceptance gate).
pub fn optimize(rounds: usize, log: &mut dyn FnMut(&str)) -> Profile {
    let pinned = crate::bench::pin();
    let env = crate::bench::env(pinned);
    let cpu = crate::cpu::this();
    let mut choices = vec![];
    for op in Op::ALL {
        let mut measured = vec![];
        let mut rejected = vec![];
        for imp in crate::registry::candidates(op) {
            match crate::verify::verify(&imp, false) {
                Ok(_) => measured.push(crate::bench::measure(&imp, rounds)),
                Err(f) => {
                    log(&format!(
                        "{}: {} rejected: {} ({})",
                        op.name(),
                        imp.name,
                        f.detail,
                        f.case
                    ));
                    rejected.push((imp.name.to_string(), format!("{}: {}", f.case, f.detail)));
                }
            }
        }
        let reference = measured
            .iter()
            .find(|r| r.name == "reference")
            .map(|r| r.score());
        let best = measured
            .iter()
            .min_by(|a, b| a.score().total_cmp(&b.score()));
        let selected = match (best, reference) {
            (Some(b), Some(r)) if b.name != "reference" && r / b.score() >= MIN_GAIN => {
                b.name.clone()
            }
            _ => "reference".to_string(),
        };
        choices.push(Choice {
            op,
            selected,
            measured,
            rejected,
        });
    }
    Profile {
        patch_version: env!("CARGO_PKG_VERSION").into(),
        arch: std::env::consts::ARCH.into(),
        features: cpu.features,
        env,
        choices,
    }
}

/// An optimised kernel must beat the reference by this factor to be used.
pub const MIN_GAIN: f64 = 1.15;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_persists_and_selects() {
        let dir = std::env::temp_dir().join(format!("patch-kernels-{}", std::process::id()));
        // SAFETY: set before any other thread reads it, in this test only.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };
        let p = optimize(3, &mut |_| {});
        assert_eq!(p.choices.len(), Op::ALL.len());
        for c in &p.choices {
            assert!(
                c.measured.iter().any(|m| m.name == "reference"),
                "{}: the reference is measured",
                c.op.name()
            );
            assert!(
                c.measured.iter().any(|m| m.name == c.selected),
                "{}: the selection was verified and measured",
                c.op.name()
            );
        }
        save(&p).unwrap();
        let back = load().expect("saved profile loads");
        for op in Op::ALL {
            assert_eq!(back.selected(op), p.selected(op));
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
