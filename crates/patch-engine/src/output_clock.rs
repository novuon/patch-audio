//! The output keeps the graph's clock.
//!
//! PipeWire lets the linked device with the highest `priority.driver` drive
//! the graph, and WirePlumber ranks inputs above outputs (a USB microphone
//! 2109, its own output 1109). So when an application opened a microphone
//! (a browser on Patch Input opens the microphones), the microphone took the
//! clock: the output turned follower and was rate-matched to it, which
//! crackles for a moment, and back again when it closed (2026-11-22).
//!
//! Patch is a console: its output is the clock. One WirePlumber rule puts
//! every ALSA input below every output (HDMI ranks lowest, 696). An input
//! still drives when nothing plays; with an output running it follows.
//! The engine checks the rule at every start and writes it only when it is
//! missing or differs. It takes effect when WirePlumber next starts (at the
//! next login: the installer asks for one anyway). Never restarted from
//! here: a WirePlumber restart under a running engine flickers the default
//! output, which the engine follows as the user's choice in the bar (seen
//! 2026-11-22: the output moved to another device).

use std::path::{Path, PathBuf};

pub const FILE: &str = "60-patch-output-clock.conf";

pub const RULE: &str = r#"# Written by Patch (omarchy-patch engine); rewritten if changed.
# The output keeps the graph's clock: an ALSA input (a microphone) that
# opens follows the output instead of taking over, so no output is
# rate-matched mid-play (a crackle every time a microphone opened).
monitor.alsa.rules = [
  {
    matches = [ { node.name = "~alsa_input.*" } ]
    actions = { update-props = { priority.driver = 500 } }
  }
]
"#;

fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("wireplumber/wireplumber.conf.d"))
}

/// Whether the rule had to be (re)written in `dir`.
fn write_rule(dir: &Path) -> std::io::Result<bool> {
    let path = dir.join(FILE);
    if std::fs::read_to_string(&path).is_ok_and(|now| now == RULE) {
        return Ok(false);
    }
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{FILE}.tmp"));
    std::fs::write(&tmp, RULE)?;
    std::fs::rename(&tmp, &path)?;
    Ok(true)
}

/// At engine start: the rule in place (for WirePlumber's next start).
pub fn ensure() {
    let Some(dir) = dir() else { return };
    match write_rule(&dir) {
        Ok(false) => {}
        Ok(true) => eprintln!("patch: output clock rule written ({FILE}); active from the next login"),
        Err(e) => eprintln!("patch: output clock rule: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_once_then_left_alone() {
        let dir = std::env::temp_dir().join(format!("patch-output-clock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(write_rule(&dir).unwrap());
        assert!(!write_rule(&dir).unwrap(), "an unchanged rule is not rewritten");
        std::fs::write(dir.join(FILE), "old").unwrap();
        assert!(write_rule(&dir).unwrap(), "a changed rule is put back");
        assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), RULE);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn inputs_rank_below_every_output() {
        // WirePlumber's lowest output (HDMI) is 696; ALSA outputs ~1000+.
        assert!(RULE.contains("~alsa_input.*") && RULE.contains("priority.driver = 500"));
    }
}
