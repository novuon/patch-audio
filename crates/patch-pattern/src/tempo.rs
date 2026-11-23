//! Cycles ↔ graph samples (ADR-005: all time comes from the graph clock).
//!
//! A tempo map is a list of anchors: "cycle `c` is at graph sample `s`,
//! from here on at `bpm`". A cycle's sample is computed from its anchor in
//! one step, exactly (rationals) and rounded once, so error never builds up
//! over a long set: cycle one million lands on the sample it should. A tempo
//! change or a re-alignment to a deck adds an anchor; it never rescales the
//! past.
//!
//! Query windows end on a grid of 1/4096 cycle (≈ 0.5 ms at 120 BPM in a
//! 4-beat cycle), which keeps every time in the scheduler small and exact.
//! Event onsets themselves are never snapped.

use crate::time::Time;
use serde::{Deserialize, Serialize};

pub const GRID: i64 = 1 << 12;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    pub cycle: Time,
    pub sample: i64,
    /// Beats per minute, exact (to a thousandth).
    pub bpm: Time,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TempoMap {
    pub rate: u32,
    pub cycle_beats: Time,
    anchors: Vec<Anchor>,
}

fn bpm_time(bpm: f64) -> Time {
    Time::approx(bpm.clamp(1.0, 999.0), 1000)
}

/// floor(n / d) for a rational, in i128.
fn floor_div(num: i128, den: i128) -> i128 {
    num.div_euclid(den)
}

impl TempoMap {
    pub fn new(rate: u32, cycle_beats: Time, bpm: f64, cycle: Time, sample: i64) -> TempoMap {
        TempoMap {
            rate: rate.max(1),
            cycle_beats: if cycle_beats > Time::ZERO {
                cycle_beats
            } else {
                Time::whole(4)
            },
            anchors: vec![Anchor {
                cycle: snap(cycle),
                sample,
                bpm: bpm_time(bpm),
            }],
        }
    }

    pub fn anchors(&self) -> &[Anchor] {
        &self.anchors
    }

    /// Samples per cycle at `bpm`.
    fn spc(&self, bpm: Time) -> Time {
        Time::whole(self.rate as i64 * 60) * self.cycle_beats / bpm
    }

    fn for_cycle(&self, c: Time) -> &Anchor {
        self.anchors
            .iter()
            .rev()
            .find(|a| a.cycle <= c)
            .unwrap_or(&self.anchors[0])
    }

    fn for_sample(&self, s: i64) -> &Anchor {
        self.anchors
            .iter()
            .rev()
            .find(|a| a.sample <= s)
            .unwrap_or(&self.anchors[0])
    }

    pub fn bpm_at(&self, c: Time) -> f64 {
        self.for_cycle(c).bpm.to_f64()
    }

    /// The graph sample where cycle position `c` falls (rounded once).
    pub fn sample_at(&self, c: Time) -> i64 {
        let a = self.for_cycle(c);
        let off = (c - a.cycle) * self.spc(a.bpm);
        let (n, d) = (off.num() as i128, off.den() as i128);
        a.sample + floor_div(2 * n + d, 2 * d) as i64
    }

    /// The cycle position at graph sample `s`, floored to the 1/4096 grid.
    pub fn cycle_at(&self, s: i64) -> Time {
        let a = self.for_sample(s);
        let spc = self.spc(a.bpm);
        let steps = floor_div(
            (s - a.sample) as i128 * GRID as i128 * spc.den() as i128,
            spc.num() as i128,
        );
        snap(a.cycle) + Time::new(steps as i64, GRID)
    }

    /// For display and the Visualiser: the position at `s`, unrounded.
    pub fn cycle_at_f64(&self, s: f64) -> f64 {
        let a = self.for_sample(s as i64);
        a.cycle.to_f64() + (s - a.sample as f64) / self.spc(a.bpm).to_f64()
    }

    /// A new tempo from cycle `at` on (`at` is snapped to the grid).
    pub fn set_bpm(&mut self, at: Time, bpm: f64) {
        let at = snap(at);
        let sample = self.sample_at(at);
        self.push(Anchor {
            cycle: at,
            sample,
            bpm: bpm_time(bpm),
        });
    }

    /// Re-anchor: cycle `cycle` is at sample `sample` from now on (a deck's
    /// grid moved, the transport jumped).
    pub fn align(&mut self, sample: i64, cycle: Time, bpm: f64) {
        self.push(Anchor {
            cycle: snap(cycle),
            sample,
            bpm: bpm_time(bpm),
        });
    }

    fn push(&mut self, a: Anchor) {
        self.anchors
            .retain(|x| x.cycle < a.cycle && x.sample < a.sample);
        self.anchors.push(a);
        if self.anchors.len() > 32 {
            self.anchors.drain(..self.anchors.len() - 32);
        }
    }
}

/// Floored onto the 1/4096-cycle grid.
pub fn snap(t: Time) -> Time {
    let n = floor_div(t.num() as i128 * GRID as i128, t.den() as i128);
    Time::new(n as i64, GRID)
}

/// Ceiled onto the grid.
pub fn snap_up(t: Time) -> Time {
    let s = snap(t);
    if s == t { s } else { s + Time::new(1, GRID) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_million_cycles_land_on_their_sample() {
        let m = TempoMap::new(48_000, Time::whole(4), 124.37, Time::ZERO, 1_000);
        let spc = 48_000.0 * 60.0 * 4.0 / 124.37;
        for c in [1i64, 7, 1_000, 123_457, 1_000_000] {
            let want = 1_000 + (c as f64 * spc).round() as i64;
            assert_eq!(m.sample_at(Time::whole(c)), want, "cycle {c}");
        }
        // A triplet in cycle 999 999.
        let t = Time::whole(999_999) + Time::new(1, 3);
        let want = 1_000 + ((999_999.0 + 1.0 / 3.0) * spc).round() as i64;
        assert!((m.sample_at(t) - want).abs() <= 1);
    }

    #[test]
    fn cycle_and_sample_invert_on_the_grid() {
        let m = TempoMap::new(44_100, Time::whole(4), 128.0, Time::whole(3), 500);
        for s in [500i64, 501, 12_345, 4_000_000] {
            let c = m.cycle_at(s);
            let back = m.sample_at(c);
            assert!(back <= s, "{s} → {c} → {back}");
            assert!(s - back <= 44_100 * 60 * 4 / 128 / GRID + 1);
        }
        assert_eq!(m.cycle_at(500), Time::whole(3));
    }

    #[test]
    fn tempo_changes_keep_the_past() {
        let mut m = TempoMap::new(48_000, Time::whole(4), 120.0, Time::ZERO, 0);
        assert_eq!(m.sample_at(Time::whole(2)), 192_000);
        m.set_bpm(Time::whole(2), 60.0);
        assert_eq!(
            m.sample_at(Time::whole(2)),
            192_000,
            "the change point stays"
        );
        assert_eq!(m.sample_at(Time::ONE), 96_000, "before it, unchanged");
        assert_eq!(m.sample_at(Time::whole(3)), 192_000 + 192_000);
        assert_eq!(m.bpm_at(Time::new(5, 2)), 60.0);
        // Re-aligning to a deck adds an anchor.
        m.align(400_000, Time::whole(3), 120.0);
        assert_eq!(m.sample_at(Time::whole(4)), 496_000);
    }
}
