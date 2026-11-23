//! Pattern → Visualiser: the service's timeline of visual events, placed by
//! the graph clock it was stamped with, so a pattern's visual pulse lands on
//! its audio onset.
//!
//! The Visualiser polls `{"op":"pattern","action":"timeline","visual":true,
//! "since":…}` a few times a second; replies carry the graph sample `now`
//! and events with their own `sample`. Between polls the feed carries the
//! clock on by wall time, and each frame applies every event whose sample
//! has come (`pattern:<param>` signals, and scene changes). Each applied
//! event's lateness (frame time − onset) is recorded: the sync error.

use crate::generative::Signals;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
struct Pending {
    sample: i64,
    /// `None`: a scene event.
    param: Option<String>,
    value: Value,
}

#[derive(Clone, Debug, Default)]
pub struct Sync {
    pub applied: u64,
    pub mean_ms: f64,
    pub max_ms: f64,
    /// Events that arrived after their onset had passed (a slow poll).
    pub late_arrivals: u64,
}

#[derive(Default)]
pub struct Feed {
    pending: Vec<Pending>,
    /// The latest event end seen: the next poll's `since`.
    pub since: Option<i64>,
    /// (graph sample, wall seconds) of the last reply, and the rate.
    anchor: Option<(i64, f64, u32)>,
    pub sync: Sync,
    /// A scene a pattern asked for, not yet taken.
    pub scene: Option<String>,
}

impl Feed {
    /// A timeline reply, received at wall time `wall_s`.
    pub fn ingest(&mut self, reply: &Value, wall_s: f64) {
        let Some(now) = reply["now"].as_i64() else { return };
        let rate = reply["clock"]["rate"].as_u64().unwrap_or(48_000) as u32;
        self.anchor = Some((now, wall_s, rate.max(1)));
        for t in reply["events"].as_array().into_iter().flatten() {
            let (Some(sample), Some(end)) = (t["sample"].as_i64(), t["end"].as_i64()) else { continue };
            if self.since.is_some_and(|s| end <= s) {
                continue;
            }
            let target = &t["event"]["target"];
            let param = match target["to"].as_str() {
                Some("visual") => Some(target["param"].as_str().unwrap_or("").to_string()),
                Some("scene") => None,
                _ => continue,
            };
            if sample < now {
                self.sync.late_arrivals += 1;
            }
            let p = Pending { sample, param, value: t["event"]["value"].clone() };
            if !self.pending.contains(&p) {
                self.pending.push(p);
            }
        }
        if let Some(end) = reply["events"].as_array().and_then(|e| e.iter().filter_map(|t| t["end"].as_i64()).max()) {
            self.since = Some(self.since.map_or(end, |s| s.max(end)));
        }
        self.pending.sort_by_key(|p| p.sample);
    }

    /// The graph sample at wall time `wall_s`, carried on from the last reply.
    pub fn sample_at(&self, wall_s: f64) -> Option<i64> {
        let (s, w, rate) = self.anchor?;
        Some(s + ((wall_s - w) * rate as f64).round() as i64)
    }

    /// Apply what is due at wall time `wall_s` (a frame) to the signals.
    pub fn apply(&mut self, wall_s: f64, signals: &mut Signals) {
        let Some(now) = self.sample_at(wall_s) else { return };
        let rate = self.anchor.map_or(48_000, |a| a.2) as f64;
        let due = self.pending.partition_point(|p| p.sample <= now);
        for p in self.pending.drain(..due) {
            let late_ms = (now - p.sample) as f64 * 1000.0 / rate;
            let s = &mut self.sync;
            s.applied += 1;
            s.mean_ms += (late_ms - s.mean_ms) / s.applied as f64;
            s.max_ms = s.max_ms.max(late_ms);
            match p.param {
                Some(param) => {
                    let v = p.value.as_f64().or_else(|| p.value.as_str().and_then(|x| x.parse().ok()));
                    if let Some(v) = v {
                        signals.set(&format!("pattern:{param}"), v as f32);
                    }
                }
                None => self.scene = p.value.as_str().map(str::to_string),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reply(now: i64, events: &[(i64, &str, f64)]) -> Value {
        json!({
            "now": now,
            "clock": {"rate": 48_000},
            "events": events.iter().map(|(s, p, v)| json!({
                "sample": s, "end": s + 6_000, "cycle": "0",
                "event": {"layer": 0, "whole": ["0", "1"], "target": {"to": "visual", "param": p}, "value": v}
            })).collect::<Vec<_>>()
        })
    }

    /// A 60 fps Visualiser polling every 250 ms, a pattern with an onset
    /// every 12 000 samples (a 16th at 120 BPM): every pulse lands within
    /// one frame of its audio onset, and never early.
    #[test]
    fn visual_pulses_land_within_a_frame_of_their_onset() {
        let mut f = Feed::default();
        let mut s = Signals::default();
        let mut wall = 0.0;
        let frame = 1.0 / 60.0;
        for poll in 0..20 {
            let now = (poll as f64 * 0.25 * 48_000.0) as i64;
            // The service has prepared ~100 ms ahead.
            let onsets: Vec<(i64, &str, f64)> = (0..200)
                .map(|k| k * 12_000)
                .filter(|&o| o >= now - 12_000 && o < now + 4_800)
                .map(|o| (o, "intensity", (o / 12_000 % 4) as f64 / 4.0))
                .collect();
            f.ingest(&reply(now, &onsets), wall);
            for _ in 0..15 {
                f.apply(wall, &mut s);
                wall += frame;
            }
        }
        assert!(f.sync.applied >= 18, "{:?}", f.sync);
        assert!(f.sync.max_ms <= frame * 1000.0 + 0.5, "{:?}", f.sync);
        assert!(f.sync.mean_ms >= 0.0);
        // Duplicates across overlapping polls apply once.
        assert_eq!(f.pending.len(), 0);
    }

    #[test]
    fn scenes_and_word_values() {
        let mut f = Feed::default();
        let mut s = Signals::default();
        let r = json!({"now": 1000, "clock": {"rate": 48_000}, "events": [
            {"sample": 900, "end": 2000, "event": {"target": {"to": "scene"}, "value": "cells"}},
            {"sample": 950, "end": 2000, "event": {"target": {"to": "visual", "param": "hue"}, "value": "0.25"}},
            {"sample": 950, "end": 2000, "event": {"target": {"to": "deck", "deck": "a"}, "value": 1}}
        ]});
        f.ingest(&r, 0.0);
        assert_eq!(f.sync.late_arrivals, 2);
        f.apply(0.0, &mut s);
        assert_eq!(f.scene.as_deref(), Some("cells"));
        assert_eq!(s.get("pattern:hue"), 0.25);
        assert_eq!(f.since, Some(2000));
    }
}
