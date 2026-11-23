//! One continuous sample timeline for the engine, whichever device drives
//! the graph.
//!
//! PipeWire's `clock.position` is the *driver's* counter. When the driver
//! changes (a microphone with a higher `priority.driver` is linked in: a
//! browser opening Patch Input opens the microphones), the position jumps
//! to the new driver's count, and back when it closes. Everything timed on
//! it (the Tidal Unit's and the Pattern Unit's events, the sequencer, timed
//! mixer changes, deck graph stamps) then fell silent, waiting for a moment
//! far away, and on the way back fired every queued event at once: a loud
//! burst (2026-11-22, the input test page in Chromium).
//!
//! Here the position follows the driver's while it runs on, and across a
//! jump it continues from where the last cycle ended, plus the real time that
//! passed between them (a suspend or an xrun still moves time on), measured
//! with `clock.nsec`, which every driver takes from the same monotonic clock.

use crate::Clock;

#[derive(Default)]
pub struct Timeline {
    /// The last cycle: the driver's position after it, ours after it, and
    /// when it ended (ns).
    last: Option<(u64, u64, u64)>,
    /// Ours minus the driver's.
    offset: i64,
}

impl Timeline {
    /// The clock with the position on the continuous timeline.
    pub fn map(&mut self, clock: &Clock) -> Clock {
        let mut out = *clock;
        let rate = u64::from(clock.rate.max(1));
        let frames = u64::from(clock.frames);
        if let Some((raw_end, ours_end, end_ns)) = self.last
            && clock.position != raw_end
        {
            // Real time between the last cycle's end and this one's start;
            // under a cycle is the hand-over itself, not a gap.
            let gap_ns = clock.nsec.saturating_sub(end_ns);
            let gap = gap_ns.saturating_mul(rate) / 1_000_000_000;
            let gap = if gap < frames.max(1) { 0 } else { gap };
            self.offset = (ours_end + gap) as i64 - clock.position as i64;
        }
        out.position = (clock.position as i64 + self.offset).max(0) as u64;
        let end_ns = clock.nsec + frames * 1_000_000_000 / rate;
        self.last = Some((clock.position + frames, out.position + frames, end_ns));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(position: u64, nsec: u64) -> Clock {
        Clock { position, nsec, rate: 48_000, frames: 256, ..Default::default() }
    }
    const CYCLE_NS: u64 = 256 * 1_000_000_000 / 48_000;

    #[test]
    fn follows_the_driver_while_it_runs() {
        let mut t = Timeline::default();
        for i in 0..10u64 {
            assert_eq!(t.map(&clock(1_000 + i * 256, i * CYCLE_NS)).position, 1_000 + i * 256);
        }
    }

    #[test]
    fn a_new_driver_continues_the_timeline() {
        let mut t = Timeline::default();
        t.map(&clock(5_000_000, 0));
        t.map(&clock(5_000_256, CYCLE_NS));
        // A microphone takes over: its own count, the next cycle in time.
        assert_eq!(t.map(&clock(77, 2 * CYCLE_NS)).position, 5_000_512);
        assert_eq!(t.map(&clock(333, 3 * CYCLE_NS)).position, 5_000_768);
        // And hands back: still no jump, in either direction.
        assert_eq!(t.map(&clock(5_000_900, 4 * CYCLE_NS + 1_000)).position, 5_001_024);
    }

    #[test]
    fn real_time_between_cycles_still_counts() {
        let mut t = Timeline::default();
        t.map(&clock(10_000, 0));
        // Suspended for a second, back on another count.
        let p = t.map(&clock(20, CYCLE_NS + 1_000_000_000)).position;
        assert_eq!(p, 10_256 + 48_000);
    }
}
