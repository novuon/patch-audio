//! Where the AI DJ takes the set's tempo: one setting, kept in the engine
//! session, shown in the DJ Booth and set from `omarchy-patch dj tempo`.
//!
//! Transitions never change: the incoming track still beat-matches the one
//! playing. The setting acts after each transition, when the new track
//! plays alone: its tempo glides (keylocked, over ~30 s, never during a
//! blend) to where the setting wants it, and the next pick favours tracks
//! that sit there without much stretching.
//!
//! Follow is the default and is what the AI did before this setting: each
//! track keeps the tempo it was mixed in at.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TempoMode {
    /// Each track stays at the tempo it was mixed in at.
    #[default]
    Follow,
    /// Around a set BPM.
    Hold,
    /// A little faster with every track.
    Faster,
    /// A little slower with every track.
    Slower,
    /// Each track returns to its own tempo once it plays alone.
    Original,
}

impl TempoMode {
    pub const ALL: [TempoMode; 5] = [
        TempoMode::Follow,
        TempoMode::Hold,
        TempoMode::Faster,
        TempoMode::Slower,
        TempoMode::Original,
    ];

    pub fn id(self) -> &'static str {
        match self {
            TempoMode::Follow => "follow",
            TempoMode::Hold => "hold",
            TempoMode::Faster => "faster",
            TempoMode::Slower => "slower",
            TempoMode::Original => "original",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TempoMode::Follow => "Follow",
            TempoMode::Hold => "Hold BPM",
            TempoMode::Faster => "Faster",
            TempoMode::Slower => "Slower",
            TempoMode::Original => "Original",
        }
    }

    pub fn parse(id: &str) -> Option<TempoMode> {
        TempoMode::ALL.into_iter().find(|m| m.id() == id)
    }
}

/// Faster / Slower: this much per track (1 %: about 1.2 BPM at 124).
pub const STEP: f64 = 0.01;
/// The most a track is sped up or slowed down to reach the set's tempo.
pub const MAX_STRETCH: f64 = 0.06;
/// Hold: the set BPM's range.
pub const MIN_BPM: f64 = 60.0;
pub const MAX_BPM: f64 = 200.0;
/// A tempo glide takes this long (at most); a small one is slower still, so
/// it never moves faster than `MIN_GLIDE_PER_S` would need.
pub const GLIDE_S: f64 = 30.0;
pub const MIN_GLIDE_PER_S: f64 = 0.0004;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TempoPlan {
    pub mode: TempoMode,
    /// Hold's BPM (kept when another mode is chosen, for coming back).
    pub bpm: f64,
}

impl Default for TempoPlan {
    fn default() -> Self {
        TempoPlan {
            mode: TempoMode::Follow,
            bpm: 124.0,
        }
    }
}

/// `bpm` at the octave (half, same, double) nearest `near`.
pub fn fold(bpm: f64, near: f64) -> f64 {
    [0.5, 1.0, 2.0]
        .into_iter()
        .map(|m| bpm * m)
        .min_by(|a, b| (a / near).ln().abs().total_cmp(&(b / near).ln().abs()))
        .unwrap_or(bpm)
}

impl TempoPlan {
    /// `{"mode": "hold", "bpm": 126}`, `{"bpm_step": 1}`: a bad request
    /// changes nothing.
    pub fn apply(&mut self, req: &Value) -> Result<(), String> {
        let mut next = *self;
        if let Some(m) = req.get("mode").and_then(Value::as_str) {
            next.mode = TempoMode::parse(m).ok_or_else(|| {
                format!("unknown tempo mode \"{m}\" (follow, hold, faster, slower, original)")
            })?;
        }
        if let Some(b) = req.get("bpm").filter(|v| !v.is_null()) {
            let b = b.as_f64().ok_or("bpm takes a number")?;
            if !(MIN_BPM..=MAX_BPM).contains(&b) {
                return Err(format!("the set BPM must be within {MIN_BPM:.0}–{MAX_BPM:.0}"));
            }
            next.bpm = b;
        }
        if let Some(step) = req.get("bpm_step").and_then(Value::as_f64) {
            next.bpm = (next.bpm + step).clamp(MIN_BPM, MAX_BPM);
            next.mode = TempoMode::Hold;
        }
        *self = next;
        Ok(())
    }

    /// The tempo (BPM) the track playing at `played` BPM settles at once it
    /// plays alone; None leaves it where it is.
    pub fn goal(&self, played: f64) -> Option<f64> {
        match self.mode {
            TempoMode::Follow | TempoMode::Original => None,
            TempoMode::Hold => Some(self.bpm),
            TempoMode::Faster => Some(played * (1.0 + STEP)),
            TempoMode::Slower => Some(played * (1.0 - STEP)),
        }
    }

    /// The deck tempo (a fraction: 0.02 = +2 %) for a track of `native` BPM
    /// playing at `played` BPM; None leaves it.
    pub fn settle(&self, played: f64, native: f64) -> Option<f64> {
        if native <= 0.0 || played <= 0.0 {
            return None;
        }
        if self.mode == TempoMode::Original {
            return Some(0.0);
        }
        let goal = self.goal(played)?;
        Some((goal / fold(native, goal) - 1.0).clamp(-MAX_STRETCH, MAX_STRETCH))
    }

    /// Whether a track of `native` BPM suits where the tempo goes next,
    /// while the set plays at `played` BPM. Picks prefer these when any
    /// exist.
    pub fn admits(&self, played: f64, native: f64) -> bool {
        let near = |goal: f64| (fold(native, goal) / goal - 1.0).abs() <= MAX_STRETCH;
        match self.mode {
            TempoMode::Follow | TempoMode::Original => true,
            TempoMode::Hold => near(self.bpm),
            // Not slower than now (a little under is fine: the glide
            // makes up for it), and still in reach.
            TempoMode::Faster => {
                let n = fold(native, played);
                n >= played * (1.0 - STEP) && near(played)
            }
            TempoMode::Slower => {
                let n = fold(native, played);
                n <= played * (1.0 + STEP) && near(played)
            }
        }
    }

    /// The glide's step per `dt` seconds from `from` to `to` (fractions).
    pub fn glide_step(from: f64, to: f64, dt: f64) -> f64 {
        ((to - from).abs() / GLIDE_S).max(MIN_GLIDE_PER_S) * dt
    }

    /// One line: what the setting does.
    pub fn summary(&self) -> String {
        match self.mode {
            TempoMode::Follow => "each track stays at the tempo it was mixed in at".into(),
            TempoMode::Hold => format!("around {:.0} BPM", self.bpm),
            TempoMode::Faster => format!("a little faster with every track (+{:.0} %)", STEP * 100.0),
            TempoMode::Slower => format!("a little slower with every track (−{:.0} %)", STEP * 100.0),
            TempoMode::Original => "each track returns to its own tempo once it plays alone".into(),
        }
    }

    pub fn json(&self) -> Value {
        json!({
            "mode": self.mode.id(),
            "bpm": self.bpm,
            "summary": self.summary(),
            "modes": TempoMode::ALL.iter().map(|m| json!({"id": m.id(), "label": m.label()})).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(mode: TempoMode, bpm: f64) -> TempoPlan {
        TempoPlan { mode, bpm }
    }

    #[test]
    fn follow_is_the_default_and_changes_nothing() {
        let p = TempoPlan::default();
        assert_eq!(p.mode, TempoMode::Follow);
        assert_eq!(p.settle(124.0, 120.0), None);
        assert!(p.admits(124.0, 140.0));
    }

    #[test]
    fn hold_settles_on_the_set_bpm_within_the_stretch() {
        let p = plan(TempoMode::Hold, 124.0);
        // 122 native → +1.64 %.
        let f = p.settle(126.0, 122.0).unwrap();
        assert!((122.0 * (1.0 + f) - 124.0).abs() < 1e-9);
        // Too far: only as far as the stretch allows.
        assert_eq!(p.settle(126.0, 100.0), Some(MAX_STRETCH));
        // Half time folds: a 62 BPM track sits at 124 unstretched.
        assert!(p.settle(124.0, 62.0).unwrap().abs() < 1e-9);
        assert!(p.admits(130.0, 120.0));
        assert!(!p.admits(124.0, 140.0));
    }

    #[test]
    fn faster_and_slower_step_one_percent_from_where_the_set_is() {
        let f = plan(TempoMode::Faster, 124.0).settle(125.0, 125.0).unwrap();
        assert!((f - STEP).abs() < 1e-9);
        let s = plan(TempoMode::Slower, 124.0).settle(125.0, 125.0).unwrap();
        assert!((s + STEP).abs() < 1e-9);
        assert!(plan(TempoMode::Faster, 0.0).admits(125.0, 127.0));
        assert!(!plan(TempoMode::Faster, 0.0).admits(125.0, 120.0));
        assert!(plan(TempoMode::Slower, 0.0).admits(125.0, 120.0));
        assert!(!plan(TempoMode::Slower, 0.0).admits(125.0, 129.0));
    }

    #[test]
    fn original_returns_every_track_to_its_own_tempo() {
        let p = plan(TempoMode::Original, 124.0);
        assert_eq!(p.settle(128.0, 124.0), Some(0.0));
    }

    #[test]
    fn requests_change_it_and_bad_ones_change_nothing() {
        let mut p = TempoPlan::default();
        p.apply(&json!({"mode": "hold", "bpm": 126})).unwrap();
        assert_eq!(p, plan(TempoMode::Hold, 126.0));
        p.apply(&json!({"mode": "faster"})).unwrap();
        assert_eq!(p, plan(TempoMode::Faster, 126.0));
        p.apply(&json!({"bpm_step": -1})).unwrap();
        assert_eq!(p, plan(TempoMode::Hold, 125.0));
        assert!(p.apply(&json!({"mode": "warp", "bpm": 130})).is_err());
        assert!(p.apply(&json!({"mode": "follow", "bpm": 20})).is_err());
        assert_eq!(p, plan(TempoMode::Hold, 125.0));
    }

    #[test]
    fn glides_take_at_most_thirty_seconds_and_crawl_when_small() {
        // 6 % over 30 s: 0.2 % a second.
        assert!((TempoPlan::glide_step(0.0, 0.06, 1.0) - 0.002).abs() < 1e-12);
        // 0.3 % moves at the floor: 7.5 s.
        assert_eq!(TempoPlan::glide_step(0.0, 0.003, 1.0), MIN_GLIDE_PER_S);
    }
}
