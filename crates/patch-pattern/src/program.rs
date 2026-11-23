//! A program: named layers, each a pattern with a target. What the Pattern
//! Unit runs, saves and hands to the AI director.

use crate::ast::{Pat, Value, compile};
use crate::event::{Event, Target};
use crate::pattern::Pattern;
use crate::time::{Span, Time};
use serde::{Deserialize, Serialize};

pub const MAX_LAYERS: usize = 64;
pub const MAX_NODES: usize = 8192;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub name: String,
    pub target: Target,
    pub pat: Pat,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub muted: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Program {
    /// Beats in one cycle (4: a cycle is a 4/4 bar).
    #[serde(default = "four")]
    pub cycle_beats: Time,
    /// The seed of every random choice.
    #[serde(default)]
    pub seed: u64,
    pub layers: Vec<Layer>,
}

fn four() -> Time {
    Time::whole(4)
}

impl Default for Program {
    fn default() -> Program {
        Program {
            cycle_beats: four(),
            seed: 0,
            layers: Vec::new(),
        }
    }
}

impl Program {
    /// Limits a program from any author (an AI, the socket) must keep.
    pub fn validate(&self) -> Result<(), String> {
        if self.layers.len() > MAX_LAYERS {
            return Err(format!("at most {MAX_LAYERS} layers"));
        }
        if self.cycle_beats <= Time::ZERO || self.cycle_beats > Time::whole(64) {
            return Err("a cycle is more than 0 and at most 64 beats".into());
        }
        let nodes: usize = self.layers.iter().map(|l| l.pat.size()).sum();
        if nodes > MAX_NODES {
            return Err(format!("too large ({nodes} nodes, at most {MAX_NODES})"));
        }
        for (i, l) in self.layers.iter().enumerate() {
            if self.layers[..i].iter().any(|o| o.name == l.name) {
                return Err(format!("two layers are named '{}'", l.name));
            }
            if let Target::Sample { source } = &l.target {
                let ok = match source {
                    patch_core::material::SampleSource::Asset(a) => a.valid(),
                    patch_core::material::SampleSource::Kit(k) => k.valid(),
                };
                if !ok {
                    return Err(format!("{}: not a derived-material id", l.name));
                }
            }
        }
        Ok(())
    }

    pub fn layer(&self, name: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.name == name)
    }

    pub fn compile(&self) -> Compiled {
        Compiled {
            layers: self
                .layers
                .iter()
                .map(|l| (l.target.clone(), l.muted, compile(&l.pat)))
                .collect(),
            seed: self.seed,
            cycle_beats: self.cycle_beats,
        }
    }
}

/// A program ready to query (the patterns built once, queried many times).
#[derive(Clone)]
pub struct Compiled {
    layers: Vec<(Target, bool, Pattern<Value>)>,
    pub seed: u64,
    pub cycle_beats: Time,
}

impl Compiled {
    /// Every event starting in `span`, in time order (then layer order),
    /// from the layers `keep` accepts.
    pub fn events_where(&self, span: Span, keep: impl Fn(&Target) -> bool) -> Vec<Event> {
        let mut out = Vec::new();
        for (i, (target, muted, p)) in self.layers.iter().enumerate() {
            if *muted || !keep(target) {
                continue;
            }
            for h in p.onsets(span, self.seed) {
                let Some(whole) = h.whole else { continue };
                out.push(Event {
                    layer: i as u16,
                    whole,
                    target: target.clone(),
                    value: h.value,
                    params: Default::default(),
                    loc: None,
            locs: Vec::new(),
                });
            }
        }
        out.sort_by(|a, b| a.onset().cmp(&b.onset()).then(a.layer.cmp(&b.layer)));
        out
    }

    pub fn events(&self, span: Span) -> Vec<Event> {
        self.events_where(span, |_| true)
    }

    /// A layer's value at time `t`: the event sounding then (the latest to
    /// start, for overlaps), or the signal's value. What a continuous
    /// consumer (the Visualiser) reads each frame.
    pub fn value_at(&self, layer: usize, t: Time) -> Option<Value> {
        let (_, muted, p) = self.layers.get(layer)?;
        if *muted {
            return None;
        }
        let tiny = Time::new(1, 1 << 20);
        p.query_span(Span::new(t, t + tiny), self.seed)
            .into_iter()
            .max_by(|a, b| a.onset().cmp(&b.onset()))
            .map(|h| h.value)
    }

    /// The most recent onset of a layer at or before `t` (within `back`
    /// cycles): for flashes and envelopes that follow events.
    pub fn last_onset(&self, layer: usize, t: Time, back: Time) -> Option<(Time, Value)> {
        let (_, muted, p) = self.layers.get(layer)?;
        if *muted {
            return None;
        }
        let tiny = Time::new(1, 1 << 20);
        p.onsets(Span::new(t - back, t + tiny), self.seed)
            .into_iter()
            .filter(|h| h.onset() <= t)
            .max_by(|a, b| a.onset().cmp(&b.onset()))
            .map(|h| (h.onset(), h.value))
    }

    pub fn targets(&self) -> impl Iterator<Item = &Target> {
        self.layers.iter().map(|l| &l.0)
    }
}
