//! The AI director's tools on pattern programs: inspect, address, mutate, diff,
//! compare and explain — all on the Patch AST, never on engine code.
//!
//! A node is addressed by its layer and a path of child indices from the
//! layer's root ([`At`]): a sequence's steps, an alternation's or stack's
//! members, a polymeter's members in reading order, and an applied
//! transform's pattern (child 0). Mutations return a new, validated
//! program; nothing here runs or schedules anything.

use crate::ast::{Pat, Transform, sample};
use crate::program::{Layer, Program};
use crate::time::{Span, Time};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// A node: a layer and the child indices that lead to it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct At {
    pub layer: String,
    #[serde(default)]
    pub path: Vec<usize>,
}

/// A node's children, in reading order.
pub fn children(p: &Pat) -> Vec<&Pat> {
    match p {
        Pat::Silence | Pat::Atom(_) | Pat::Signal(_) => vec![],
        Pat::Seq(steps) => steps.iter().map(|s| &s.pat).collect(),
        Pat::Alt(ps) | Pat::Stack(ps) | Pat::Choose { pats: ps, .. } => ps.iter().collect(),
        Pat::Poly { pats, .. } => pats.iter().flatten().collect(),
        Pat::Apply { pat, .. } => vec![pat],
    }
}

fn child_mut(p: &mut Pat, i: usize) -> Option<&mut Pat> {
    match p {
        Pat::Silence | Pat::Atom(_) | Pat::Signal(_) => None,
        Pat::Seq(steps) => steps.get_mut(i).map(|s| &mut s.pat),
        Pat::Alt(ps) | Pat::Stack(ps) | Pat::Choose { pats: ps, .. } => ps.get_mut(i),
        Pat::Poly { pats, .. } => pats.iter_mut().flatten().nth(i),
        Pat::Apply { pat, .. } => (i == 0).then_some(pat.as_mut()),
    }
}

pub fn get<'a>(p: &'a Pat, path: &[usize]) -> Option<&'a Pat> {
    path.iter().try_fold(p, |p, &i| children(p).get(i).copied())
}

fn get_mut<'a>(p: &'a mut Pat, path: &[usize]) -> Option<&'a mut Pat> {
    path.iter().try_fold(p, |p, &i| child_mut(p, i))
}

/// Where a path points, in words: "step 2 › member 1".
pub fn place(root: &Pat, path: &[usize]) -> String {
    if path.is_empty() {
        return "the whole pattern".into();
    }
    let mut words = vec![];
    let mut p = root;
    for &i in path {
        let w = match p {
            Pat::Seq(_) => Some(format!("step {}", i + 1)),
            Pat::Alt(_) => Some(format!("cycle choice {}", i + 1)),
            Pat::Stack(_) => Some(format!("part {}", i + 1)),
            Pat::Choose { .. } => Some(format!("option {}", i + 1)),
            Pat::Poly { .. } => Some(format!("polymeter step {}", i + 1)),
            // Inside a transform is the same place, transformed.
            _ => None,
        };
        words.extend(w);
        match children(p).get(i) {
            Some(c) => p = c,
            None => break,
        }
    }
    if words.is_empty() {
        "the whole pattern".into()
    } else {
        words.join(" › ")
    }
}

// --- notation ---------------------------------------------------------------

/// A transform as code: `fast(2)`, `every(4, rev)`.
pub fn transform_text(t: &Transform) -> String {
    let n = t.name();
    match t {
        Transform::Fast(x) | Transform::Slow(x) | Transform::Early(x) | Transform::Late(x) => {
            format!("{n}({x})")
        }
        Transform::Rev | Transform::Palindrome | Transform::Id => n.into(),
        Transform::Ply(x) | Transform::Segment(x) | Transform::Iter(x) => format!("{n}({x})"),
        Transform::Euclid {
            pulses,
            steps,
            rotate: 0,
        } => format!("euclid({pulses}, {steps})"),
        Transform::Euclid {
            pulses,
            steps,
            rotate,
        } => format!("euclid({pulses}, {steps}, {rotate})"),
        Transform::Degrade { prob, .. } => format!("degrade({prob})"),
        Transform::Every {
            n: k,
            offset: 0,
            then,
        } => format!("every({k}, {})", transform_text(then)),
        Transform::Every { n: k, offset, then } => {
            format!("every({k}, {offset}, {})", transform_text(then))
        }
        Transform::Sometimes { prob, then, .. } | Transform::SomeCycles { prob, then, .. } => {
            format!("{n}({prob}, {})", transform_text(then))
        }
        Transform::Within { begin, end, then } => {
            format!("within({begin}, {end}, {})", transform_text(then))
        }
        Transform::Chunk { n: k, then } => format!("chunk({k}, {})", transform_text(then)),
        Transform::Off { by, then } => format!("off({by}, {})", transform_text(then)),
        Transform::Superimpose(then) => format!("superimpose({})", transform_text(then)),
        Transform::Add(x) | Transform::Mul(x) => format!("{n}({x})"),
        Transform::Range { lo, hi } => format!("range({lo}, {hi})"),
        Transform::Set(v) => format!("set({v})"),
        Transform::Chain(ts) => ts.iter().map(transform_text).collect::<Vec<_>>().join("."),
    }
}

/// A pattern as compact mini-notation with transforms appended:
/// `[bd <sd cp>]@2 hh*2` style, `.fast(2)` after a group.
pub fn notation(p: &Pat) -> String {
    fn inner(p: &Pat, top: bool) -> String {
        let join = |ps: &[&Pat], sep: &str| {
            ps.iter()
                .map(|p| inner(p, false))
                .collect::<Vec<_>>()
                .join(sep)
        };
        match p {
            Pat::Silence => "~".into(),
            Pat::Atom(v) => v.to_string(),
            Pat::Signal(s) => s.name().into(),
            Pat::Seq(steps) => {
                let body = steps
                    .iter()
                    .map(|s| {
                        let x = inner(&s.pat, false);
                        if s.weight == Time::ONE {
                            x
                        } else {
                            format!("{x}@{}", s.weight)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                if top { body } else { format!("[{body}]") }
            }
            Pat::Alt(ps) => format!("<{}>", join(&ps.iter().collect::<Vec<_>>(), " ")),
            Pat::Stack(ps) => format!("[{}]", join(&ps.iter().collect::<Vec<_>>(), ", ")),
            Pat::Choose { pats, .. } => {
                format!("[{}]", join(&pats.iter().collect::<Vec<_>>(), " | "))
            }
            Pat::Poly { pats, steps } => {
                let body = pats
                    .iter()
                    .map(|seq| join(&seq.iter().collect::<Vec<_>>(), " "))
                    .collect::<Vec<_>>()
                    .join(", ");
                match steps {
                    Some(n) => format!("{{{body}}}%{n}"),
                    None => format!("{{{body}}}"),
                }
            }
            Pat::Apply { transform, pat } => {
                let base = inner(pat, false);
                format!("{base}.{}", transform_text(transform))
            }
        }
    }
    inner(p, true)
}

// --- describe ---------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LayerSummary {
    pub name: String,
    pub target: String,
    pub muted: bool,
    pub nodes: usize,
    pub notation: String,
    /// The words it can play (sounds, slots, scenes).
    pub words: Vec<String>,
    /// Transform names used, outermost first.
    pub transforms: Vec<String>,
    /// Mean onsets per cycle over the first four cycles.
    pub density: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Summary {
    pub cycle_beats: Time,
    pub layers: Vec<LayerSummary>,
    /// One line per layer, for a person or a language model.
    pub text: String,
}

fn transforms_used(p: &Pat, out: &mut Vec<String>) {
    if let Pat::Apply { transform, .. } = p {
        out.push(transform.name().to_string());
    }
    for c in children(p) {
        transforms_used(c, out);
    }
}

const DENSITY_CYCLES: i64 = 4;

fn density(p: &Pat, seed: u64) -> f64 {
    sample(p, 0, DENSITY_CYCLES, seed).len() as f64 / DENSITY_CYCLES as f64
}

pub fn describe(program: &Program) -> Summary {
    let layers: Vec<LayerSummary> = program
        .layers
        .iter()
        .map(|l| {
            let mut transforms = vec![];
            transforms_used(&l.pat, &mut transforms);
            LayerSummary {
                name: l.name.clone(),
                target: l.target.label(),
                muted: l.muted,
                nodes: l.pat.size(),
                notation: notation(&l.pat),
                words: l.pat.words(),
                transforms,
                density: density(&l.pat, program.seed),
            }
        })
        .collect();
    let mut text = format!(
        "{} layer{}, a cycle is {} beats",
        layers.len(),
        if layers.len() == 1 { "" } else { "s" },
        program.cycle_beats
    );
    for l in &layers {
        text.push_str(&format!(
            "\n{}{} → {}: {} ({:.2} events/cycle)",
            l.name,
            if l.muted { " (muted)" } else { "" },
            l.target,
            l.notation,
            l.density
        ));
    }
    Summary {
        cycle_beats: program.cycle_beats,
        layers,
        text,
    }
}

// --- mutate -----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "edit", rename_all = "snake_case")]
pub enum Edit {
    /// The node becomes `pat`.
    Replace {
        pat: Pat,
    },
    /// The node is wrapped in a transform.
    Wrap {
        transform: Transform,
    },
    /// An applied transform is removed, keeping its pattern.
    Unwrap,
    /// The layer (the path is ignored).
    Mute,
    Unmute,
    /// A new layer after the others (the address names it).
    AddLayer {
        layer: Layer,
    },
    RemoveLayer,
}

pub fn mutate(program: &Program, at: &At, edit: &Edit) -> Result<Program, String> {
    let mut p = program.clone();
    if let Edit::AddLayer { layer } = edit {
        if layer.name != at.layer {
            return Err(format!(
                "the new layer is '{}', not '{}'",
                layer.name, at.layer
            ));
        }
        p.layers.push(layer.clone());
        p.validate()?;
        return Ok(p);
    }
    let li = p
        .layers
        .iter()
        .position(|l| l.name == at.layer)
        .ok_or_else(|| format!("no layer '{}'", at.layer))?;
    match edit {
        Edit::Mute => p.layers[li].muted = true,
        Edit::Unmute => p.layers[li].muted = false,
        Edit::RemoveLayer => {
            p.layers.remove(li);
        }
        Edit::AddLayer { .. } => unreachable!(),
        Edit::Replace { .. } | Edit::Wrap { .. } | Edit::Unwrap => {
            let node = get_mut(&mut p.layers[li].pat, &at.path)
                .ok_or_else(|| format!("{}: no node at {:?}", at.layer, at.path))?;
            let old = std::mem::replace(node, Pat::Silence);
            *node = match (edit, old) {
                (Edit::Replace { pat }, _) => pat.clone(),
                (Edit::Wrap { transform }, old) => old.apply(transform.clone()),
                (Edit::Unwrap, Pat::Apply { pat, .. }) => *pat,
                (Edit::Unwrap, old) => {
                    *node = old;
                    return Err(format!("{}: {:?} is not a transform", at.layer, at.path));
                }
                _ => unreachable!(),
            };
        }
    }
    p.validate()?;
    Ok(p)
}

// --- diff -------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum Change {
    CycleBeats {
        from: Time,
        to: Time,
    },
    Seed {
        from: u64,
        to: u64,
    },
    LayerAdded {
        layer: String,
        target: String,
        notation: String,
    },
    LayerRemoved {
        layer: String,
    },
    Retargeted {
        layer: String,
        from: String,
        to: String,
    },
    Muted {
        layer: String,
    },
    Unmuted {
        layer: String,
    },
    Wrapped {
        layer: String,
        path: Vec<usize>,
        place: String,
        transform: String,
    },
    Unwrapped {
        layer: String,
        path: Vec<usize>,
        place: String,
        transform: String,
    },
    TransformChanged {
        layer: String,
        path: Vec<usize>,
        place: String,
        from: String,
        to: String,
    },
    Replaced {
        layer: String,
        path: Vec<usize>,
        place: String,
        from: String,
        to: String,
    },
}

/// Same shape: the same variant, the same number of children, and the same
/// non-child data (weights, seeds, steps), so a difference is inside.
fn same_shape(a: &Pat, b: &Pat) -> bool {
    match (a, b) {
        (Pat::Seq(x), Pat::Seq(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(s, t)| s.weight == t.weight)
        }
        (Pat::Alt(x), Pat::Alt(y)) | (Pat::Stack(x), Pat::Stack(y)) => x.len() == y.len(),
        (Pat::Choose { pats: x, seed: s }, Pat::Choose { pats: y, seed: t }) => {
            x.len() == y.len() && s == t
        }
        (Pat::Poly { pats: x, steps: s }, Pat::Poly { pats: y, steps: t }) => {
            s == t && x.len() == y.len() && x.iter().zip(y).all(|(a, b)| a.len() == b.len())
        }
        (Pat::Apply { transform: s, .. }, Pat::Apply { transform: t, .. }) => s == t,
        _ => false,
    }
}

fn diff_pat(
    layer: &str,
    root: &Pat,
    a: &Pat,
    b: &Pat,
    path: &mut Vec<usize>,
    out: &mut Vec<Change>,
) {
    if a == b {
        return;
    }
    let (layer, at) = (layer.to_string(), path.clone());
    let place = place(root, path);
    match (a, b) {
        (_, Pat::Apply { transform, pat }) if **pat == *a => out.push(Change::Wrapped {
            layer,
            path: at,
            place,
            transform: transform_text(transform),
        }),
        (Pat::Apply { transform, pat }, _) if **pat == *b => out.push(Change::Unwrapped {
            layer,
            path: at,
            place,
            transform: transform_text(transform),
        }),
        (
            Pat::Apply {
                transform: s,
                pat: x,
            },
            Pat::Apply {
                transform: t,
                pat: y,
            },
        ) if x == y && s != t => out.push(Change::TransformChanged {
            layer,
            path: at,
            place,
            from: transform_text(s),
            to: transform_text(t),
        }),
        _ if same_shape(a, b) => {
            for (i, (x, y)) in children(a).into_iter().zip(children(b)).enumerate() {
                path.push(i);
                diff_pat(&layer, root, x, y, path, out);
                path.pop();
            }
        }
        _ => out.push(Change::Replaced {
            layer,
            path: at,
            place,
            from: notation(a),
            to: notation(b),
        }),
    }
}

/// What changed from `a` to `b`, structurally, layers matched by name.
pub fn diff(a: &Program, b: &Program) -> Vec<Change> {
    let mut out = vec![];
    if a.cycle_beats != b.cycle_beats {
        out.push(Change::CycleBeats {
            from: a.cycle_beats,
            to: b.cycle_beats,
        });
    }
    if a.seed != b.seed {
        out.push(Change::Seed {
            from: a.seed,
            to: b.seed,
        });
    }
    for la in &a.layers {
        match b.layer(&la.name) {
            None => out.push(Change::LayerRemoved {
                layer: la.name.clone(),
            }),
            Some(lb) => {
                if la.target != lb.target {
                    out.push(Change::Retargeted {
                        layer: la.name.clone(),
                        from: la.target.label(),
                        to: lb.target.label(),
                    });
                }
                match (la.muted, lb.muted) {
                    (false, true) => out.push(Change::Muted {
                        layer: la.name.clone(),
                    }),
                    (true, false) => out.push(Change::Unmuted {
                        layer: la.name.clone(),
                    }),
                    _ => {}
                }
                diff_pat(&la.name, &la.pat, &la.pat, &lb.pat, &mut vec![], &mut out);
            }
        }
    }
    for lb in &b.layers {
        if a.layer(&lb.name).is_none() {
            out.push(Change::LayerAdded {
                layer: lb.name.clone(),
                target: lb.target.label(),
                notation: notation(&lb.pat),
            });
        }
    }
    out
}

/// A change in words.
pub fn explain_change(c: &Change) -> String {
    match c {
        Change::CycleBeats { from, to } => format!("a cycle is now {to} beats (was {from})"),
        Change::Seed { .. } => "the random choices are reshuffled (new seed)".into(),
        Change::LayerAdded {
            layer,
            target,
            notation,
        } => format!("new layer {layer} → {target}: {notation}"),
        Change::LayerRemoved { layer } => format!("{layer} is removed"),
        Change::Retargeted { layer, from, to } => format!("{layer} now drives {to} (was {from})"),
        Change::Muted { layer } => format!("{layer} is muted"),
        Change::Unmuted { layer } => format!("{layer} plays again"),
        Change::Wrapped {
            layer,
            place,
            transform,
            ..
        } => format!("{layer}: {place} now goes through {transform}"),
        Change::Unwrapped {
            layer,
            place,
            transform,
            ..
        } => format!("{layer}: {place} no longer goes through {transform}"),
        Change::TransformChanged {
            layer,
            place,
            from,
            to,
            ..
        } => format!("{layer}: {place} uses {to} instead of {from}"),
        Change::Replaced {
            layer,
            place,
            from,
            to,
            ..
        } => format!("{layer}: {place} plays {to} instead of {from}"),
    }
}

/// Every change from `a` to `b`, one sentence each.
pub fn explain(a: &Program, b: &Program) -> Vec<String> {
    diff(a, b).iter().map(explain_change).collect()
}

// --- compare ----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Comparison {
    pub cycles: i64,
    /// Onsets per cycle.
    pub density_a: f64,
    pub density_b: f64,
    /// Shared onsets (time and target) over all onsets of either: 1 is the
    /// same rhythm on the same targets, 0 nothing in common.
    pub onset_overlap: f64,
    /// The same, counting the value too (same rhythm *and* same notes).
    pub event_overlap: f64,
    pub only_a: usize,
    pub only_b: usize,
    pub words_added: Vec<String>,
    pub words_removed: Vec<String>,
}

fn jaccard<T: Ord>(a: &BTreeSet<T>, b: &BTreeSet<T>) -> f64 {
    let union = a.union(b).count();
    if union == 0 {
        1.0
    } else {
        a.intersection(b).count() as f64 / union as f64
    }
}

/// How two programs sound against each other over `cycles` cycles from 0.
pub fn compare(a: &Program, b: &Program, cycles: i64) -> Comparison {
    let cycles = cycles.clamp(1, 256);
    let span = Span::cycles(0, cycles);
    let events = |p: &Program| p.compile().events(span);
    let (ea, eb) = (events(a), events(b));
    let onsets = |e: &[crate::event::Event]| -> BTreeSet<(Time, String)> {
        e.iter().map(|e| (e.onset(), e.target.label())).collect()
    };
    let full = |e: &[crate::event::Event]| -> BTreeSet<(Time, String, String)> {
        e.iter()
            .map(|e| (e.onset(), e.target.label(), e.value.to_string()))
            .collect()
    };
    let (fa, fb) = (full(&ea), full(&eb));
    let words =
        |p: &Program| -> BTreeSet<String> { p.layers.iter().flat_map(|l| l.pat.words()).collect() };
    let (wa, wb) = (words(a), words(b));
    Comparison {
        cycles,
        density_a: ea.len() as f64 / cycles as f64,
        density_b: eb.len() as f64 / cycles as f64,
        onset_overlap: jaccard(&onsets(&ea), &onsets(&eb)),
        event_overlap: jaccard(&fa, &fb),
        only_a: fa.difference(&fb).count(),
        only_b: fb.difference(&fa).count(),
        words_added: wb.difference(&wa).cloned().collect(),
        words_removed: wa.difference(&wb).cloned().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::parse;

    fn drums() -> Program {
        parse("s(\"bd [sd sd] <hh oh> ~\")\ndeck(a).filter(\"<0.2 -0.4>\")").unwrap()
    }

    #[test]
    fn notation_reads_like_the_mini_language() {
        let p = drums();
        assert_eq!(notation(&p.layers[0].pat), "bd [sd sd] <hh oh> ~");
        let d = describe(&p);
        assert_eq!(d.layers.len(), 2);
        assert_eq!(d.layers[0].density, 4.0);
        assert!(d.text.contains("bd [sd sd] <hh oh> ~"), "{}", d.text);
        assert!(d.layers[0].words.contains(&"oh".to_string()));
    }

    #[test]
    fn paths_address_nodes_and_mutations_land_there() {
        let p = drums();
        let layer = p.layers[0].name.clone();
        let at = At {
            layer: layer.clone(),
            path: vec![1, 0],
        };
        assert_eq!(
            get(&p.layers[0].pat, &at.path),
            Some(&Pat::Atom(crate::Value::atom("sd")))
        );
        assert_eq!(place(&p.layers[0].pat, &at.path), "step 2 › step 1");

        let wrapped = mutate(
            &p,
            &At {
                layer: layer.clone(),
                path: vec![1],
            },
            &Edit::Wrap {
                transform: Transform::Fast(Time::whole(2)),
            },
        )
        .unwrap();
        assert_eq!(
            notation(&wrapped.layers[0].pat),
            "bd [sd sd].fast(2) <hh oh> ~"
        );
        let back = mutate(
            &wrapped,
            &At {
                layer: layer.clone(),
                path: vec![1],
            },
            &Edit::Unwrap,
        )
        .unwrap();
        assert_eq!(back, p, "unwrap undoes wrap");
        assert!(
            mutate(&p, &at, &Edit::Unwrap).is_err(),
            "an atom is not a transform"
        );
        assert!(
            mutate(
                &p,
                &At {
                    layer: "nope".into(),
                    path: vec![]
                },
                &Edit::Mute
            )
            .is_err()
        );
        assert!(
            mutate(
                &p,
                &At {
                    layer: layer.clone(),
                    path: vec![9]
                },
                &Edit::Unwrap
            )
            .is_err()
        );
        let muted = mutate(
            &p,
            &At {
                layer,
                path: vec![],
            },
            &Edit::Mute,
        )
        .unwrap();
        assert!(muted.layers[0].muted);
    }

    #[test]
    fn diff_finds_the_edit_and_explains_it() {
        let a = drums();
        let name = a.layers[0].name.clone();
        let b = mutate(
            &a,
            &At {
                layer: name.clone(),
                path: vec![2],
            },
            &Edit::Wrap {
                transform: Transform::Rev,
            },
        )
        .unwrap();
        let c = mutate(
            &b,
            &At {
                layer: name.clone(),
                path: vec![0],
            },
            &Edit::Replace {
                pat: Pat::Atom(crate::Value::atom("cp")),
            },
        )
        .unwrap();
        let changes = diff(&a, &c);
        assert_eq!(changes.len(), 2, "{changes:?}");
        assert!(
            matches!(&changes[0], Change::Replaced { path, from, to, .. } if path == &[0] && from == "bd" && to == "cp")
        );
        assert!(
            matches!(&changes[1], Change::Wrapped { path, transform, .. } if path == &[2] && transform == "rev")
        );
        let words = explain(&a, &c);
        assert_eq!(words[0], format!("{name}: step 1 plays cp instead of bd"));
        assert_eq!(words[1], format!("{name}: step 3 now goes through rev"));
        assert!(diff(&a, &a).is_empty());
        // Layers by name: removed, added, muted.
        let mut d = a.clone();
        d.layers.remove(1);
        d.layers[0].muted = true;
        let ch = diff(&a, &d);
        assert!(ch.iter().any(|c| matches!(c, Change::LayerRemoved { .. })));
        assert!(ch.iter().any(|c| matches!(c, Change::Muted { .. })));
        assert!(
            diff(&d, &a)
                .iter()
                .any(|c| matches!(c, Change::LayerAdded { .. }))
        );
    }

    #[test]
    fn compare_measures_rhythm_and_content() {
        let a = parse("s(\"bd*4\")").unwrap();
        let same_rhythm = parse("s(\"sd*4\")").unwrap();
        let half = parse("s(\"bd*2\")").unwrap();
        let c = compare(&a, &a, 4);
        assert_eq!((c.onset_overlap, c.event_overlap, c.only_a), (1.0, 1.0, 0));
        let c = compare(&a, &same_rhythm, 4);
        assert_eq!(c.onset_overlap, 1.0);
        assert_eq!(c.event_overlap, 0.0);
        assert_eq!(
            (c.words_added.clone(), c.words_removed.clone()),
            (vec!["sd".to_string()], vec!["bd".to_string()])
        );
        let c = compare(&a, &half, 4);
        assert_eq!((c.density_a, c.density_b), (4.0, 2.0));
        assert_eq!(c.onset_overlap, 0.5);
        assert_eq!(c.only_a, 8);
    }

    #[test]
    fn edits_round_trip_as_json() {
        let e: Edit = serde_json::from_str(r#"{"edit":"wrap","transform":{"fast":"2"}}"#).unwrap();
        assert_eq!(
            e,
            Edit::Wrap {
                transform: Transform::Fast(Time::whole(2))
            }
        );
        let at: At = serde_json::from_str(r#"{"layer":"x"}"#).unwrap();
        assert!(at.path.is_empty());
    }
}
