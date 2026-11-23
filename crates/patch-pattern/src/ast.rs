//! The Pattern AST: what every author of a pattern builds.
//!
//! Typed code, mini-notation, the Sequencer, the AI director and the API all
//! produce a [`Pat`]; [`compile`] turns it into a [`Pattern<Value>`]. The
//! AST is plain serde data, so it can be saved, diffed, mutated by an AI and
//! sent over the control socket without ever going through source text.

use crate::pattern::Pattern;
use crate::time::{Span, Time};
use serde::{Deserialize, Serialize};
use std::fmt;

/// What an event carries: a number or a word ("bd", "c4", "mute").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Num(f64),
    Text(String),
}

impl Value {
    /// "1.5" → 1.5; words stay words.
    pub fn atom(word: &str) -> Value {
        match word.parse::<f64>() {
            Ok(n) if n.is_finite() => Value::Num(n),
            _ => Value::Text(word.to_string()),
        }
    }

    pub fn num(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            Value::Text(_) => None,
        }
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            Value::Num(_) => None,
        }
    }

    /// A word becomes true unless it is "0", "false", "off" or "~".
    pub fn truthy(&self) -> bool {
        match self {
            Value::Num(n) => *n != 0.0,
            Value::Text(s) => !matches!(s.as_str(), "0" | "false" | "off" | "f"),
        }
    }
}

impl From<Value> for bool {
    fn from(v: Value) -> bool {
        v.truthy()
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Value::Num(n) => write!(f, "{n}"),
            Value::Text(s) => f.write_str(s),
        }
    }
}

/// A continuous signal, one cycle per period, 0…1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    Sine,
    Cosine,
    Saw,
    Isaw,
    Tri,
    Square,
    Rand,
}

impl Signal {
    pub const ALL: [Signal; 7] = [
        Signal::Sine,
        Signal::Cosine,
        Signal::Saw,
        Signal::Isaw,
        Signal::Tri,
        Signal::Square,
        Signal::Rand,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Signal::Sine => "sine",
            Signal::Cosine => "cosine",
            Signal::Saw => "saw",
            Signal::Isaw => "isaw",
            Signal::Tri => "tri",
            Signal::Square => "square",
            Signal::Rand => "rand",
        }
    }

    pub fn parse(s: &str) -> Option<Signal> {
        Signal::ALL.into_iter().find(|x| x.name() == s)
    }

    pub fn at(self, t: Time, seed: u64) -> f64 {
        let x = t.cycle_pos().to_f64();
        let tau = std::f64::consts::TAU;
        match self {
            Signal::Sine => 0.5 + 0.5 * (x * tau).sin(),
            Signal::Cosine => 0.5 + 0.5 * (x * tau).cos(),
            Signal::Saw => x,
            Signal::Isaw => 1.0 - x,
            Signal::Tri => {
                if x < 0.5 {
                    x * 2.0
                } else {
                    2.0 - x * 2.0
                }
            }
            Signal::Square => {
                if x < 0.5 {
                    0.0
                } else {
                    1.0
                }
            }
            Signal::Rand => crate::pattern::rand_at(seed, t, 0x5eed),
        }
    }
}

/// One step of a sequence and its share of the step's time (`a@3`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub pat: Pat,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub weight: Time,
}

fn one() -> Time {
    Time::ONE
}

fn is_one(t: &Time) -> bool {
    *t == Time::ONE
}

/// A pattern as data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pat {
    /// `~`
    Silence,
    /// One value per cycle.
    Atom(Value),
    /// A sequence squeezed into one cycle (`a b c`, `[a b]`), with weights.
    Seq(Vec<Step>),
    /// One per cycle in turn (`<a b c>`).
    Alt(Vec<Pat>),
    /// Together (`a, b`).
    Stack(Vec<Pat>),
    /// One of them per cycle, at random (`a | b`).
    Choose { pats: Vec<Pat>, seed: u64 },
    /// Each pattern's steps at `steps` per cycle (`{a b c, d e}%4`); with
    /// no count, the first pattern's length.
    Poly {
        pats: Vec<Vec<Pat>>,
        #[serde(default)]
        steps: Option<Time>,
    },
    /// A continuous 0…1 signal.
    Signal(Signal),
    /// A transformation of a pattern.
    Apply { transform: Transform, pat: Box<Pat> },
}

/// A function on patterns, as data: what `every` and `sometimes` apply.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transform {
    Fast(Time),
    Slow(Time),
    /// Shift earlier (positive) or later (negative), in cycles.
    Early(Time),
    Late(Time),
    Rev,
    Palindrome,
    Ply(u32),
    Segment(u32),
    Iter(u32),
    Euclid {
        pulses: u32,
        steps: u32,
        #[serde(default)]
        rotate: i32,
    },
    Degrade {
        prob: f64,
        #[serde(default)]
        seed: u64,
    },
    Every {
        n: u32,
        #[serde(default)]
        offset: i64,
        then: Box<Transform>,
    },
    Sometimes {
        prob: f64,
        #[serde(default)]
        seed: u64,
        then: Box<Transform>,
    },
    SomeCycles {
        prob: f64,
        #[serde(default)]
        seed: u64,
        then: Box<Transform>,
    },
    Within {
        begin: Time,
        end: Time,
        then: Box<Transform>,
    },
    Chunk {
        n: u32,
        then: Box<Transform>,
    },
    Off {
        by: Time,
        then: Box<Transform>,
    },
    Superimpose(Box<Transform>),
    /// Numbers: `v * mul + add`; `range` maps 0…1 onto lo…hi.
    Add(f64),
    Mul(f64),
    Range {
        lo: f64,
        hi: f64,
    },
    /// Every value replaced (`mute`, `set("solo")`).
    Set(Value),
    /// Several, left to right (`fast(2).rev`).
    Chain(Vec<Transform>),
    /// Nothing (a placeholder an editor can fill).
    Id,
}

impl Transform {
    pub fn name(&self) -> &'static str {
        match self {
            Transform::Fast(_) => "fast",
            Transform::Slow(_) => "slow",
            Transform::Early(_) => "early",
            Transform::Late(_) => "late",
            Transform::Rev => "rev",
            Transform::Palindrome => "palindrome",
            Transform::Ply(_) => "ply",
            Transform::Segment(_) => "segment",
            Transform::Iter(_) => "iter",
            Transform::Euclid { .. } => "euclid",
            Transform::Degrade { .. } => "degrade",
            Transform::Every { .. } => "every",
            Transform::Sometimes { .. } => "sometimes",
            Transform::SomeCycles { .. } => "somecycles",
            Transform::Within { .. } => "within",
            Transform::Chunk { .. } => "chunk",
            Transform::Off { .. } => "off",
            Transform::Superimpose(_) => "superimpose",
            Transform::Add(_) => "add",
            Transform::Mul(_) => "mul",
            Transform::Range { .. } => "range",
            Transform::Set(_) => "set",
            Transform::Chain(_) => "chain",
            Transform::Id => "id",
        }
    }

    /// The transform applied to a compiled pattern.
    pub fn apply(&self, p: Pattern<Value>) -> Pattern<Value> {
        match self {
            Transform::Fast(t) => p.fast(*t),
            Transform::Slow(t) => p.slow(*t),
            Transform::Early(t) => p.early(*t),
            Transform::Late(t) => p.late(*t),
            Transform::Rev => p.rev(),
            Transform::Palindrome => p.every(2, 1, |x| x.rev()),
            Transform::Ply(n) => p.ply(*n),
            Transform::Segment(n) => p.segment(*n),
            Transform::Iter(n) => p.iter(*n),
            Transform::Euclid {
                pulses,
                steps,
                rotate,
            } => p.euclid(*pulses, *steps, *rotate),
            Transform::Degrade { prob, seed } => p.degrade_by(*prob, *seed),
            Transform::Every { n, offset, then } => p.every(*n, *offset, |x| then.apply(x)),
            Transform::Sometimes { prob, seed, then } => {
                p.sometimes_by(*prob, *seed, |x| then.apply(x))
            }
            Transform::SomeCycles { prob, seed, then } => {
                p.somecycles_by(*prob, *seed, |x| then.apply(x))
            }
            Transform::Within { begin, end, then } => p.within(*begin, *end, |x| then.apply(x)),
            Transform::Chunk { n, then } => p.chunk(*n, |x| then.apply(x)),
            Transform::Off { by, then } => p.off(*by, |x| then.apply(x)),
            Transform::Superimpose(then) => p.superimpose(|x| then.apply(x)),
            Transform::Add(a) => {
                let a = *a;
                p.fmap(move |v| map_num(v, |n| n + a))
            }
            Transform::Mul(m) => {
                let m = *m;
                p.fmap(move |v| map_num(v, |n| n * m))
            }
            Transform::Range { lo, hi } => {
                let (lo, hi) = (*lo, *hi);
                p.fmap(move |v| map_num(v, |n| lo + n * (hi - lo)))
            }
            Transform::Set(v) => {
                let v = v.clone();
                p.fmap(move |_| v.clone())
            }
            Transform::Chain(ts) => ts.iter().fold(p, |p, t| t.apply(p)),
            Transform::Id => p,
        }
    }
}

fn map_num(v: Value, f: impl Fn(f64) -> f64) -> Value {
    match v {
        Value::Num(n) => Value::Num(f(n)),
        other => other,
    }
}

/// The AST as a runnable pattern.
pub fn compile(p: &Pat) -> Pattern<Value> {
    match p {
        Pat::Silence => Pattern::silence(),
        Pat::Atom(v) => Pattern::pure(v.clone()),
        Pat::Seq(steps) => match steps.as_slice() {
            [one] => compile(&one.pat),
            _ => Pattern::timecat(steps.iter().map(|s| (s.weight, compile(&s.pat))).collect()),
        },
        Pat::Alt(ps) => Pattern::slowcat(ps.iter().map(compile).collect()),
        Pat::Stack(ps) => Pattern::stack(ps.iter().map(compile).collect()),
        Pat::Choose { pats, seed } => {
            Pattern::choose_cycle(pats.iter().map(compile).collect(), *seed)
        }
        Pat::Poly { pats, steps } => {
            let base = steps
                .unwrap_or_else(|| Time::whole(pats.first().map_or(1, |p| p.len().max(1)) as i64));
            Pattern::stack(
                pats.iter()
                    .filter(|seq| !seq.is_empty())
                    .map(|seq| {
                        let n = Time::whole(seq.len() as i64);
                        Pattern::fastcat(seq.iter().map(compile).collect()).fast(base / n)
                    })
                    .collect(),
            )
        }
        Pat::Signal(s) => {
            let s = *s;
            Pattern::signal(move |t, seed| Value::Num(s.at(t, seed)))
        }
        Pat::Apply { transform, pat } => transform.apply(compile(pat)),
    }
}

impl Pat {
    pub fn apply(self, transform: Transform) -> Pat {
        Pat::Apply {
            transform,
            pat: Box::new(self),
        }
    }

    pub fn seq(pats: Vec<Pat>) -> Pat {
        Pat::Seq(
            pats.into_iter()
                .map(|pat| Step {
                    pat,
                    weight: Time::ONE,
                })
                .collect(),
        )
    }

    /// Nodes in the tree (a size for limits and for comparing edits).
    pub fn size(&self) -> usize {
        1 + match self {
            Pat::Silence | Pat::Atom(_) | Pat::Signal(_) => 0,
            Pat::Seq(s) => s.iter().map(|s| s.pat.size()).sum(),
            Pat::Alt(ps) | Pat::Stack(ps) | Pat::Choose { pats: ps, .. } => {
                ps.iter().map(Pat::size).sum()
            }
            Pat::Poly { pats, .. } => pats.iter().flatten().map(Pat::size).sum(),
            Pat::Apply { pat, .. } => pat.size(),
        }
    }

    /// Every distinct word the pattern can produce (sounds, scenes, …).
    pub fn words(&self) -> Vec<String> {
        fn walk(p: &Pat, out: &mut Vec<String>) {
            match p {
                Pat::Atom(Value::Text(s)) => {
                    if !out.contains(s) {
                        out.push(s.clone())
                    }
                }
                Pat::Silence | Pat::Atom(_) | Pat::Signal(_) => {}
                Pat::Seq(s) => s.iter().for_each(|s| walk(&s.pat, out)),
                Pat::Alt(ps) | Pat::Stack(ps) | Pat::Choose { pats: ps, .. } => {
                    ps.iter().for_each(|p| walk(p, out))
                }
                Pat::Poly { pats, .. } => pats.iter().flatten().for_each(|p| walk(p, out)),
                Pat::Apply { pat, transform } => {
                    if let Transform::Set(Value::Text(s)) = transform
                        && !out.contains(s)
                    {
                        out.push(s.clone());
                    }
                    walk(pat, out)
                }
            }
        }
        let mut out = Vec::new();
        walk(self, &mut out);
        out
    }
}

/// Events a pattern plays in `[from, from + cycles)`, as (onset, value):
/// for inspection, tests and AI comparison.
pub fn sample(p: &Pat, from: i64, cycles: i64, seed: u64) -> Vec<(Time, Value)> {
    compile(p)
        .onsets(Span::cycles(from, from + cycles), seed)
        .into_iter()
        .map(|h| (h.part.begin, h.value))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ast_round_trips_through_json() {
        let p = Pat::seq(vec![
            Pat::Atom(Value::atom("bd")),
            Pat::Alt(vec![Pat::Atom(Value::atom("sd")), Pat::Silence]),
        ])
        .apply(Transform::Every {
            n: 4,
            offset: 0,
            then: Box::new(Transform::Fast(Time::whole(2))),
        });
        let j = serde_json::to_string(&p).unwrap();
        let back: Pat = serde_json::from_str(&j).unwrap();
        assert_eq!(p, back);
        assert_eq!(sample(&p, 0, 8, 1), sample(&back, 0, 8, 1));
    }

    #[test]
    fn numbers_transform_and_words_dont() {
        let p = Pat::seq(vec![
            Pat::Atom(Value::Num(0.5)),
            Pat::Atom(Value::atom("x")),
        ])
        .apply(Transform::Range {
            lo: 100.0,
            hi: 200.0,
        });
        let v: Vec<_> = sample(&p, 0, 1, 0).into_iter().map(|x| x.1).collect();
        assert_eq!(v, [Value::Num(150.0), Value::atom("x")]);
    }

    #[test]
    fn polymeter_steps_through_its_own_length() {
        let p = Pat::Poly {
            pats: vec![vec![
                Pat::Atom(Value::Num(1.0)),
                Pat::Atom(Value::Num(2.0)),
                Pat::Atom(Value::Num(3.0)),
            ]],
            steps: Some(Time::whole(4)),
        };
        let v: Vec<_> = sample(&p, 0, 1, 0).into_iter().map(|x| x.1).collect();
        assert_eq!(
            v,
            [
                Value::Num(1.0),
                Value::Num(2.0),
                Value::Num(3.0),
                Value::Num(1.0)
            ]
        );
    }
}
