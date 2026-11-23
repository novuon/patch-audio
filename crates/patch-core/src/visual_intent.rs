//! Visual intent: what the picture should *mean*, not how to draw it.
//!
//! The AI director (and patterns, and the Visualiser's own director) speak this
//! contract; the Visualiser interprets it — in choreography it picks
//! moods and scenes, in generative and hybrid modes it sets the generative
//! scene's parameters. No engine code or shader crosses it.
//!
//! The engine keeps the current intent on a [`Board`] (`op: visual_intent`);
//! writers send partial intents, which merge onto what is there.

use crate::command::Origin;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    #[default]
    Unknown,
    Intro,
    Verse,
    Build,
    Drop,
    Breakdown,
    Outro,
    /// Between two tracks.
    Transition,
}

/// How colour behaves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Colour {
    /// The Omarchy theme's palette.
    #[default]
    Theme,
    /// Hold the current hue.
    Hold,
    /// Drift slowly.
    Drift,
    /// Step on bars or phrases.
    Step,
    /// Flash with accents.
    Flash,
}

/// What the picture should follow most.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Focus {
    #[default]
    Whole,
    Kick,
    Bass,
    Drums,
    Vocals,
    Melody,
}

/// The semantic intent. Every amount is 0 … 1.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Intent {
    pub section: Section,
    /// How much is happening, and how bright.
    pub intensity: f32,
    /// How fast things move.
    pub motion: f32,
    /// 0 free … 1 strictly mirrored.
    pub symmetry: f32,
    /// Sparse … busy.
    pub density: f32,
    /// How much of the last frame stays (trails, smear).
    pub feedback: f32,
    pub colour: Colour,
    /// A hue to lean on (0 … 1 around the wheel), when not the theme's.
    pub hue: Option<f32>,
    /// Beats over which a change should land (0: cut).
    pub transition_beats: f32,
    pub focus: Focus,
}

impl Default for Intent {
    fn default() -> Intent {
        Intent {
            section: Section::Unknown,
            intensity: 0.5,
            motion: 0.5,
            symmetry: 0.5,
            density: 0.5,
            feedback: 0.3,
            colour: Colour::Theme,
            hue: None,
            transition_beats: 4.0,
            focus: Focus::Whole,
        }
    }
}

impl Intent {
    pub fn validate(&self) -> Result<(), String> {
        for (name, v) in [
            ("intensity", self.intensity),
            ("motion", self.motion),
            ("symmetry", self.symmetry),
            ("density", self.density),
            ("feedback", self.feedback),
        ] {
            if !(0.0..=1.0).contains(&v) {
                return Err(format!("{name} is 0 … 1"));
            }
        }
        if self.hue.is_some_and(|h| !(0.0..=1.0).contains(&h)) {
            return Err("hue is 0 … 1".into());
        }
        if !(0.0..=64.0).contains(&self.transition_beats) {
            return Err("transition_beats is 0 … 64".into());
        }
        Ok(())
    }

    /// `partial` (a JSON object of some fields) over this intent.
    pub fn merged(&self, partial: &Value) -> Result<Intent, String> {
        let Some(fields) = partial.as_object() else {
            return Err("an intent is an object of fields".into());
        };
        let mut v = serde_json::to_value(self).map_err(|e| e.to_string())?;
        for (k, x) in fields {
            if v.get(k).is_none() {
                return Err(format!("intent has no field '{k}'"));
            }
            v[k] = x.clone();
        }
        let next: Intent = serde_json::from_value(v).map_err(|e| e.to_string())?;
        next.validate()?;
        Ok(next)
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Posted {
    pub intent: Intent,
    /// Who wrote it last; `None` until someone does.
    pub origin: Option<Origin>,
    /// Wall clock, ms.
    pub updated_ms: u64,
    /// After this it no longer applies (the Visualiser's director leads
    /// again); `None`: until cleared.
    pub until_ms: Option<u64>,
    /// Bumped on every change (readers poll cheaply).
    pub version: u64,
}

impl Posted {
    /// Whether a writer's intent applies at `now_ms`.
    pub fn active(&self, now_ms: u64) -> bool {
        self.origin.is_some() && self.until_ms.is_none_or(|u| now_ms < u)
    }
}

/// The engine's current intent and the `visual_intent` op.
#[derive(Default)]
pub struct Board {
    posted: Mutex<Posted>,
}

impl Board {
    pub fn get(&self) -> Posted {
        self.posted.lock().unwrap().clone()
    }

    /// `{"action":"get"}`, `{"action":"set","intent":{…},"hold_s":32,
    /// "origin":"ai"}` (fields merge onto the current intent) or
    /// `{"action":"clear"}`.
    pub fn request(&self, req: &Value, now_ms: u64) -> Result<Value, String> {
        let mut p = self.posted.lock().unwrap();
        match req["action"].as_str().unwrap_or("get") {
            "get" => {}
            "set" => {
                let next = p.intent.merged(&req["intent"])?;
                let origin = serde_json::from_value(req["origin"].clone()).unwrap_or(Origin::User);
                let hold = req["hold_s"].as_f64().filter(|h| *h > 0.0);
                if hold.is_some_and(|h| h > 3600.0) {
                    return Err("hold_s is at most an hour".into());
                }
                p.intent = next;
                p.origin = Some(origin);
                p.updated_ms = now_ms;
                p.until_ms = hold.map(|h| now_ms + (h * 1000.0) as u64);
                p.version += 1;
            }
            "clear" => {
                let v = p.version + 1;
                *p = Posted { version: v, updated_ms: now_ms, ..Posted::default() };
            }
            other => return Err(format!("visual_intent has no action '{other}'")),
        }
        Ok(json!({"posted": *p, "active": p.active(now_ms)}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_intents_merge_and_bad_ones_change_nothing() {
        let b = Board::default();
        let r = b
            .request(&json!({"action":"set","intent":{"section":"drop","intensity":0.9},"origin":"ai","hold_s":16}), 1_000)
            .unwrap();
        assert_eq!(r["posted"]["intent"]["section"], "drop");
        assert_eq!(r["active"], true);
        let p = b.get();
        assert_eq!((p.intent.intensity, p.intent.motion, p.origin), (0.9, 0.5, Some(Origin::Ai)));
        assert!(p.active(16_999) && !p.active(17_000), "held 16 s");
        for bad in [json!({"intensity": 2.0}), json!({"sparkle": 1}), json!({"section": "chorus"}), json!(3)] {
            assert!(b.request(&json!({"action":"set","intent":bad}), 2_000).is_err(), "{bad}");
        }
        assert_eq!(b.get().version, 1, "rejected writes change nothing");
        b.request(&json!({"action":"clear"}), 3_000).unwrap();
        let p = b.get();
        assert!(!p.active(3_000));
        assert_eq!(p.intent, Intent::default());
        assert_eq!(p.version, 2);
    }
}
