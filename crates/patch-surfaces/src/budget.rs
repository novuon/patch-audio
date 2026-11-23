//! One drawing budget for all of Patch's windows, instead of each window
//! assuming the machine is its own.
//!
//! * The governor: once a second the UI thread's own CPU time (everything
//!   it updates and draws, in every window). Over 85 % of a core for 2 s:
//!   everything that moves steps down together, from every display frame
//!   to every 2nd, then every 3rd (counted frames, so motion stays even: a
//!   time-based 30 fps cap on a 60 Hz tick made the waveforms judder).
//!   Back up when the faster step would stay under 70 % for 5 s. Input never waits on it: keys,
//!   drags and clicks are handled as they come.
//! * The tier: what this machine draws like, from a short benchmark of
//!   Patch's heaviest drawing (`omarchy-patch optimize`, and at install),
//!   kept in `~/.config/omarchy-patch/render-tier`. It sets the step a
//!   session starts at, so a slow machine doesn't stutter first.
//! * PATCH_RENDER_LEVEL=0|1|2 fixes the step (no governor).

use std::cell::Cell;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    High,
    Medium,
    Low,
}

impl Tier {
    pub fn name(self) -> &'static str {
        match self {
            Tier::High => "high",
            Tier::Medium => "medium",
            Tier::Low => "low",
        }
    }

    fn from_name(s: &str) -> Option<Tier> {
        [Tier::High, Tier::Medium, Tier::Low]
            .into_iter()
            .find(|t| t.name() == s.trim())
    }

    /// The step a session starts at.
    pub fn start_level(self) -> u8 {
        match self {
            Tier::High | Tier::Medium => 0,
            Tier::Low => 1,
        }
    }
}

pub fn path() -> std::path::PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("omarchy-patch/render-tier")
}

pub fn load() -> Option<Tier> {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|s| Tier::from_name(&s))
}

pub fn save(t: Tier) -> std::io::Result<()> {
    let p = path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(p, format!("{}\n", t.name()))
}

/// Patch's heaviest per-frame drawing, on one thread: a deck's scrolling
/// waveform strip (2400 × 112) and a Visualiser frame drawn on the CPU
/// (1280 × 720). Milliseconds for both (the median of 7), and the tier.
pub fn benchmark() -> (f64, Tier) {
    use crate::scope;
    let detail = crate::dj::demo_detail(0);
    let c = slint::Color::from_rgb_u8;
    let colors = scope::Colors {
        bg: c(10, 10, 10),
        low: c(80, 80, 80),
        mid: c(140, 140, 140),
        high: c(230, 230, 230),
        beat: c(60, 60, 60),
        bar: c(120, 120, 120),
        accent: c(230, 60, 90),
        clip: c(220, 80, 80),
    };
    let mut grid = patch_visual::grid::Grid::blank(160, 45);
    for (i, cell) in grid.cells.iter_mut().enumerate() {
        cell.ch = ['░', '▒', '▓', '█', 'a', 'Z'][i % 6];
        cell.fg = Some([200, (i % 255) as u8, 120]);
    }
    let mut glyphs = patch_visual::render::Glyphs::blocks();
    let look = patch_visual::render::Look {
        bg: [0, 0, 0],
        text: [220, 220, 220],
        pulse: 0.5,
        glow: 0.6,
        bump: 0.0,
        split: 2.0,
        shake: 0.0,
    };
    let mut times = vec![];
    for k in 0..7 {
        let t0 = Instant::now();
        let view = scope::View {
            detail: &detail,
            at: 30.0 + k as f64,
            span: 16.0,
            grid: Some((126.0, 0.1, Some(0))),
            cues: [None; crate::dj::HOT_CUES],
            looping: None,
            loop_on: false,
            playhead: false,
            style: scope::Style::Classic,
        };
        std::hint::black_box(scope::render(2400, 112, &view, &colors));
        std::hint::black_box(patch_visual::render::draw(
            &grid,
            1280,
            720,
            &mut glyphs,
            &look,
        ));
        times.push(t0.elapsed().as_secs_f64() * 1e3);
    }
    times.sort_by(f64::total_cmp);
    let ms = times[times.len() / 2];
    // The dev machine (a desktop CPU): 6.8 ms. A laptop on battery is
    // slower; the thresholds leave a frame (16.7 ms) its headroom.
    let tier = if ms < 12.0 {
        Tier::High
    } else if ms < 30.0 {
        Tier::Medium
    } else {
        Tier::Low
    };
    (ms, tier)
}

/// The governor (see the module docs).
pub struct Budget {
    level: Cell<u8>,
    over: Cell<u8>,
    under: Cell<u8>,
    /// The UI thread's CPU time (ticks) when last read, and when.
    last: Cell<Option<(Instant, u64)>>,
    busy: Cell<f32>,
    /// PATCH_RENDER_LEVEL=0|1|2: that step, and no governor.
    fixed: bool,
}

impl Budget {
    pub fn new(start: u8) -> Budget {
        let fixed = std::env::var("PATCH_RENDER_LEVEL")
            .ok()
            .and_then(|v| v.parse::<u8>().ok());
        Budget {
            level: Cell::new(fixed.unwrap_or(start).min(2)),
            over: Cell::new(0),
            under: Cell::new(0),
            last: Cell::new(None),
            busy: Cell::new(0.0),
            fixed: fixed.is_some(),
        }
    }

    pub fn level(&self) -> u8 {
        self.level.get()
    }

    /// The UI thread's busy share of one core in the last second.
    pub fn busy(&self) -> f32 {
        self.busy.get()
    }

    /// Draw what moves on this frame (every 1st, 2nd or 3rd).
    pub fn animate(&self, frame: u64) -> bool {
        frame.is_multiple_of(self.level.get() as u64 + 1)
    }

    /// Once a second, from the UI thread.
    pub fn second(&self) {
        let Some(ticks) = thread_ticks() else { return };
        let now = Instant::now();
        if let Some((at, before)) = self.last.replace(Some((now, ticks))) {
            let secs = now.duration_since(at).as_secs_f64().max(0.001);
            let hz = 100.0; // USER_HZ
            let busy = (ticks.saturating_sub(before) as f64 / hz / secs) as f32;
            self.judge(busy);
        }
    }

    /// One second's verdict on `busy` (share of a core). Down a step only
    /// when frames are at risk: 85 % of a core for 2 s. A 60 % trigger
    /// threw away frames that still fit (10 ms of a 16.7 ms frame), and
    /// the waveforms juddered through every transition with the decks in
    /// view (2026-11-12: 58–67 % watching two decks, 82–91 % blending).
    /// Back up when the step above would fit too: drawing every frame
    /// costs about `level + 1` times what every (level+1)th frame does, so
    /// the busy share scaled up stays under 70 % for 5 s. Stepping up on a
    /// low reading alone would come straight back down: a stutter every
    /// few seconds.
    fn judge(&self, busy: f32) {
        self.busy.set(busy);
        if self.fixed {
            return;
        }
        let l = self.level.get();
        let above = busy * (l as f32 + 1.0) / l.max(1) as f32;
        if busy > 0.85 {
            self.under.set(0);
            self.over.set(self.over.get() + 1);
            if self.over.get() >= 2 && l < 2 {
                self.level.set(l + 1);
                self.over.set(0);
            }
        } else if l > 0 && above < 0.7 {
            self.over.set(0);
            self.under.set(self.under.get() + 1);
            if self.under.get() >= 5 {
                self.level.set(l - 1);
                self.under.set(0);
            }
        } else {
            self.over.set(0);
            self.under.set(0);
        }
    }
}

/// This thread's CPU time (user + system), in clock ticks.
fn thread_ticks() -> Option<u64> {
    let st = std::fs::read_to_string("/proc/thread-self/stat").ok()?;
    let f: Vec<&str> = st[st.rfind(')')? + 2..].split_whitespace().collect();
    Some(f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_down_under_load_and_back_up_when_calm() {
        let b = Budget::new(0);
        assert!(b.animate(1) && b.animate(2));
        b.level.set(1);
        assert!(
            !b.animate(1) && b.animate(2) && !b.animate(3) && b.animate(4),
            "every 2nd"
        );
        b.level.set(2);
        assert!(
            b.animate(3) && !b.animate(4) && !b.animate(5) && b.animate(6),
            "every 3rd"
        );
        assert_eq!(Tier::from_name("low\n"), Some(Tier::Low));
    }

    /// Two decks in view (about 60 % of a core) keep every frame; only a
    /// thread near full steps down, and it comes back only when the
    /// faster step would fit, so it never see-saws.
    #[test]
    fn steps_down_only_when_frames_are_at_risk_and_never_see_saws() {
        let b = Budget::new(0);
        for _ in 0..10 {
            b.judge(0.67);
        }
        assert_eq!(b.level(), 0, "60–67 % fits a 60 Hz frame");
        b.judge(0.9);
        b.judge(0.9);
        assert_eq!(b.level(), 1, "two seconds near full: every 2nd frame");
        for _ in 0..10 {
            b.judge(0.45);
        }
        assert_eq!(b.level(), 1, "45 % at every 2nd frame is 90 % at every frame");
        for _ in 0..5 {
            b.judge(0.3);
        }
        assert_eq!(b.level(), 0, "30 % at every 2nd frame fits every frame");
        assert_eq!(Tier::Low.start_level(), 1);
    }
}
