//! The generative engine's contract: a visual scene as Patch data.
//!
//! A [`VisualScene`] is a small graph of named operations (sources,
//! coordinate and colour transforms, combiners); each argument is a
//! constant or a [`Param::Signal`] bound to the [`Signals`] bus (beat,
//! energy, bands, pattern parameters, visual intent…). A [`VisualBackend`]
//! compiles a scene to one fragment shader plus the uniforms that carry its
//! signals; Patch renders it (feedback, compositing, timing are Patch's).
//! Today the backend is Hydra, out of process (`hydra.rs`); a native
//! compiler can replace it behind the same trait.
//!
//! Nothing here knows Hydra: op names are Patch's vocabulary (they happen
//! to match Hydra's today, which the adapter relies on and a native
//! backend would implement).

use patch_core::visual_intent::{Colour, Intent, Section};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// An argument: a constant, or a signal from the bus (`value * scale +
/// offset`, smoothed over `smooth` seconds).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Param {
    Const {
        #[serde(rename = "const")]
        value: f32,
    },
    Signal {
        signal: String,
        #[serde(default = "one")]
        scale: f32,
        #[serde(default)]
        offset: f32,
        #[serde(default)]
        smooth: f32,
    },
}

fn one() -> f32 {
    1.0
}

impl Param {
    pub fn c(value: f32) -> Param {
        Param::Const { value }
    }

    pub fn sig(name: &str, scale: f32, offset: f32) -> Param {
        Param::Signal { signal: name.into(), scale, offset, smooth: 0.0 }
    }

    pub fn smoothed(self, seconds: f32) -> Param {
        match self {
            Param::Signal { signal, scale, offset, .. } => Param::Signal { signal, scale, offset, smooth: seconds },
            c => c,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    /// Makes colour from coordinates (no input).
    Source,
    /// Moves coordinates of its input.
    Coord,
    /// Changes its input's colour.
    Colour,
    /// Mixes its input with a second chain.
    Combine,
    /// Moves its input's coordinates by a second chain.
    CombineCoord,
}

/// The operations a scene may use, their kind and argument count. A
/// backend must implement all of them (the Hydra adapter checks on hello).
pub const OPS: &[(&str, OpKind, usize)] = &[
    ("noise", OpKind::Source, 2),
    ("voronoi", OpKind::Source, 3),
    ("osc", OpKind::Source, 3),
    ("shape", OpKind::Source, 3),
    ("gradient", OpKind::Source, 1),
    ("solid", OpKind::Source, 4),
    // The last frame (feedback).
    ("prev", OpKind::Source, 0),
    ("rotate", OpKind::Coord, 2),
    ("scale", OpKind::Coord, 5),
    ("pixelate", OpKind::Coord, 2),
    ("repeat", OpKind::Coord, 4),
    ("kaleid", OpKind::Coord, 1),
    ("scroll", OpKind::Coord, 4),
    ("scrollX", OpKind::Coord, 2),
    ("scrollY", OpKind::Coord, 2),
    ("posterize", OpKind::Colour, 2),
    ("shift", OpKind::Colour, 4),
    ("invert", OpKind::Colour, 1),
    ("contrast", OpKind::Colour, 1),
    ("brightness", OpKind::Colour, 1),
    ("luma", OpKind::Colour, 2),
    ("thresh", OpKind::Colour, 2),
    ("color", OpKind::Colour, 4),
    ("saturate", OpKind::Colour, 1),
    ("hue", OpKind::Colour, 1),
    ("colorama", OpKind::Colour, 1),
    ("add", OpKind::Combine, 1),
    ("sub", OpKind::Combine, 1),
    ("layer", OpKind::Combine, 0),
    ("blend", OpKind::Combine, 1),
    ("mult", OpKind::Combine, 1),
    ("diff", OpKind::Combine, 0),
    ("mask", OpKind::Combine, 0),
    ("modulate", OpKind::CombineCoord, 1),
    ("modulateScale", OpKind::CombineCoord, 2),
    ("modulateRotate", OpKind::CombineCoord, 2),
    ("modulateKaleid", OpKind::CombineCoord, 1),
    ("modulatePixelate", OpKind::CombineCoord, 2),
    ("modulateHue", OpKind::CombineCoord, 1),
];

pub fn op(name: &str) -> Option<(OpKind, usize)> {
    OPS.iter().find(|o| o.0 == name).map(|o| (o.1, o.2))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VisualNode {
    pub op: String,
    #[serde(default)]
    pub args: Vec<Param>,
    /// The chain it transforms first; a combiner's second chain next.
    #[serde(default)]
    pub inputs: Vec<usize>,
}

impl VisualNode {
    pub fn new(op: &str, args: Vec<Param>, inputs: Vec<usize>) -> VisualNode {
        VisualNode { op: op.into(), args, inputs }
    }
}

pub const MAX_NODES: usize = 64;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VisualScene {
    pub nodes: Vec<VisualNode>,
    /// The node whose colour is the picture.
    pub output: usize,
}

impl VisualScene {
    /// Limits and shape any author (an AI, a file, the socket) must keep:
    /// known ops, at most their arguments, the right inputs, no cycles.
    pub fn validate(&self) -> Result<(), String> {
        if self.nodes.is_empty() || self.nodes.len() > MAX_NODES {
            return Err(format!("a scene has 1 … {MAX_NODES} nodes"));
        }
        if self.output >= self.nodes.len() {
            return Err("the output isn't a node".into());
        }
        for (i, n) in self.nodes.iter().enumerate() {
            let (kind, argc) = op(&n.op).ok_or_else(|| format!("node {i}: no op '{}'", n.op))?;
            if n.args.len() > argc {
                return Err(format!("node {i}: {} takes {argc} arguments", n.op));
            }
            let want = match kind {
                OpKind::Source => 0,
                OpKind::Coord | OpKind::Colour => 1,
                OpKind::Combine | OpKind::CombineCoord => 2,
            };
            if n.inputs.len() != want {
                return Err(format!("node {i}: {} takes {want} input(s)", n.op));
            }
            if let Some(bad) = n.inputs.iter().find(|&&j| j >= self.nodes.len()) {
                return Err(format!("node {i}: no node {bad}"));
            }
            for p in &n.args {
                match p {
                    Param::Const { value } if !value.is_finite() => return Err(format!("node {i}: a constant isn't finite")),
                    Param::Signal { signal, scale, offset, smooth } => {
                        if !signal_known(signal) {
                            return Err(format!("node {i}: no signal '{signal}'"));
                        }
                        if !(scale.is_finite() && offset.is_finite() && (0.0..=10.0).contains(smooth)) {
                            return Err(format!("node {i}: '{signal}' has a bad scale, offset or smoothing"));
                        }
                    }
                    _ => {}
                }
            }
        }
        // No cycles: every path from the output ends at a source.
        fn walk(s: &VisualScene, i: usize, on: &mut Vec<bool>) -> Result<(), String> {
            if on[i] {
                return Err(format!("node {i} feeds itself"));
            }
            on[i] = true;
            for &j in &s.nodes[i].inputs {
                walk(s, j, on)?;
            }
            on[i] = false;
            Ok(())
        }
        walk(self, self.output, &mut vec![false; self.nodes.len()])
    }

    /// The structure, without signal bindings' values: a scene whose
    /// structure is unchanged needs no recompiling (only new uniforms).
    pub fn structure_key(&self) -> String {
        let mut s = String::new();
        for n in &self.nodes {
            s.push_str(&n.op);
            for p in &n.args {
                match p {
                    Param::Const { value } => s.push_str(&format!(",{value}")),
                    Param::Signal { .. } => s.push_str(",$"),
                }
            }
            s.push_str(&format!("{:?};", n.inputs));
        }
        s.push_str(&self.output.to_string());
        s
    }
}

// --- signals ----------------------------------------------------------------

/// The fixed signal names; plus `pattern:<param>`, `intent:<field>` and
/// `stem:<name>`.
pub const SIGNALS: &[&str] = &[
    "time", "beat", "phase", "bar", "phrase", "bpm", "energy", "low", "mid", "high", "kick", "crossfader", "transition",
];

pub fn signal_known(name: &str) -> bool {
    SIGNALS.contains(&name)
        || ["pattern:", "intent:", "stem:"]
            .iter()
            .any(|p| name.strip_prefix(p).is_some_and(|rest| !rest.is_empty() && rest.len() <= 32))
}

/// The bus: every signal's value now. Unset signals read 0.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Signals {
    values: BTreeMap<String, f32>,
}

impl Signals {
    pub fn set(&mut self, name: &str, v: f32) {
        if v.is_finite() {
            self.values.insert(name.to_string(), v);
        }
    }

    pub fn get(&self, name: &str) -> f32 {
        self.values.get(name).copied().unwrap_or(0.0)
    }

    pub fn has(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }

    /// A pattern's `visual.<field>(…)` steers the intent field of the same
    /// name, so a pattern drives every scene (they read `intent:*`).
    pub fn pattern_over_intent(&mut self) {
        for f in ["intensity", "motion", "symmetry", "density", "feedback", "hue"] {
            let key = format!("pattern:{f}");
            if self.has(&key) {
                let v = self.get(&key).clamp(0.0, 1.0);
                self.set(&format!("intent:{f}"), v);
            }
        }
    }

    /// The clock: `beat` counts beats; `phase` is within the beat; `bar`
    /// and `phrase` are positions 0…1 within a bar and an 8-bar phrase.
    pub fn set_clock(&mut self, seconds: f64, beat: f64, bpm: f64) {
        self.set("time", seconds as f32);
        self.set("beat", beat as f32);
        self.set("phase", beat.rem_euclid(1.0) as f32);
        self.set("bar", (beat / 4.0).rem_euclid(1.0) as f32);
        self.set("phrase", (beat / 32.0).rem_euclid(1.0) as f32);
        self.set("bpm", bpm as f32);
    }

    /// Hybrid: the intent as `intent:<field>` signals (0…1 each; section
    /// as an energy ladder, colour as hue).
    pub fn set_intent(&mut self, i: &Intent) {
        self.set("intent:intensity", i.intensity);
        self.set("intent:motion", i.motion);
        self.set("intent:symmetry", i.symmetry);
        self.set("intent:density", i.density);
        self.set("intent:feedback", i.feedback);
        self.set("intent:hue", i.hue.unwrap_or(0.0));
        self.set("intent:colour", if i.colour == Colour::Theme { 0.0 } else { 1.0 });
        self.set(
            "intent:section",
            match i.section {
                Section::Unknown | Section::Intro | Section::Outro => 0.2,
                Section::Verse | Section::Breakdown => 0.4,
                Section::Transition | Section::Build => 0.7,
                Section::Drop => 1.0,
            },
        );
    }
}

/// A compiled scene: one fragment shader body for Patch's GL header (GLSL
/// 3.30 core / ES 3.00: `in vec2 uv`, `out vec4 frag`), and which uniform
/// carries which argument.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledShader {
    pub frag: String,
    pub uniforms: Vec<(String, Param)>,
    /// Reads `prevBuffer` (the last frame): Patch binds its feedback texture.
    pub feedback: bool,
    pub compile_us: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    pub engine: String,
    pub version: String,
    pub feedback: bool,
    pub blur: bool,
    pub out_of_process: bool,
    pub sandboxed: bool,
}

pub trait VisualBackend {
    fn compile(&mut self, scene: &VisualScene) -> Result<CompiledShader, String>;
    fn capabilities(&self) -> Capabilities;
}

/// A shader's uniforms with their smoothing state: what the renderer sets
/// every frame.
#[derive(Clone, Debug, Default)]
pub struct Bindings {
    bound: Vec<(String, Param, f32)>,
}

impl Bindings {
    pub fn new(uniforms: &[(String, Param)]) -> Bindings {
        Bindings {
            bound: uniforms.iter().map(|(n, p)| (n.clone(), p.clone(), f32::NAN)).collect(),
        }
    }

    /// Every uniform's value now, `dt` seconds after the last call.
    pub fn values(&mut self, s: &Signals, dt: f32) -> Vec<(&str, f32)> {
        self.bound
            .iter_mut()
            .map(|(name, p, last)| {
                let v = match p {
                    Param::Const { value } => *value,
                    Param::Signal { signal, scale, offset, smooth } => {
                        let target = s.get(signal) * *scale + *offset;
                        if last.is_nan() || *smooth <= 0.0 {
                            target
                        } else {
                            *last + (target - *last) * (1.0 - (-dt / *smooth).exp())
                        }
                    }
                };
                *last = v;
                (name.as_str(), v)
            })
            .collect()
    }
}

// --- the GLSL shim ----------------------------------------------------------

/// GLSL ES 1.0 (WebGL 1, what Hydra writes) → a body for Patch's GLSL 3.30
/// core / ES 3.00 header: no `precision` line (the header has it),
/// `varying` → `in`, `gl_FragColor` → `frag` (declared), `texture2D` →
/// `texture`. Refuses what it doesn't know rather than guess.
pub fn shim(es1: &str) -> Result<String, String> {
    if es1.contains("#version") || es1.contains("#extension") {
        return Err("the shader brings its own #version or extensions".into());
    }
    if !es1.contains("gl_FragColor") {
        return Err("no gl_FragColor: not a fragment shader".into());
    }
    let mut out = String::with_capacity(es1.len() + 64);
    out.push_str("out vec4 frag;\n");
    for line in es1.lines() {
        let t = line.trim_start();
        if t.starts_with("precision ") {
            continue;
        }
        let line = if let Some(rest) = t.strip_prefix("varying ") {
            format!("in {rest}")
        } else {
            line.to_string()
        };
        out.push_str(&replace_word(&replace_word(&line, "gl_FragColor", "frag"), "texture2D", "texture"));
        out.push('\n');
    }
    Ok(out)
}

/// Replace whole identifiers only.
pub(crate) fn replace_word(s: &str, from: &str, to: &str) -> String {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(from) {
        let before = rest[..i].chars().next_back();
        let after = rest[i + from.len()..].chars().next();
        out.push_str(&rest[..i]);
        if before.is_some_and(ident) || after.is_some_and(ident) {
            out.push_str(from);
        } else {
            out.push_str(to);
        }
        rest = &rest[i + from.len()..];
    }
    out.push_str(rest);
    out
}

// --- modes and the compositor ----------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// The character-cell director alone (as before).
    #[default]
    Choreography,
    /// The generative scene alone.
    Generative,
    /// Both: the generative scene beneath, driven by the director's intent
    /// (or the AI director's), the cells over it.
    Hybrid,
}

impl Mode {
    pub fn next(self) -> Mode {
        match self {
            Mode::Choreography => Mode::Generative,
            Mode::Generative => Mode::Hybrid,
            Mode::Hybrid => Mode::Choreography,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Choreography => "choreography",
            Mode::Generative => "generative",
            Mode::Hybrid => "hybrid",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Blend {
    #[default]
    Normal,
    Add,
    Screen,
    Multiply,
}

impl Blend {
    /// Index in the compositor shader.
    pub fn index(self) -> i32 {
        self as i32
    }
}

/// How the two layers meet: the cells (choreography) over the generative
/// picture, at an opacity, in a blend mode; `mask` keeps the generative
/// layer only where the cells are dark (text stays legible).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Compositor {
    pub generative_opacity: f32,
    pub cells_opacity: f32,
    pub blend: Blend,
    pub mask: bool,
}

impl Default for Compositor {
    fn default() -> Compositor {
        Compositor { generative_opacity: 0.85, cells_opacity: 1.0, blend: Blend::Screen, mask: true }
    }
}

/// One channel of the cells (`top`) over the generative picture (`base`):
/// the reference for the compositor shader (`COMPOSITE_GLSL`), which must
/// agree with it.
pub fn composite(base: f32, top: f32, c: &Compositor) -> f32 {
    let g = base * c.generative_opacity;
    let t = top * c.cells_opacity;
    let g = if c.mask { g * (1.0 - top.clamp(0.0, 1.0)) } else { g };
    let v = match c.blend {
        Blend::Normal => {
            if top > 0.0 {
                t + g * (1.0 - c.cells_opacity)
            } else {
                g
            }
        }
        Blend::Add => g + t,
        Blend::Screen => 1.0 - (1.0 - g) * (1.0 - t),
        Blend::Multiply => {
            if top > 0.0 {
                g * t
            } else {
                g
            }
        }
    };
    v.clamp(0.0, 1.0)
}

/// The compositor in GLSL (per channel, as [`composite`]).
pub const COMPOSITE_GLSL: &str = r#"
vec3 composite(vec3 base, vec3 top, float gen_op, float cell_op, int blend, bool mask) {
    vec3 g = base * gen_op;
    vec3 t = top * cell_op;
    if (mask) g *= (vec3(1.0) - clamp(top, 0.0, 1.0));
    vec3 lit = step(vec3(1e-6), top);
    vec3 v;
    if (blend == 0) v = mix(g, t + g * (1.0 - cell_op), lit);
    else if (blend == 1) v = g + t;
    else if (blend == 2) v = vec3(1.0) - (vec3(1.0) - g) * (vec3(1.0) - t);
    else v = mix(g, g * t, lit);
    return clamp(v, 0.0, 1.0);
}
"#;

// --- presets ----------------------------------------------------------------

/// Scenes Patch ships, each driven by the bus (and, in hybrid, by intent).
pub fn presets() -> Vec<(&'static str, VisualScene)> {
    use Param as P;
    let n = VisualNode::new;
    vec![
        (
            "tunnel",
            // Oscillator through a kaleidoscope, fed back and pushed by the kick.
            VisualScene {
                nodes: vec![
                    n("osc", vec![P::c(12.0), P::sig("intent:motion", 0.2, 0.02).smoothed(0.5), P::sig("low", 1.2, 0.2)], vec![]),
                    n("kaleid", vec![P::sig("intent:symmetry", 8.0, 2.0).smoothed(1.0)], vec![0]),
                    n("prev", vec![], vec![]),
                    n("scale", vec![P::sig("kick", 0.06, 1.01)], vec![2]),
                    n("blend", vec![P::sig("intent:feedback", 0.9, 0.0).smoothed(1.0)], vec![1, 3]),
                    n("brightness", vec![P::sig("intent:intensity", 0.3, -0.15).smoothed(0.3)], vec![4]),
                ],
                output: 5,
            },
        ),
        (
            "cells",
            // Voronoi cells breathing with energy, hue walking with the phrase.
            VisualScene {
                nodes: vec![
                    n("voronoi", vec![P::sig("intent:density", 12.0, 3.0).smoothed(1.0), P::sig("energy", 0.6, 0.1), P::c(0.3)], vec![]),
                    n("hue", vec![P::sig("phrase", 1.0, 0.0)], vec![0]),
                    n("noise", vec![P::c(3.0), P::sig("intent:motion", 0.3, 0.05)], vec![]),
                    n("modulate", vec![P::sig("mid", 0.4, 0.05)], vec![1, 2]),
                    n("contrast", vec![P::sig("intent:intensity", 0.8, 1.0)], vec![3]),
                ],
                output: 4,
            },
        ),
        (
            "shapes",
            // A polygon on the beat, repeated by density, smeared by feedback.
            VisualScene {
                nodes: vec![
                    n("shape", vec![P::sig("intent:symmetry", 5.0, 3.0), P::sig("kick", 0.25, 0.2), P::c(0.02)], vec![]),
                    n("repeat", vec![P::sig("intent:density", 5.0, 1.0), P::sig("intent:density", 5.0, 1.0)], vec![0]),
                    n("rotate", vec![P::c(0.0), P::sig("intent:motion", 0.4, 0.0)], vec![1]),
                    n("prev", vec![], vec![]),
                    n("blend", vec![P::sig("intent:feedback", 0.8, 0.1)], vec![2, 3]),
                    n("colorama", vec![P::sig("high", 0.05, 0.0)], vec![4]),
                ],
                output: 5,
            },
        ),
        (
            "nebula",
            // Slow noise clouds twisted by their own brightness; the last
            // frame zoomed and turned a hair each frame (a drifting nebula);
            // the bass pushes the zoom, highs shift the colours.
            VisualScene {
                nodes: vec![
                    n("noise", vec![P::sig("intent:density", 4.0, 1.5).smoothed(2.0), P::sig("intent:motion", 0.15, 0.02).smoothed(1.0)], vec![]),
                    n("noise", vec![P::c(2.0), P::c(0.05)], vec![]),
                    n("modulateRotate", vec![P::sig("mid", 2.0, 0.5).smoothed(0.4), P::c(0.0)], vec![0, 1]),
                    n("colorama", vec![P::sig("high", 0.08, 0.005).smoothed(0.3)], vec![2]),
                    n("prev", vec![], vec![]),
                    n("scale", vec![P::sig("low", 0.04, 1.005).smoothed(0.15)], vec![4]),
                    n("rotate", vec![P::c(0.0), P::sig("intent:motion", 0.02, 0.002)], vec![5]),
                    n("blend", vec![P::sig("intent:feedback", 0.85, 0.1).smoothed(1.0)], vec![3, 6]),
                    n("brightness", vec![P::sig("intent:intensity", 0.25, -0.12).smoothed(0.5)], vec![7]),
                ],
                output: 8,
            },
        ),
        (
            "prism",
            // Stripes folded into a kaleidoscope whose folds follow the
            // intent's symmetry, refracted by a turning triangle, differenced
            // against themselves a phrase later: hard, geometric, for drops.
            VisualScene {
                nodes: vec![
                    n("osc", vec![P::sig("intent:intensity", 40.0, 10.0).smoothed(0.5), P::c(0.05), P::sig("phrase", 3.0, 0.0)], vec![]),
                    n("kaleid", vec![P::sig("intent:symmetry", 10.0, 3.0).smoothed(1.0)], vec![0]),
                    n("shape", vec![P::c(3.0), P::sig("kick", 0.3, 0.3), P::c(0.4)], vec![]),
                    n("rotate", vec![P::c(0.0), P::sig("intent:motion", 0.6, 0.05)], vec![2]),
                    n("modulateKaleid", vec![P::sig("intent:symmetry", 6.0, 2.0)], vec![1, 3]),
                    n("osc", vec![P::c(8.0), P::c(-0.03), P::c(1.2)], vec![]),
                    n("kaleid", vec![P::c(5.0)], vec![5]),
                    n("diff", vec![], vec![4, 6]),
                    n("saturate", vec![P::sig("intent:intensity", 1.5, 0.5)], vec![7]),
                ],
                output: 8,
            },
        ),
        (
            "lattice",
            // A Voronoi lattice tiled by density and pixelated on the beat
            // phase, thresholded into ink, then masking a colour oscillator:
            // circuitry that breathes with energy.
            VisualScene {
                nodes: vec![
                    n("voronoi", vec![P::c(8.0), P::sig("energy", 0.8, 0.1), P::c(0.0)], vec![]),
                    n("repeat", vec![P::sig("intent:density", 3.0, 1.0), P::sig("intent:density", 3.0, 1.0), P::c(0.0), P::c(0.0)], vec![0]),
                    n("pixelate", vec![P::sig("phase", -40.0, 80.0), P::sig("phase", -40.0, 80.0)], vec![1]),
                    n("thresh", vec![P::sig("low", -0.3, 0.6).smoothed(0.2), P::c(0.04)], vec![2]),
                    n("osc", vec![P::c(6.0), P::c(0.1), P::sig("bar", 2.0, 0.0)], vec![]),
                    n("hue", vec![P::sig("phrase", 1.0, 0.0)], vec![4]),
                    n("mask", vec![], vec![5, 3]),
                    n("prev", vec![], vec![]),
                    n("scrollY", vec![P::c(0.0), P::sig("intent:motion", 0.05, 0.0)], vec![7]),
                    n("add", vec![P::sig("intent:feedback", 0.6, 0.0)], vec![6, 8]),
                ],
                output: 9,
            },
        ),
        (
            "pulse",
            // Rings from the centre, one per kick, with trails: concentric
            // circles scaled by the kick envelope over a fading echo of
            // themselves, coloured by the phrase. Minimal, for grooves.
            VisualScene {
                nodes: vec![
                    n("shape", vec![P::c(64.0), P::sig("kick", 0.45, 0.05), P::c(0.01)], vec![]),
                    n("shape", vec![P::c(64.0), P::sig("kick", 0.45, 0.03), P::c(0.01)], vec![]),
                    n("diff", vec![], vec![0, 1]),
                    n("color", vec![P::sig("intent:hue", 1.0, 0.3), P::c(0.4), P::sig("phrase", 1.0, 0.2), P::c(1.0)], vec![2]),
                    n("prev", vec![], vec![]),
                    n("scale", vec![P::sig("energy", 0.05, 1.02)], vec![4]),
                    n("brightness", vec![P::c(-0.04)], vec![5]),
                    n("add", vec![P::sig("intent:feedback", 0.9, 0.05)], vec![3, 6]),
                ],
                output: 7,
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_valid_and_round_trip_as_json() {
        for (name, s) in presets() {
            s.validate().unwrap_or_else(|e| panic!("{name}: {e}"));
            let j = serde_json::to_string(&s).unwrap();
            let back: VisualScene = serde_json::from_str(&j).unwrap();
            assert_eq!(back, s, "{name}");
        }
        let p: Param = serde_json::from_str(r#"{"const": 2}"#).unwrap();
        assert_eq!(p, Param::c(2.0));
        let p: Param = serde_json::from_str(r#"{"signal": "kick"}"#).unwrap();
        assert_eq!(p, Param::sig("kick", 1.0, 0.0));
    }

    #[test]
    fn bad_scenes_say_why() {
        let n = VisualNode::new;
        let s = |nodes, output| VisualScene { nodes, output };
        for (scene, says) in [
            (s(vec![n("sparkle", vec![], vec![])], 0), "no op"),
            (s(vec![n("osc", vec![], vec![0])], 0), "0 input"),
            (s(vec![n("kaleid", vec![], vec![])], 0), "1 input"),
            (s(vec![n("osc", vec![], vec![]), n("kaleid", vec![], vec![1])], 1), "feeds itself"),
            (s(vec![n("osc", vec![Param::sig("wobble", 1.0, 0.0)], vec![])], 0), "no signal"),
            (s(vec![n("osc", vec![Param::c(1.0); 4], vec![])], 0), "3 arguments"),
            (s(vec![n("osc", vec![], vec![])], 3), "output"),
        ] {
            let e = scene.validate().unwrap_err();
            assert!(e.contains(says), "{e}");
        }
        assert!(signal_known("pattern:intensity") && signal_known("intent:hue") && !signal_known("pattern:"));
    }

    #[test]
    fn structure_ignores_signal_values_but_not_constants() {
        let a = presets().remove(0).1;
        let mut b = a.clone();
        b.nodes[0].args[1] = Param::sig("high", 5.0, 1.0);
        assert_eq!(a.structure_key(), b.structure_key());
        b.nodes[0].args[0] = Param::c(13.0);
        assert_ne!(a.structure_key(), b.structure_key());
    }

    #[test]
    fn bindings_scale_offset_and_smooth() {
        let mut b = Bindings::new(&[
            ("a0".into(), Param::sig("kick", 2.0, 1.0)),
            ("b1".into(), Param::sig("energy", 1.0, 0.0).smoothed(1.0)),
        ]);
        let mut s = Signals::default();
        s.set("kick", 0.5);
        s.set("energy", 0.0);
        assert_eq!(b.values(&s, 0.016), vec![("a0", 2.0), ("b1", 0.0)]);
        s.set("energy", 1.0);
        let v = b.values(&s, 1.0)[1].1;
        assert!((v - (1.0 - (-1.0f32).exp())).abs() < 1e-5, "one time constant: {v}");
    }

    #[test]
    fn the_shim_speaks_glsl_330() {
        let es1 = "  precision highp float;\n  uniform float sync0;\n  varying vec2 uv;\n  uniform sampler2D prevBuffer;\n  vec4 prev(vec2 _st) { return texture2D(prevBuffer, fract(_st)); }\n  void main () {\n    vec4 c = prev(uv);\n    gl_FragColor = c;\n  }\n";
        let out = shim(es1).unwrap();
        assert!(out.starts_with("out vec4 frag;"));
        assert!(!out.contains("precision") && !out.contains("varying") && !out.contains("texture2D") && !out.contains("gl_FragColor"));
        assert!(out.contains("in vec2 uv;") && out.contains("texture(prevBuffer") && out.contains("frag = c;"));
        assert_eq!(replace_word("mytexture2D(texture2D(x))", "texture2D", "texture"), "mytexture2D(texture(x))");
        assert!(shim("#version 100\ngl_FragColor = vec4(1);").is_err());
    }

    #[test]
    fn compositing_keeps_text_legible() {
        let c = Compositor::default();
        // Bright cell: the cell wins (masked generative beneath).
        assert_eq!(composite(0.8, 1.0, &c), 1.0);
        // Dark cell: the generative picture shows.
        assert!((composite(0.8, 0.0, &c) - 0.8 * 0.85).abs() < 1e-6);
        let add = Compositor { blend: Blend::Add, mask: false, ..c };
        assert_eq!(composite(0.5, 0.7, &add), 1.0);
        let normal = Compositor { blend: Blend::Normal, mask: false, cells_opacity: 0.5, ..c };
        assert!((composite(0.4, 0.6, &normal) - (0.3 + 0.4 * 0.85 * 0.5)).abs() < 1e-6);
    }

    #[test]
    fn intent_reaches_the_bus() {
        let mut s = Signals::default();
        let i = Intent { section: Section::Drop, intensity: 0.9, ..Default::default() };
        s.set_intent(&i);
        assert_eq!(s.get("intent:section"), 1.0);
        assert_eq!(s.get("intent:intensity"), 0.9);
        s.set_clock(1.0, 33.5, 128.0);
        assert_eq!((s.get("phase"), s.get("bar")), (0.5, 33.5 / 4.0 - 8.0));
        s.set("pattern:intensity", 0.2);
        s.pattern_over_intent();
        assert_eq!(s.get("intent:intensity"), 0.2, "a pattern steers the intent");
        assert_eq!(s.get("intent:section"), 1.0);
    }
}
