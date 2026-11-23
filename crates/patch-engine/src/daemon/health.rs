//! Buffer health for the bar: once a second the engine's counters become a
//! state (green, amber, red) and a reason, in `health.json` in the runtime
//! dir. The bar's dot reads that file; nothing is spawned or asked for.
//!
//! The dot shows only a problem the user could hear, and one that lasts
//! (the user, 2026-11-03: yellow "all the time" looks broken). Red: a skip
//! in the last 15 s. Amber: a pattern — [`AMBER_SKIPS`] skips in the last
//! 5 minutes, or the busiest cycle over 80 % of its time in [`BUSY_SECONDS`]
//! of the last 15 s. One skip is red for 15 s and then gone; one busy spike
//! (a track loading) shows nothing. A skip is a second in which, while
//! Patch was sounding (its output or headphones), the device ran dry (an
//! xrun PipeWire reports, or the graph woke over two cycles late) or
//! Patch's processing missed its cycle, or a playing deck ran out of audio.
//! Skips in silence can't be heard: they stay in `engine status` only.

use super::*;
use std::collections::VecDeque;

const RED_FOR: Duration = Duration::from_secs(15);
const AMBER_FOR: Duration = Duration::from_secs(300);
const NEAR: f64 = 0.8;
/// Skips in 5 minutes that make a pattern (amber once their red is over).
const AMBER_SKIPS: usize = 3;
/// Seconds of the last 15 with a cycle near its limit that make amber.
const BUSY_SECONDS: usize = 3;

/// What went wrong in one second (several can, the first named wins).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Second {
    /// The device ran dry: xruns and late wake-ups.
    pub dry: u64,
    /// Patch's callback took longer than its cycle.
    pub late: u64,
    /// A playing deck ran out of audio (deck letters, bit per deck).
    pub starved: u32,
    /// The busiest callback, as a share of its cycle.
    pub peak: f64,
}

impl Second {
    fn skipped(&self) -> bool {
        self.dry + self.late > 0 || self.starved != 0
    }
}

#[derive(Default)]
pub(super) struct Watch {
    seconds: VecDeque<(Instant, Second)>,
}

#[derive(Debug, PartialEq)]
pub(super) struct Health {
    pub state: &'static str,
    pub reason: String,
}

fn cause(s: &Second) -> String {
    if s.dry > 0 {
        "the audio device ran dry".into()
    } else if s.late > 0 {
        "Patch's processing ran late".into()
    } else {
        let decks: Vec<String> = (0..32)
            .filter(|i| s.starved & (1 << i) != 0)
            .map(|i| ((b'A' + i as u8) as char).to_string())
            .collect();
        format!("deck {} ran out of audio", decks.join(" and "))
    }
}

impl Watch {
    pub fn push(&mut self, at: Instant, s: Second) {
        self.seconds.push_back((at, s));
        while self
            .seconds
            .front()
            .is_some_and(|(t, _)| at.duration_since(*t) > AMBER_FOR)
        {
            self.seconds.pop_front();
        }
    }

    pub fn judge(&self, now: Instant) -> Health {
        let recent = |d: Duration| {
            self.seconds
                .iter()
                .filter(move |(t, _)| now.duration_since(*t) <= d)
                .map(|(_, s)| s)
        };
        let skips = |d| recent(d).filter(|s| s.skipped()).count();
        let last = self.seconds.iter().rev().map(|(_, s)| s).find(|s| s.skipped());
        let peak = recent(RED_FOR).map(|s| s.peak).fold(0.0, f64::max);
        let busy = recent(RED_FOR).filter(|s| s.peak > NEAR).count();
        let times = |n: usize| if n == 1 { "1 skip".to_string() } else { format!("{n} skips") };
        if let Some(last) = last {
            let red = skips(RED_FOR);
            if red > 0 {
                return Health { state: "red", reason: format!("{} just now: {}", times(red), cause(last)) };
            }
            let amber = skips(AMBER_FOR);
            if amber >= AMBER_SKIPS {
                return Health {
                    state: "amber",
                    reason: format!("{} in the last 5 minutes: {}", times(amber), cause(last)),
                };
            }
        }
        let pct = (peak * 100.0).round() as u32;
        if busy >= BUSY_SECONDS {
            Health { state: "amber", reason: format!("Close to the limit: the busiest cycle took {pct} % of its time") }
        } else if peak > 0.0 {
            Health { state: "green", reason: format!("Buffers healthy: the busiest cycle took {pct} % of its time") }
        } else {
            Health { state: "green", reason: "Buffers healthy: nothing plays".into() }
        }
    }
}

pub(super) fn health_path() -> std::path::PathBuf {
    patch_core::ipc::runtime_dir().join("health.json")
}

/// The watch: one look a second, the file rewritten each time (its `at`
/// tells the bar the engine is alive).
pub(super) fn health(s: Arc<Shared>) {
    let p = &s.params;
    let mut watch = Watch::default();
    let (mut dry, mut late) = (0, 0);
    let mut starved = vec![0u64; p.decks.len()];
    let path = health_path();
    let tmp = path.with_extension("json.tmp");
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let now_dry = p.heard_dry.load(Ordering::Relaxed);
        let now_late = p.heard_late.load(Ordering::Relaxed);
        let mut second = Second {
            dry: now_dry - dry,
            late: now_late - late,
            ..Default::default()
        };
        (dry, late) = (now_dry, now_late);
        for (i, d) in p.decks.iter().enumerate() {
            let n = d.underruns_total.load(Ordering::Relaxed);
            if n > starved[i] {
                second.starved |= 1 << i;
            }
            starved[i] = n;
        }
        let (rate, frames) = (
            p.last_rate.load(Ordering::Relaxed),
            p.last_frames.load(Ordering::Relaxed),
        );
        let period = u64::from(frames) * 1_000_000_000 / u64::from(rate.max(1));
        let worst = p.cost_peak.swap(0, Ordering::Relaxed);
        if period > 0 {
            second.peak = worst as f64 / period as f64;
        }
        let at = Instant::now();
        watch.push(at, second);
        let h = watch.judge(at);
        let detail = (rate > 0).then(|| {
            format!("{frames} frames at {} kHz ({:.1} ms a cycle)", f64::from(rate) / 1000.0, period as f64 / 1e6)
        });
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let line = json!({
            "state": h.state,
            "reason": h.reason,
            "detail": detail,
            "at": unix,
            "skips": {"dry": dry, "late": late, "decks": starved.iter().sum::<u64>()},
        });
        if std::fs::write(&tmp, line.to_string() + "\n").is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
        if s.retired.load(Ordering::Acquire) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_skip_is_red_for_15_s_then_gone() {
        let t0 = Instant::now();
        let mut w = Watch::default();
        w.push(t0, Second { peak: 0.3, ..Default::default() });
        assert_eq!(w.judge(t0).state, "green");
        assert!(w.judge(t0).reason.contains("30 %"));
        w.push(t0 + Duration::from_secs(1), Second { dry: 3, late: 1, peak: 1.2, ..Default::default() });
        let h = w.judge(t0 + Duration::from_secs(1));
        assert_eq!(h.state, "red");
        assert_eq!(h.reason, "1 skip just now: the audio device ran dry");
        let later = t0 + Duration::from_secs(17);
        w.push(later, Second { peak: 0.2, ..Default::default() });
        assert_eq!(w.judge(later).state, "green");
    }

    #[test]
    fn repeated_skips_are_amber_until_five_quiet_minutes() {
        let t0 = Instant::now();
        let mut w = Watch::default();
        for i in 0..3 {
            w.push(t0 + Duration::from_secs(60 * i), Second { dry: 1, ..Default::default() });
        }
        let later = t0 + Duration::from_secs(150);
        w.push(later, Second { peak: 0.2, ..Default::default() });
        let h = w.judge(later);
        assert_eq!(h.state, "amber");
        assert_eq!(h.reason, "3 skips in the last 5 minutes: the audio device ran dry");
        let gone = t0 + Duration::from_secs(120 + 301);
        w.push(gone, Second { peak: 0.2, ..Default::default() });
        assert_eq!(w.judge(gone).state, "green");
    }

    #[test]
    fn a_busy_spike_is_nothing_a_busy_stretch_is_amber_and_a_starved_deck_is_named() {
        let t0 = Instant::now();
        let mut w = Watch::default();
        w.push(t0, Second { peak: 0.85, ..Default::default() });
        assert_eq!(w.judge(t0).state, "green");
        for i in 1..3 {
            w.push(t0 + Duration::from_secs(i), Second { peak: 0.9, ..Default::default() });
        }
        assert_eq!(w.judge(t0 + Duration::from_secs(2)).state, "amber");
        w.push(t0 + Duration::from_secs(2), Second { starved: 0b10, ..Default::default() });
        assert_eq!(w.judge(t0 + Duration::from_secs(2)).reason, "1 skip just now: deck B ran out of audio");
    }
}
