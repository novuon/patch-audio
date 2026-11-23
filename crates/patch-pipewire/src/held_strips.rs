//! A paused application keeps its Mixer strip, in its place.
//!
//! Chromium corks a paused output after ~10 s, closes it ~5 s later and
//! opens a new stream (a new identity) on play. The strip used to vanish
//! with the old stream and come back elsewhere in the row. Now a departed
//! stream's strip stays where it was, frozen at its last settings, for
//! [`HOLD`]; the next stream of the same application takes that place. A
//! strip whose application has quit goes at once.

use patch_model::audio::{Strip, StripControls};
use std::time::{Duration, Instant};

/// How long a departed stream's strip waits for its application to play again.
pub const HOLD: Duration = Duration::from_secs(60);

#[derive(Clone, Debug)]
struct Slot {
    node: &'static str,
    app: &'static str,
    process: String,
    strip: Strip,
    controls: Option<StripControls>,
    gone: Option<Instant>,
}

/// One application strip shown now.
pub struct Live {
    pub strip: Strip,
    /// The application (`contextual::application_base`).
    pub app: &'static str,
    pub process: String,
    pub controls: Option<StripControls>,
}

#[derive(Default)]
pub struct Held {
    slots: Vec<Slot>,
}

impl Held {
    /// Take the application strips shown now, in their order. Returns
    /// whether a strip arrived, departed, was handed over or dropped.
    pub fn update(&mut self, live: Vec<Live>, now: Instant, alive: impl Fn(&str) -> bool) -> bool {
        let mut changed = false;
        let mut matched = vec![false; self.slots.len()];
        let mut arrivals = Vec::new();
        for l in live {
            match self.slots.iter().position(|s| s.node == l.strip.node) {
                Some(i) => {
                    let s = &mut self.slots[i];
                    changed |= s.gone.take().is_some();
                    s.strip = l.strip;
                    s.controls = l.controls;
                    s.process = l.process;
                    matched[i] = true;
                }
                None => arrivals.push(l),
            }
        }
        for l in arrivals {
            // The same application's departed strip: the same process first,
            // then the one that went last.
            let pick = (0..self.slots.len())
                .filter(|i| !matched[*i] && self.slots[*i].app == l.app)
                .max_by_key(|i| {
                    let s = &self.slots[*i];
                    (s.process == l.process, s.gone.unwrap_or(now))
                });
            let slot = Slot {
                node: l.strip.node,
                app: l.app,
                process: l.process,
                strip: l.strip,
                controls: l.controls,
                gone: None,
            };
            match pick {
                Some(i) => {
                    self.slots[i] = slot;
                    matched[i] = true;
                }
                None => {
                    self.slots.push(slot);
                    matched.push(true);
                }
            }
            changed = true;
        }
        for (s, m) in self.slots.iter_mut().zip(&matched) {
            if !m && s.gone.is_none() {
                s.gone = Some(now);
                s.strip.solo = false;
                changed = true;
            }
        }
        let before = self.slots.len();
        self.slots.retain(|s| s.gone.is_none() || s.process.is_empty() || alive(&s.process));
        changed | (self.slots.len() != before) | self.expire(now)
    }

    /// Drop strips held for longer than [`HOLD`].
    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.slots.len();
        self.slots.retain(|s| s.gone.is_none_or(|gone| now.duration_since(gone) < HOLD));
        self.slots.len() != before
    }

    /// The application strips in their places: live ones as they are now,
    /// held ones frozen. Live strips newer than the last update go last.
    pub fn order(&self, live: Vec<Strip>) -> Vec<Strip> {
        let mut live: Vec<Option<Strip>> = live.into_iter().map(Some).collect();
        let mut out = Vec::with_capacity(self.slots.len() + live.len());
        for s in &self.slots {
            match live.iter_mut().find(|l| l.as_ref().is_some_and(|l| l.node == s.node)) {
                Some(l) => out.extend(l.take()),
                None if s.gone.is_some() => out.push(s.strip.clone()),
                None => {}
            }
        }
        out.extend(live.into_iter().flatten());
        out
    }

    /// A held strip's knobs as they were (they wait for the next stream).
    pub fn controls(&self, node: &str) -> Option<StripControls> {
        self.slots.iter().find(|s| s.node == node && s.gone.is_some()).and_then(|s| s.controls)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(node: &'static str, gain_db: f32) -> Strip {
        Strip { node, name: node, gain_db, headroom: true, mute: false, solo: false, master: false }
    }
    fn live(node: &'static str, app: &'static str, process: &str, gain_db: f32) -> Live {
        Live { strip: strip(node, gain_db), app, process: process.into(), controls: None }
    }
    fn nodes(v: &[Strip]) -> Vec<&'static str> {
        v.iter().map(|s| s.node).collect()
    }

    #[test]
    fn a_paused_browser_keeps_its_place_and_the_next_stream_takes_it() {
        let t = Instant::now();
        let mut h = Held::default();
        h.update(vec![live("yt-1", "chromium", "42", -6.0), live("spotify", "spotify", "7", 0.0)], t, |_| true);
        // Chromium closes the paused output: its strip stays, frozen.
        assert!(h.update(vec![live("spotify", "spotify", "7", 0.0)], t + Duration::from_secs(15), |_| true));
        let shown = h.order(vec![strip("spotify", 0.0)]);
        assert_eq!(nodes(&shown), ["yt-1", "spotify"]);
        assert_eq!(shown[0].gain_db, -6.0);
        // Play again: a new stream, in the old place.
        let at = t + Duration::from_secs(40);
        assert!(h.update(vec![live("spotify", "spotify", "7", 0.0), live("yt-2", "chromium", "42", -6.0)], at, |_| true));
        assert_eq!(nodes(&h.order(vec![strip("spotify", 0.0), strip("yt-2", -6.0)])), ["yt-2", "spotify"]);
    }

    #[test]
    fn a_held_strip_goes_after_the_hold_or_when_its_application_quits() {
        let t = Instant::now();
        let mut h = Held::default();
        h.update(vec![live("a", "chromium", "42", 0.0), live("b", "mpv", "9", 0.0)], t, |_| true);
        h.update(vec![], t, |_| true);
        assert_eq!(h.order(vec![]).len(), 2);
        assert!(!h.expire(t + HOLD - Duration::from_secs(1)));
        // mpv quit: its strip goes with the next look; Chromium's waits.
        assert!(h.update(vec![], t + Duration::from_secs(2), |pid| pid != "9"));
        assert_eq!(nodes(&h.order(vec![])), ["a"]);
        assert!(h.expire(t + HOLD));
        assert!(h.order(vec![]).is_empty());
    }

    #[test]
    fn another_application_never_takes_a_held_place() {
        let t = Instant::now();
        let mut h = Held::default();
        h.update(vec![live("a", "chromium", "42", 0.0)], t, |_| true);
        h.update(vec![live("m", "mpv", "9", 0.0)], t, |_| true);
        assert_eq!(nodes(&h.order(vec![strip("m", 0.0)])), ["a", "m"]);
    }

    #[test]
    fn two_tabs_each_keep_their_place() {
        let t = Instant::now();
        let mut h = Held::default();
        h.update(vec![live("x", "chromium", "42", -3.0), live("y", "chromium", "42", -9.0)], t, |_| true);
        // The first tab pauses and returns as a new stream: the second stays put.
        h.update(vec![live("y", "chromium", "42", -9.0)], t + Duration::from_secs(15), |_| true);
        h.update(vec![live("y", "chromium", "42", -9.0), live("x2", "chromium", "42", -3.0)], t + Duration::from_secs(20), |_| true);
        assert_eq!(nodes(&h.order(vec![strip("y", -9.0), strip("x2", -3.0)])), ["x2", "y"]);
    }

    #[test]
    fn a_new_stream_between_updates_shows_at_the_end() {
        let mut h = Held::default();
        h.update(vec![live("a", "chromium", "42", 0.0)], Instant::now(), |_| true);
        assert_eq!(nodes(&h.order(vec![strip("b", 0.0), strip("a", 0.0)])), ["a", "b"]);
    }
}
