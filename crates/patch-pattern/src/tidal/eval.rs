//! Tidal expressions evaluated into patterns.
//!
//! A value is a number, a pattern (of plain values, or of controls: the
//! `s "bd" # gain 1.2` kind every channel plays), a function, a list or a
//! tuple. Functions curry, compose (`.`) and come in sections (`(# speed
//! 2)`), so live code reads the way TidalCycles is written. Every pattern
//! is Patch's own [`Pattern`]; the meaning of each word follows Tidal's
//! documented behaviour, written for Patch (nothing is taken from Tidal's
//! source).

use super::notes;
use super::parse::{Expr, Node, is_structure_op};
use crate::ast::Value;
use crate::pattern::{Hap, Pattern, Query, rand_at};
use crate::time::{Span, Time};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::Arc;

/// A control pattern's value: named controls (`s`, `n`, `gain`, …).
pub type Ctl = BTreeMap<String, Value>;

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    /// A plain value: a word or a number (`"bd sn"`, `"0 .. 7"`), and the
    /// words of the code it came from.
    S(Value, Loc),
    /// Controls (what `s`, `n`, `gain` … make), and where they were written.
    C(Ctl, Loc),
}

impl Val {
    /// A value written nowhere in particular (a number argument, a signal).
    pub fn s(v: Value) -> Val {
        Val::S(v, Loc::NONE)
    }

    /// The words of the code this value came from.
    pub fn loc(&self) -> Loc {
        match self {
            Val::S(_, l) | Val::C(_, l) => *l,
        }
    }
}

impl From<Val> for bool {
    fn from(v: Val) -> bool {
        match v {
            Val::S(v, _) => v.truthy() && v.text() != Some("~"),
            Val::C(..) => true,
        }
    }
}

/// Where in the code a value was written: the mini-notation words an event
/// comes from (`"bd"` in `s "bd*2"`, and `"0"` in `# n "0 3"` too), so an
/// editor can light them up while the event plays. Up to [`Loc::MAX`]
/// ranges, each (source, from, to): a byte range in source `source`, which
/// the evaluator is told with [`with_source`] (a channel's code, a `let`'s).
///
/// Places are an annotation, not part of a value: two values equal in all
/// else are equal wherever they were written.
#[derive(Clone, Copy, Debug, Default)]
pub struct Loc {
    n: u8,
    at: [(u16, u16, u16); Loc::MAX],
}

impl PartialEq for Loc {
    fn eq(&self, _: &Loc) -> bool {
        true
    }
}

impl Loc {
    pub const MAX: usize = 6;
    pub const NONE: Loc = Loc {
        n: 0,
        at: [(0, 0, 0); Loc::MAX],
    };

    /// One range; none when it doesn't fit (code past 64 KiB).
    pub fn one(source: u16, from: usize, to: usize) -> Loc {
        let mut l = Loc::NONE;
        if let (Ok(a), Ok(b)) = (u16::try_from(from), u16::try_from(to)) {
            l.at[0] = (source, a, b);
            l.n = 1;
        }
        l
    }

    pub fn ranges(&self) -> &[(u16, u16, u16)] {
        &self.at[..self.n as usize]
    }

    /// Both places (each range once, the first [`Loc::MAX`]).
    pub fn union(self, other: Loc) -> Loc {
        let mut out = self;
        for r in other.ranges() {
            if (out.n as usize) < Loc::MAX && !out.ranges().contains(r) {
                out.at[out.n as usize] = *r;
                out.n += 1;
            }
        }
        out
    }
}

thread_local! {
    static SOURCE: Cell<u16> = const { Cell::new(0) };
}

/// Evaluate with the code's places counted as source `source` (see
/// [`Loc`]): a session evaluates each channel and `let` as its own source.
pub fn with_source<T>(source: u16, f: impl FnOnce() -> T) -> T {
    let old = SOURCE.replace(source);
    let r = f();
    SOURCE.set(old);
    r
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Plain,
    Control,
}

#[derive(Clone)]
pub enum V {
    Num(f64),
    Pat(Kind, Pattern<Val>),
    Fun(Func),
    List(Vec<V>),
    Tuple(Vec<V>),
}

#[derive(Clone)]
pub struct Func {
    kind: FnKind,
    /// Where it was written (for errors).
    at: usize,
}

#[derive(Clone)]
enum FnKind {
    /// A word of the vocabulary with the arguments it has so far.
    Word {
        name: &'static str,
        arity: usize,
        args: Vec<V>,
    },
    /// `f . g`
    Compose(Box<Func>, Box<Func>),
    /// A function the code defines (`let f x = …`) and the arguments it
    /// has so far.
    Lambda { lam: Arc<Lambda>, args: Vec<V> },
    /// An operator with one side (a section) or none (`(+)`).
    Op {
        op: String,
        left: Option<Box<V>>,
        right: Option<Box<V>>,
    },
}

/// `let name args = body`: evaluated when the last argument comes, with
/// the `let`s before it and its arguments bound.
pub struct Lambda {
    pub params: Vec<String>,
    pub body: Node,
    pub env: Env,
    /// The source its words count in (see [`Loc`]).
    pub source: u16,
}

impl Func {
    /// A function the code defines.
    pub fn lambda(params: Vec<String>, body: Node, env: Env, at: usize) -> Func {
        Func {
            kind: FnKind::Lambda {
                lam: Arc::new(Lambda {
                    params,
                    body,
                    env,
                    source: SOURCE.get(),
                }),
                args: vec![],
            },
            at,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EvalError {
    pub at: usize,
    pub message: String,
}

type R<T> = Result<T, EvalError>;

fn err<T>(at: usize, message: impl Into<String>) -> R<T> {
    Err(EvalError {
        at,
        message: message.into(),
    })
}

/// What a name is bound to: the vocabulary and the `let`s before it.
#[derive(Clone, Default)]
pub struct Env {
    lets: BTreeMap<String, V>,
}

impl Env {
    pub fn bind(&mut self, name: &str, v: V) {
        self.lets.insert(name.to_string(), v);
    }

    pub fn has(&self, name: &str) -> bool {
        self.lets.contains_key(name)
    }
}

impl V {
    pub fn kind_name(&self) -> &'static str {
        match self {
            V::Num(_) => "a number",
            V::Pat(Kind::Plain, _) => "a pattern of values",
            V::Pat(Kind::Control, _) => "a control pattern",
            V::Fun(_) => "a function",
            V::List(_) => "a list",
            V::Tuple(_) => "a pair",
        }
    }
}

// ── Patterns: building blocks Patch's algebra doesn't have ──────────────

fn q(span: Span, seed: u64) -> Query {
    Query { span, seed }
}

fn shift_hap(h: Hap<Val>, by: Time) -> Hap<Val> {
    Hap {
        whole: h.whole.map(|w| w.with_time(|t| t + by)),
        part: h.part.with_time(|t| t + by),
        value: h.value,
    }
}

/// The left side's structure (its events), each with the right side's
/// value at the same time: what `#`, `|+` and `<*` do.
fn app_left(l: &Pattern<Val>, r: &Pattern<Val>, f: Arc<dyn Fn(&Val, &Val) -> Val + Send + Sync>) -> Pattern<Val> {
    let (l, r) = (l.clone(), r.clone());
    Pattern::new(move |qq| {
        let mut out = Vec::new();
        for a in l.query(qq) {
            let over = a.whole.unwrap_or(a.part);
            for b in r.query(q(over, qq.seed)) {
                if let Some(part) = a.part.intersect(b.part) {
                    out.push(Hap {
                        whole: a.whole,
                        part,
                        value: f(&a.value, &b.value),
                    });
                }
            }
        }
        out
    })
}

/// Both sides' structure: events where both have one (`|+|`, `+`).
fn app_both(l: &Pattern<Val>, r: &Pattern<Val>, f: Arc<dyn Fn(&Val, &Val) -> Val + Send + Sync>) -> Pattern<Val> {
    let (l, r) = (l.clone(), r.clone());
    Pattern::new(move |qq| {
        let rs = r.query(qq);
        let mut out = Vec::new();
        for a in l.query(qq) {
            for b in &rs {
                let Some(part) = a.part.intersect(b.part) else { continue };
                let whole = match (a.whole, b.whole) {
                    (Some(x), Some(y)) => match x.intersect(y) {
                        Some(w) => Some(w),
                        None => continue,
                    },
                    (Some(x), None) | (None, Some(x)) => Some(x),
                    (None, None) => None,
                };
                out.push(Hap {
                    whole,
                    part,
                    value: f(&a.value, &b.value),
                });
            }
        }
        out
    })
}

/// A pattern whose shape depends on a patterned argument: each of the
/// argument's events picks the pattern played during it.
fn inner_join(arg: &Pattern<Val>, f: Arc<dyn Fn(&Value) -> Option<Pattern<Val>> + Send + Sync>) -> Pattern<Val> {
    let arg = arg.clone();
    Pattern::new(move |qq| {
        let mut out = Vec::new();
        for a in arg.query(qq) {
            let Val::S(v, _) = &a.value else { continue };
            if let Some(p) = f(v) {
                out.extend(p.query(q(a.part, qq.seed)));
            }
        }
        out
    })
}

/// Values one event becomes several of (a chord's notes), same timing.
fn flat_map(p: &Pattern<Val>, f: Arc<dyn Fn(&Val) -> Vec<Val> + Send + Sync>) -> Pattern<Val> {
    let p = p.clone();
    Pattern::new(move |qq| {
        p.query(qq)
            .into_iter()
            .flat_map(|h| {
                f(&h.value).into_iter().map(move |value| Hap {
                    whole: h.whole,
                    part: h.part,
                    value,
                })
            })
            .collect()
    })
}

/// Cycle `c` of the result plays `[c + s, c + s + d)` of `p` stretched over
/// the cycle (`zoom`).
fn zoom_cycle(p: &Pattern<Val>, s: Time, d: Time) -> Pattern<Val> {
    if d <= Time::ZERO {
        return Pattern::silence();
    }
    let p = p.clone();
    Pattern::new(move |qq| {
        let mut out = Vec::new();
        for span in qq.span.split_cycles() {
            let c = span.begin.sam();
            let to_src = |t: Time| c + s + (t - c) * d;
            let to_dst = |t: Time| c + (t - c - s) / d;
            for h in p.query(q(span.with_time(to_src), qq.seed)) {
                out.push(Hap {
                    whole: h.whole.map(|w| w.with_time(to_dst)),
                    part: h.part.with_time(to_dst),
                    value: h.value,
                });
            }
        }
        out
    })
}

/// Each cycle cut into `n` slices, slice `pick(cycle, slot)` played in
/// slot `slot` (`shuffle`, `scramble`, `bite`'s cousin).
fn reslice(p: &Pattern<Val>, n: u32, pick: Arc<dyn Fn(u64, i64, i64) -> i64 + Send + Sync>) -> Pattern<Val> {
    let n = n.max(1) as i64;
    let p = p.clone();
    Pattern::new(move |qq| {
        let mut out = Vec::new();
        for span in qq.span.split_cycles() {
            let c = span.begin.sam();
            for slot in 0..n {
                let dst = Span::new(c + Time::new(slot, n), c + Time::new(slot + 1, n));
                let Some(want) = span.intersect(dst) else { continue };
                let src = pick(qq.seed, c.cycle(), slot).rem_euclid(n);
                let by = Time::new(slot - src, n);
                for h in p.query(q(want.with_time(|t| t - by), qq.seed)) {
                    out.push(shift_hap(h, by));
                }
            }
        }
        out
    })
}

fn signal(f: impl Fn(Time, u64) -> f64 + Send + Sync + 'static) -> Pattern<Val> {
    Pattern::signal(move |t, seed| Val::s(Value::Num(f(t, seed))))
}

/// Smooth noise: random values on whole cycles, eased between.
fn perlin_at(t: Time, seed: u64) -> f64 {
    let x = t.to_f64();
    let a = x.floor();
    let frac = x - a;
    let r = |c: f64| rand_at(seed, Time::whole(c as i64), 0x9e21);
    let smooth = 6.0 * frac.powi(5) - 15.0 * frac.powi(4) + 10.0 * frac.powi(3);
    r(a) + smooth * (r(a + 1.0) - r(a))
}

// ── Values ──────────────────────────────────────────────────────────────

/// A number from a value: numbers, and note names (`"c e g"`).
pub fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Num(n) => Some(*n),
        Value::Text(s) => notes::note_name(s),
    }
}

fn arith(op: char, a: &Value, b: &Value) -> Value {
    if let (Some(x), Some(y)) = (number(a), number(b)) {
        let r = match op {
            '+' => x + y,
            '-' => x - y,
            '*' => x * y,
            '/' => {
                if y == 0.0 {
                    0.0
                } else {
                    x / y
                }
            }
            '%' => {
                if y == 0.0 {
                    0.0
                } else {
                    x.rem_euclid(y)
                }
            }
            '<' => x,
            _ => y,
        };
        return Value::Num(r);
    }
    match (op, a, b) {
        ('<', a, _) => a.clone(),
        ('>', _, b) => b.clone(),
        // Words add up (`s "drum" |+ s ":3"`); other ops keep the left.
        ('+', Value::Text(x), Value::Text(y)) => Value::Text(format!("{x}{y}")),
        (_, a, _) => a.clone(),
    }
}

/// Two values combined by `op` (`+ - * / %`, `<` keep left, `>` keep right):
/// controls key by key, keeping keys only one side has.
fn combine(op: char, a: &Val, b: &Val) -> Val {
    match (a, b) {
        (Val::S(x, lx), Val::S(y, ly)) => Val::S(arith(op, x, y), lx.union(*ly)),
        (Val::C(x, lx), Val::C(y, ly)) => {
            let mut out = x.clone();
            for (k, v) in y {
                match out.get(k) {
                    Some(old) => {
                        let nv = arith(op, old, v);
                        out.insert(k.clone(), nv);
                    }
                    None => {
                        out.insert(k.clone(), v.clone());
                    }
                }
            }
            Val::C(out, lx.union(*ly))
        }
        (a, b) => {
            if op == '>' {
                b.clone()
            } else {
                a.clone()
            }
        }
    }
}

fn op_fn(op: char) -> Arc<dyn Fn(&Val, &Val) -> Val + Send + Sync> {
    Arc::new(move |a, b| combine(op, a, b))
}

fn time(x: f64) -> Time {
    Time::approx(x, 1 << 12)
}

// ── Mini-notation ───────────────────────────────────────────────────────

/// A word of a string after [`expand_ranges`]: where it is in the
/// expanded text (`exp`, `len`) and in the string as written (`orig`,
/// `orig_len`). A range's numbers (`made`) all point at the whole `0 .. 7`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Piece {
    exp: usize,
    len: usize,
    orig: usize,
    orig_len: usize,
    made: bool,
}

/// `"0 .. 3"` → `"0 1 2 3"` (Tidal's ranges), before the shared parser;
/// with each word's place, to find what was written from the expanded text.
fn expand_ranges(s: &str) -> (String, Vec<Piece>) {
    let mut words: Vec<(&str, usize)> = Vec::new();
    let mut pos = 0;
    for w in s.split(' ') {
        words.push((w, pos));
        pos += w.len() + 1;
    }
    // (text, where written, its length, made by a range)
    let mut out: Vec<(String, usize, usize, bool)> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        if words[i].0 == ".." && i > 0 && i + 1 < words.len() {
            let a = out.last().and_then(|w| w.0.parse::<i64>().ok());
            let b = words[i + 1].0.parse::<i64>().ok();
            if let (Some(a), Some(b)) = (a, b)
                && (a - b).abs() <= 256
            {
                let first = out.len() - 1;
                let start = out[first].1;
                let len = words[i + 1].1 + words[i + 1].0.len() - start;
                out[first].2 = len;
                out[first].3 = true;
                let step = if b >= a { 1 } else { -1 };
                let mut k = a + step;
                while (step > 0 && k <= b) || (step < 0 && k >= b) {
                    out.push((k.to_string(), start, len, true));
                    k += step;
                }
                i += 2;
                continue;
            }
        }
        out.push((words[i].0.to_string(), words[i].1, words[i].0.len(), false));
        i += 1;
    }
    let mut text = String::new();
    let mut pieces = Vec::with_capacity(out.len());
    for (k, (w, orig, orig_len, made)) in out.into_iter().enumerate() {
        if k > 0 {
            text.push(' ');
        }
        pieces.push(Piece {
            exp: text.len(),
            len: w.len(),
            orig,
            orig_len,
            made,
        });
        text.push_str(&w);
    }
    (text, pieces)
}

/// Where the expanded text's bytes `a..b` (one word or less) were written.
fn written(pieces: &[Piece], a: usize, b: usize) -> (usize, usize) {
    match pieces.iter().rev().find(|p| p.exp <= a) {
        Some(p) if p.made => (p.orig, p.orig + p.orig_len),
        Some(p) => {
            let at = |x: usize| p.orig + x.saturating_sub(p.exp).min(p.orig_len);
            (at(a), at(b.max(a)))
        }
        None => (a, b),
    }
}

/// Tidal rotates Euclidean patterns earlier (`bd(3,8,2)`); the shared
/// mini-notation follows Strudel (later).
fn tidal_rotation(p: crate::ast::Pat) -> crate::ast::Pat {
    use crate::ast::{Pat, Step, Transform};
    fn tr(t: Transform) -> Transform {
        match t {
            Transform::Euclid {
                pulses,
                steps,
                rotate,
            } => Transform::Euclid {
                pulses,
                steps,
                rotate: -rotate,
            },
            other => other,
        }
    }
    match p {
        Pat::Seq(s) => Pat::Seq(
            s.into_iter()
                .map(|s| Step {
                    pat: tidal_rotation(s.pat),
                    weight: s.weight,
                })
                .collect(),
        ),
        Pat::Alt(v) => Pat::Alt(v.into_iter().map(tidal_rotation).collect()),
        Pat::Stack(v) => Pat::Stack(v.into_iter().map(tidal_rotation).collect()),
        Pat::Choose { pats, seed } => Pat::Choose {
            pats: pats.into_iter().map(tidal_rotation).collect(),
            seed,
        },
        Pat::Poly { pats, steps } => Pat::Poly {
            pats: pats
                .into_iter()
                .map(|v| v.into_iter().map(tidal_rotation).collect())
                .collect(),
            steps,
        },
        Pat::Apply { transform, pat } => Pat::Apply {
            transform: tr(transform),
            pat: Box::new(tidal_rotation(*pat)),
        },
        other => other,
    }
}

/// A string literal at `at` (its opening quote) as a pattern of values,
/// each knowing the word it was written as (see [`Loc`]).
pub fn mini(s: &str, at: usize) -> R<Pattern<Val>> {
    let (src, pieces) = expand_ranges(s);
    // `?` and `|` draw from a seed per string position: two strings differ.
    let (pat, atoms) = crate::mini::parse_located(&src, at as u64 * 7919).map_err(|e| EvalError {
        // Inside the string: one past its opening quote.
        at: at + 1 + written(&pieces, e.at, e.at).0.min(s.len()),
        message: e.message,
    })?;
    let source = SOURCE.get();
    let table: Arc<[(Value, Loc)]> = atoms
        .into_iter()
        .map(|(v, a, b)| {
            let (a, b) = written(&pieces, a, b);
            (v, Loc::one(source, at + 1 + a, at + 1 + b))
        })
        .collect();
    // The parsed atoms are indices into the table (`parse_located`).
    Ok(crate::ast::compile(&tidal_rotation(pat)).fmap(move |i| {
        match i.num().and_then(|i| table.get(i as usize)) {
            Some((v, l)) => Val::S(v.clone(), *l),
            None => Val::s(i),
        }
    }))
}

// ── Coercions ───────────────────────────────────────────────────────────

fn pat_of(v: V, at: usize) -> R<(Kind, Pattern<Val>)> {
    match v {
        V::Num(x) => Ok((Kind::Plain, Pattern::pure(Val::s(Value::Num(x))))),
        V::Pat(k, p) => Ok((k, p)),
        other => err(at, format!("expected a pattern, got {}", other.kind_name())),
    }
}

fn control(v: V, at: usize, what: &str) -> R<Pattern<Val>> {
    match v {
        V::Pat(Kind::Control, p) => Ok(p),
        V::Pat(Kind::Plain, _) | V::Num(_) => err(
            at,
            format!("{what} needs a control pattern (like s \"bd\" or n \"0 2\"), not plain values"),
        ),
        other => err(at, format!("{what} needs a control pattern, got {}", other.kind_name())),
    }
}

fn func(v: V, at: usize) -> R<Func> {
    match v {
        V::Fun(f) => Ok(f),
        other => err(
            at,
            format!("expected a function (like (fast 2) or (# speed 2)), got {}", other.kind_name()),
        ),
    }
}

fn list(v: V, at: usize) -> R<Vec<V>> {
    match v {
        V::List(l) => Ok(l),
        other => err(at, format!("expected a list [ … ], got {}", other.kind_name())),
    }
}

fn pair(v: V, at: usize) -> R<(f64, f64)> {
    match v {
        V::Tuple(t) if t.len() == 2 => match (&t[0], &t[1]) {
            (V::Num(a), V::Num(b)) => Ok((*a, *b)),
            _ => err(at, "expected two numbers, like (0.25, 0.75)"),
        },
        other => err(at, format!("expected a pair like (0.25, 0.75), got {}", other.kind_name())),
    }
}

fn konst(v: &V, at: usize, what: &str) -> R<f64> {
    match v {
        V::Num(x) => Ok(*x),
        _ => err(at, format!("{what} must be a number here")),
    }
}

/// A numeric argument that may be patterned (`fast "<1 2>"`): the pattern
/// `f` builds for each of its values.
fn tparam(arg: V, at: usize, f: impl Fn(f64) -> Pattern<Val> + Send + Sync + 'static) -> R<Pattern<Val>> {
    match arg {
        V::Num(x) => Ok(f(x)),
        V::Pat(Kind::Plain, p) => Ok(inner_join(&p, Arc::new(move |v| number(v).map(&f)))),
        other => err(at, format!("expected a number or a pattern of numbers, got {}", other.kind_name())),
    }
}

/// Like [`tparam`], for words (`arp "<up down>"`).
fn wparam(arg: V, at: usize, f: impl Fn(&str) -> Option<Pattern<Val>> + Send + Sync + 'static) -> R<Pattern<Val>> {
    match arg {
        V::Pat(Kind::Plain, p) => Ok(inner_join(&p, Arc::new(move |v| f(&v.to_string())))),
        other => err(at, format!("expected a pattern of words, got {}", other.kind_name())),
    }
}

// ── Applying functions ──────────────────────────────────────────────────

pub fn apply(f: &Func, arg: V, at: usize) -> R<V> {
    match &f.kind {
        FnKind::Word { name, arity, args } => {
            let mut args = args.clone();
            args.push(arg);
            if args.len() == *arity {
                call(name, args, f.at)
            } else {
                Ok(V::Fun(Func {
                    kind: FnKind::Word {
                        name,
                        arity: *arity,
                        args,
                    },
                    at: f.at,
                }))
            }
        }
        FnKind::Compose(a, b) => {
            let mid = apply(b, arg, at)?;
            apply(a, mid, at)
        }
        FnKind::Lambda { lam, args } => {
            let mut args = args.clone();
            args.push(arg);
            if args.len() < lam.params.len() {
                return Ok(V::Fun(Func {
                    kind: FnKind::Lambda { lam: lam.clone(), args },
                    at: f.at,
                }));
            }
            let mut env = lam.env.clone();
            for (p, v) in lam.params.iter().zip(args) {
                env.bind(p, v);
            }
            with_source(lam.source, || eval(&lam.body, &env))
        }
        FnKind::Op { op, left, right } => match (left, right) {
            (None, Some(r)) => binop(op, arg, (**r).clone(), f.at),
            (Some(l), None) => binop(op, (**l).clone(), arg, f.at),
            (None, None) => Ok(V::Fun(Func {
                kind: FnKind::Op {
                    op: op.clone(),
                    left: Some(Box::new(arg)),
                    right: None,
                },
                at: f.at,
            })),
            (Some(_), Some(_)) => err(at, "too many arguments for an operator"),
        },
    }
}

/// `f` applied to a pattern, which must stay the same kind (a function
/// given to `every`, `sometimes`, `jux` …).
fn transform(f: &Func, k: Kind, p: Pattern<Val>, at: usize) -> R<Pattern<Val>> {
    match apply(f, V::Pat(k, p), at)? {
        V::Pat(k2, p2) if k2 == k => Ok(p2),
        V::Pat(..) => err(at, "this function turns the pattern into another kind"),
        other => err(
            at,
            format!("this function should give a pattern back, it gave {}", other.kind_name()),
        ),
    }
}

pub fn binop(op: &str, a: V, b: V, at: usize) -> R<V> {
    if let Some(name) = op.strip_prefix('`').and_then(|o| o.strip_suffix('`')) {
        let f = word(name, at)?;
        let f = func(f, at)?;
        let g = apply(&f, a, at)?;
        return apply(&func(g, at)?, b, at);
    }
    match op {
        "$" => apply(&func(a, at)?, b, at),
        "." => {
            let f = func(a, at)?;
            let g = func(b, at)?;
            Ok(V::Fun(Func {
                kind: FnKind::Compose(Box::new(f), Box::new(g)),
                at,
            }))
        }
        "+" | "-" | "*" | "/" => {
            let c = op.chars().next().unwrap();
            if let (V::Num(x), V::Num(y)) = (&a, &b) {
                return match arith(c, &Value::Num(*x), &Value::Num(*y)) {
                    Value::Num(n) => Ok(V::Num(n)),
                    _ => unreachable!(),
                };
            }
            let (ka, pa) = pat_of(a, at)?;
            let (kb, pb) = pat_of(b, at)?;
            if ka != kb {
                return err(at, format!("'{op}' needs both sides the same kind: use |{op} to add to controls (|{op} n 12)"));
            }
            Ok(V::Pat(ka, app_both(&pa, &pb, op_fn(c))))
        }
        "<~" | "~>" => {
            let (k, p) = pat_of(b, at)?;
            let early = op == "<~";
            let shifted = tparam(a, at, move |x| {
                let t = time(x);
                if early { p.early(t) } else { p.late(t) }
            })?;
            Ok(V::Pat(k, shifted))
        }
        "<>" => {
            let (ka, pa) = pat_of(a, at)?;
            let (kb, pb) = pat_of(b, at)?;
            if ka != kb {
                return err(at, "'<>' overlays two patterns of the same kind");
            }
            Ok(V::Pat(ka, Pattern::stack(vec![pa, pb])))
        }
        o if o == "#" || is_structure_op(o) => {
            let (ka, pa) = pat_of(a, at)?;
            let (kb, pb) = pat_of(b, at)?;
            if ka != kb {
                return err(
                    at,
                    format!("'{o}' joins two patterns of the same kind: both controls (s \"bd\" {o} gain 1.2) or both values"),
                );
            }
            let (left, right, c) = if o == "#" {
                (true, false, '>')
            } else {
                let core = o.trim_matches('|').chars().next().unwrap();
                (o.starts_with('|'), o.ends_with('|'), core)
            };
            let f = op_fn(c);
            Ok(V::Pat(
                ka,
                match (left, right) {
                    (true, true) => app_both(&pa, &pb, f),
                    (true, false) => app_left(&pa, &pb, f),
                    _ => {
                        let g: Arc<dyn Fn(&Val, &Val) -> Val + Send + Sync> =
                            Arc::new(move |b, a| f(a, b));
                        app_left(&pb, &pa, g)
                    }
                },
            ))
        }
        other => err(at, format!("'{other}' isn't an operator Patch's Tidal knows")),
    }
}

// ── Evaluation ──────────────────────────────────────────────────────────

pub fn eval(n: &Node, env: &Env) -> R<V> {
    let at = n.at;
    match &n.expr {
        Expr::Num(x) => Ok(V::Num(*x)),
        Expr::Str(s) => Ok(V::Pat(Kind::Plain, mini(s, at)?)),
        Expr::Var(name) => match env.lets.get(name) {
            Some(v) => Ok(v.clone()),
            None => word(name, at),
        },
        Expr::App(f, a) => {
            let fv = eval(f, env)?;
            let av = eval(a, env)?;
            match fv {
                V::Fun(func) => apply(&func, av, a.at),
                other => err(
                    f.at,
                    format!("{} isn't a function, it can't take an argument here", other.kind_name()),
                ),
            }
        }
        Expr::Bin(op, a, b) => {
            let av = eval(a, env)?;
            let bv = eval(b, env)?;
            binop(op, av, bv, at)
        }
        Expr::SectionL(a, op) => Ok(V::Fun(Func {
            kind: FnKind::Op {
                op: op.clone(),
                left: Some(Box::new(eval(a, env)?)),
                right: None,
            },
            at,
        })),
        Expr::SectionR(op, b) => {
            let bv = eval(b, env)?;
            Ok(V::Fun(Func {
                kind: FnKind::Op {
                    op: op.clone(),
                    left: None,
                    right: Some(Box::new(bv)),
                },
                at,
            }))
        }
        Expr::OpFn(op) => Ok(V::Fun(Func {
            kind: FnKind::Op {
                op: op.clone(),
                left: None,
                right: None,
            },
            at,
        })),
        Expr::List(items) => Ok(V::List(
            items.iter().map(|i| eval(i, env)).collect::<R<Vec<_>>>()?,
        )),
        Expr::Tuple(items) => Ok(V::Tuple(
            items.iter().map(|i| eval(i, env)).collect::<R<Vec<_>>>()?,
        )),
        Expr::Lambda(params, body) => Ok(V::Fun(Func::lambda(params.clone(), (**body).clone(), env.clone(), at))),
        Expr::Neg(e) => match eval(e, env)? {
            V::Num(x) => Ok(V::Num(-x)),
            V::Pat(Kind::Plain, p) => Ok(V::Pat(
                Kind::Plain,
                p.fmap(|v| match v {
                    Val::S(x, l) => Val::S(arith('*', &x, &Value::Num(-1.0)), l),
                    other => other,
                }),
            )),
            other => err(at, format!("can't negate {}", other.kind_name())),
        },
    }
}

/// Every word, its argument count, and what it does: the vocabulary.
pub const WORDS: &[(&str, usize)] = &[
    // Constants and signals.
    ("silence", 0),
    ("sine", 0),
    ("cosine", 0),
    ("saw", 0),
    ("isaw", 0),
    ("tri", 0),
    ("square", 0),
    ("rand", 0),
    ("perlin", 0),
    ("id", 1),
    ("irand", 1),
    ("run", 1),
    ("scan", 1),
    ("choose", 1),
    ("cycleChoose", 1),
    // Building patterns.
    ("cat", 1),
    ("slowcat", 1),
    ("fastcat", 1),
    ("fastCat", 1),
    ("timeCat", 1),
    ("timecat", 1),
    ("randcat", 1),
    ("stack", 1),
    ("overlay", 2),
    ("append", 2),
    ("slowAppend", 2),
    ("slowappend", 2),
    ("fastAppend", 2),
    ("fastappend", 2),
    // Time.
    ("fast", 2),
    ("density", 2),
    ("slow", 2),
    ("sparsity", 2),
    ("hurry", 2),
    ("rev", 1),
    ("palindrome", 1),
    ("iter", 2),
    ("iter'", 2),
    ("ply", 2),
    ("segment", 2),
    ("discretise", 2),
    ("range", 3),
    ("rangex", 3),
    ("quantise", 1),
    ("toScale", 2),
    ("scale", 2),
    ("arp", 2),
    ("arpeggiate", 1),
    ("rot", 2),
    ("brak", 1),
    ("press", 1),
    ("pressBy", 2),
    ("swingBy", 3),
    ("swing", 2),
    ("inside", 3),
    ("outside", 3),
    ("linger", 2),
    ("trunc", 2),
    ("zoom", 2),
    ("compress", 2),
    ("fastGap", 2),
    ("fastgap", 2),
    ("shuffle", 2),
    ("scramble", 2),
    ("rolled", 1),
    ("rolledBy", 2),
    ("plyWith", 3),
    ("bite", 3),
    ("squeeze", 2),
    ("wedge", 3),
    ("soak", 3),
    ("chew", 3),
    ("fit", 3),
    // A channel changing over (written by xfade, clutch, jumpIn, once).
    ("_xfade", 4),
    ("_clutch", 4),
    ("_jump", 3),
    ("_once", 2),
    // Samples: cutting them up.
    ("chop", 2),
    ("striate", 2),
    ("striateBy", 3),
    ("slice", 3),
    ("randslice", 2),
    ("splice", 3),
    ("loopAt", 2),
    // Conditions.
    ("every", 3),
    ("every'", 4),
    ("whenmod", 4),
    ("foldEvery", 3),
    ("within", 3),
    ("within'", 3),
    ("chunk", 3),
    ("chunk'", 3),
    ("while", 3),
    ("sometimesBy", 3),
    ("sometimes", 2),
    ("often", 2),
    ("rarely", 2),
    ("almostNever", 2),
    ("almostAlways", 2),
    ("always", 2),
    ("never", 2),
    ("someCyclesBy", 3),
    ("somecyclesBy", 3),
    ("someCycles", 2),
    ("somecycles", 2),
    ("degrade", 1),
    ("degradeBy", 2),
    ("unDegradeBy", 2),
    // Structure.
    ("struct", 2),
    ("mask", 2),
    ("sew", 3),
    ("stitch", 3),
    ("inv", 1),
    ("euclid", 3),
    ("euclidInv", 3),
    ("euclidFull", 4),
    ("euclidOff", 4),
    ("eoff", 4),
    // Layers.
    ("superimpose", 2),
    ("off", 3),
    ("layer", 2),
    ("jux", 2),
    ("juxBy", 3),
    ("stut", 4),
    ("echo", 4),
    ("stutWith", 4),
    ("echoWith", 4),
    ("stutter", 3),
    ("ghost", 1),
    ("select", 2),
    ("pickF", 3),
    ("fix", 3),
    ("unfix", 3),
    ("contrast", 4),
    ("euclidOffFull", 6),
    ("wchoose", 1),
    ("chooseBy", 2),
    ("wrandcat", 1),
    ("binary", 1),
    ("binaryN", 2),
    ("brand", 0),
    ("brandBy", 1),
    ("envL", 0),
    ("envR", 0),
    ("envLR", 0),
    ("envRR", 0),
    ("fadeIn", 2),
    ("fadeOut", 2),
    ("spread", 3),
    ("slowspread", 3),
    ("fastspread", 3),
];

/// Controls: what an event asks of the instrument.
pub const CONTROLS: &[&str] = &[
    "s", "sound", "n", "note", "up", "gain", "amp", "velocity", "pan", "speed", "shape", "crush",
    "coarse", "cutoff", "lpf", "ctf", "resonance", "lpq", "res", "hcutoff", "hpf", "hresonance",
    "hpq", "attack", "att", "release", "rel", "sustain", "sus", "hold", "legato", "room", "size",
    "sz", "dry", "delay", "delaytime", "delayt", "delayfeedback", "delayfb", "octave", "orbit",
    "cut", "unit", "begin", "end", "vowel", "accelerate", "loop", "channel", "squiz", "lock",
    "detune", "voice", "nudge", "lpenv", "midinote",
];

/// The name a control is stored under (aliases folded: `lpf` → `cutoff`).
pub fn control_name(name: &str) -> &str {
    match name {
        "sound" => "s",
        "up" => "note",
        "lpf" | "ctf" => "cutoff",
        "lpq" | "res" => "resonance",
        "hpf" => "hcutoff",
        "hpq" => "hresonance",
        "att" => "attack",
        "rel" => "release",
        "sus" => "sustain",
        "sz" => "size",
        "delayt" => "delaytime",
        "delayfb" => "delayfeedback",
        other => other,
    }
}

fn text_control(name: &str) -> bool {
    matches!(name, "s" | "vowel" | "unit")
}

/// A name as a value: a let, a word of the vocabulary, a control.
pub fn word(name: &str, at: usize) -> R<V> {
    if let Some(&(w, arity)) = WORDS.iter().find(|(w, _)| *w == name) {
        if arity == 0 {
            return call(w, vec![], at);
        }
        return Ok(V::Fun(Func {
            kind: FnKind::Word {
                name: w,
                arity,
                args: vec![],
            },
            at,
        }));
    }
    if let Some(&c) = CONTROLS.iter().find(|c| **c == name) {
        return Ok(V::Fun(Func {
            kind: FnKind::Word {
                name: c,
                arity: 1,
                args: vec![],
            },
            at,
        }));
    }
    let hint = suggest(name)
        .map(|s| format!(" (did you mean {s}?)"))
        .unwrap_or_default();
    let why = match name {
        "ur" | "nTake" => {
            " — not in Patch's Tidal yet".to_string()
        }
        "d1" | "d2" | "d3" | "d4" | "hush" | "setcps" | "p" | "once" => {
            " — that starts a statement, at the beginning of a line".to_string()
        }
        _ => hint,
    };
    err(at, format!("'{name}' isn't a word Patch's Tidal knows{why}"))
}

/// Edits between two words (a swap of neighbours counts as one: `gian`).
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().map(|c| c.to_ascii_lowercase()).collect();
    let b: Vec<char> = b.chars().map(|c| c.to_ascii_lowercase()).collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=b.len() {
        d[0][j] = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = v;
        }
    }
    d[a.len()][b.len()]
}

/// The closest known word, when one is close.
pub fn suggest(name: &str) -> Option<&'static str> {
    WORDS
        .iter()
        .map(|w| w.0)
        .filter(|w| !w.starts_with('_'))
        .chain(CONTROLS.iter().copied())
        .map(|w| (distance(name, w), w))
        .filter(|(d, w)| *d <= 2.min(w.len() / 2).max(1))
        .min_by_key(|(d, _)| *d)
        .map(|(_, w)| w)
}

fn control_value(name: &str, v: &Value) -> Vec<Value> {
    if text_control(name) {
        return vec![Value::Text(v.to_string())];
    }
    if matches!(name, "n" | "note")
        && let Value::Text(s) = v
        && s.contains('\'')
    {
        return notes::chord(s)
            .map(|ns| ns.into_iter().map(Value::Num).collect())
            .unwrap_or_default();
    }
    match number(v) {
        Some(x) => vec![Value::Num(x)],
        None => vec![],
    }
}

/// A control function applied to its values (`s "bd:3"`, `n "c'maj"`).
fn make_control(name: &'static str, arg: V, at: usize) -> R<V> {
    let key = control_name(name).to_string();
    let p = match arg {
        V::Num(x) => Pattern::pure(Val::s(Value::Num(x))),
        V::Pat(Kind::Plain, p) => p,
        other => {
            return err(
                at,
                format!("{name} takes values (like \"bd sn\" or \"0 2\"), not {}", other.kind_name()),
            );
        }
    };
    Ok(V::Pat(
        Kind::Control,
        flat_map(
            &p,
            Arc::new(move |v| {
                let Val::S(v, loc) = v else { return vec![] };
                let loc = *loc;
                // `s "bd:3"` is s "bd" with n 3.
                if key == "s"
                    && let Some((s, n)) = v.to_string().split_once(':')
                    && let Ok(n) = n.parse::<f64>()
                {
                    let mut c = Ctl::new();
                    c.insert("s".into(), Value::Text(s.into()));
                    c.insert("n".into(), Value::Num(n));
                    return vec![Val::C(c, loc)];
                }
                control_value(&key, v)
                    .into_iter()
                    .map(|x| {
                        let mut c = Ctl::new();
                        c.insert(key.clone(), x);
                        Val::C(c, loc)
                    })
                    .collect()
            }),
        ),
    ))
}

fn pats_of(items: Vec<V>, at: usize) -> R<(Kind, Vec<Pattern<Val>>)> {
    let mut kind = None;
    let mut out = vec![];
    for v in items {
        let (k, p) = pat_of(v, at)?;
        if kind.is_some_and(|x| x != k) {
            return err(at, "a list of patterns must all be the same kind");
        }
        kind = Some(k);
        out.push(p);
    }
    Ok((kind.unwrap_or(Kind::Control), out))
}

fn scale_pat(gain_by: f64, speed_by: f64, p: &Pattern<Val>) -> Pattern<Val> {
    p.fmap(move |v| match v {
        Val::C(mut c, l) => {
            if gain_by != 1.0 {
                let g = c.get("gain").and_then(Value::num).unwrap_or(1.0) * gain_by;
                c.insert("gain".into(), Value::Num(g));
            }
            if speed_by != 1.0 {
                let s = c.get("speed").and_then(Value::num).unwrap_or(1.0) * speed_by;
                c.insert("speed".into(), Value::Num(s));
            }
            Val::C(c, l)
        }
        other => other,
    })
}

fn set_pan(p: &Pattern<Val>, pan: f64) -> Pattern<Val> {
    p.fmap(move |v| match v {
        Val::C(mut c, l) => {
            c.insert("pan".into(), Value::Num(pan));
            Val::C(c, l)
        }
        other => other,
    })
}

const SALT_DEGRADE: u64 = 0x7d1d_0001;
const SALT_SOMETIMES: u64 = 0x7d1d_0002;
const SALT_CYCLES: u64 = 0x7d1d_0003;
const SALT_RAND: u64 = 0x7d1d_0004;
const SALT_SHUFFLE: u64 = 0x7d1d_0005;

fn arp_order(mode: &str, xs: &[Hap<Val>]) -> Option<Vec<Hap<Val>>> {
    let v: Vec<Hap<Val>> = xs.to_vec();
    let rev = |v: &[Hap<Val>]| v.iter().rev().cloned().collect::<Vec<_>>();
    let init = |v: Vec<Hap<Val>>| {
        let mut v = v;
        v.pop();
        v
    };
    fn converge(v: &[Hap<Val>]) -> Vec<Hap<Val>> {
        let (mut lo, mut hi) = (0usize, v.len());
        let mut out = vec![];
        let mut front = true;
        while lo < hi {
            if front {
                out.push(v[lo].clone());
                lo += 1;
            } else {
                hi -= 1;
                out.push(v[hi].clone());
            }
            front = !front;
        }
        out
    }
    Some(match mode {
        "up" => v,
        "down" => rev(&v),
        "updown" => [init(v.clone()), init(rev(&v))].concat(),
        "downup" => [init(rev(&v)), init(v.clone())].concat(),
        "up&down" => [v.clone(), rev(&v)].concat(),
        "down&up" => [rev(&v), v.clone()].concat(),
        "converge" => converge(&v),
        "diverge" => rev(&converge(&v)),
        "disconverge" => {
            let c = converge(&v);
            let r = rev(&c);
            [c, r.into_iter().skip(1).collect()].concat()
        }
        "pinkyup" => match v.split_last() {
            Some((last, rest)) => rest.iter().flat_map(|x| [x.clone(), last.clone()]).collect(),
            None => v,
        },
        "thumbup" => match v.split_first() {
            Some((first, rest)) => rest.iter().flat_map(|x| [first.clone(), x.clone()]).collect(),
            None => v,
        },
        _ => return None,
    })
}

fn arp(p: &Pattern<Val>, mode: String) -> Pattern<Val> {
    let p = p.clone();
    Pattern::new(move |qq| {
        let mut haps = p.query(qq);
        haps.sort_by(|a, b| {
            let (x, y) = (a.whole.map(|w| (w.begin, w.end)), b.whole.map(|w| (w.begin, w.end)));
            x.cmp(&y)
        });
        let mut out = vec![];
        let mut i = 0;
        while i < haps.len() {
            let w = haps[i].whole;
            let mut j = i;
            while j < haps.len() && haps[j].whole == w {
                j += 1;
            }
            let group = &haps[i..j];
            i = j;
            let Some(w) = w else {
                out.extend(group.iter().cloned());
                continue;
            };
            let Some(order) = arp_order(&mode, group) else { continue };
            let d = w.duration() / Time::whole(order.len().max(1) as i64);
            for (k, h) in order.into_iter().enumerate() {
                let b = w.begin + d * Time::whole(k as i64);
                let nw = Span::new(b, b + d);
                if let Some(part) = nw.intersect(h.part) {
                    out.push(Hap {
                        whole: Some(nw),
                        part,
                        value: h.value,
                    });
                }
            }
        }
        out
    })
}

fn rot(p: &Pattern<Val>, by: i64) -> Pattern<Val> {
    let p = p.clone();
    Pattern::new(move |qq| {
        let mut out = vec![];
        for span in qq.span.split_cycles() {
            let c = span.begin.sam();
            let mut all: Vec<Hap<Val>> = p
                .query(q(Span::new(c, c + Time::ONE), qq.seed))
                .into_iter()
                .filter(|h| h.whole.is_some())
                .collect();
            all.sort_by_key(|h| h.part.begin);
            let n = all.len();
            if n == 0 {
                continue;
            }
            let values: Vec<Val> = all.iter().map(|h| h.value.clone()).collect();
            for (k, h) in all.into_iter().enumerate() {
                if let Some(part) = h.part.intersect(span) {
                    out.push(Hap {
                        whole: h.whole,
                        part,
                        value: values[(k as i64 + by).rem_euclid(n as i64) as usize].clone(),
                    });
                }
            }
        }
        out
    })
}

fn press_by(p: &Pattern<Val>, r: f64) -> Pattern<Val> {
    let r = time(r.clamp(0.0, 1.0));
    let p = p.clone();
    Pattern::new(move |qq| {
        p.query(qq)
            .into_iter()
            .filter_map(|h| {
                let w = h.whole?;
                let nw = Span::new(w.begin + w.duration() * r, w.end);
                let part = nw.intersect(h.part)?;
                Some(Hap {
                    whole: Some(nw),
                    part,
                    value: h.value,
                })
            })
            .collect()
    })
}

/// Each cycle plays `len` of itself from `from` (`trunc`), or that piece
/// over and over to the end of the cycle (`linger`).
fn copy_cycle(p: &Pattern<Val>, from: Time, len: Time, repeat: bool) -> Pattern<Val> {
    let p = p.clone();
    Pattern::new(move |qq| {
        let mut out = vec![];
        if len <= Time::ZERO {
            return out;
        }
        for span in qq.span.split_cycles() {
            let c = span.begin.sam();
            let mut r = 0i64;
            loop {
                let b = c + len * Time::whole(r);
                if b >= c + Time::ONE {
                    break;
                }
                let dst = Span::new(b, (b + len).min(c + Time::ONE));
                if let Some(want) = span.intersect(dst) {
                    let by = b - (c + from);
                    for h in p.query(q(want.with_time(|t| t - by), qq.seed)) {
                        out.push(shift_hap(h, by));
                    }
                }
                if !repeat {
                    break;
                }
                r += 1;
            }
        }
        out
    })
}

/// Fisher–Yates on a seeded sequence: the order of `n` slices this cycle.
fn permutation(seed: u64, cycle: i64, n: i64) -> Vec<i64> {
    let mut v: Vec<i64> = (0..n).collect();
    for i in (1..n as usize).rev() {
        let r = rand_at(seed, Time::new(cycle * 64 + i as i64, 64), SALT_SHUFFLE);
        let j = (r * (i + 1) as f64) as usize;
        v.swap(i, j.min(i));
    }
    v
}

/// A word of the vocabulary given all its arguments.
fn call(name: &'static str, a: Vec<V>, at: usize) -> R<V> {
    let mut a = a.into_iter();
    let mut next = || a.next().expect("arity is checked before a call");
    let plain = |p: Pattern<Val>| -> R<V> { Ok(V::Pat(Kind::Plain, p)) };
    let sig = |f: fn(f64) -> f64| -> R<V> {
        Ok(V::Pat(Kind::Plain, signal(move |t, _| f(t.cycle_pos().to_f64()))))
    };
    use std::f64::consts::TAU;
    if CONTROLS.contains(&name) {
        return make_control(name, next(), at);
    }
    match name {
        "silence" => Ok(V::Pat(Kind::Control, Pattern::silence())),
        "sine" => sig(|x| 0.5 + 0.5 * (x * TAU).sin()),
        "cosine" => sig(|x| 0.5 + 0.5 * (x * TAU).cos()),
        "saw" => sig(|x| x),
        "isaw" => sig(|x| 1.0 - x),
        "tri" => sig(|x| if x < 0.5 { 2.0 * x } else { 2.0 - 2.0 * x }),
        "square" => sig(|x| if x < 0.5 { 0.0 } else { 1.0 }),
        "rand" => plain(signal(|t, seed| rand_at(seed, t, SALT_RAND))),
        "perlin" => plain(signal(perlin_at)),
        "id" => Ok(next()),
        "irand" => {
            let n = next();
            plain(tparam(n, at, |n| {
                signal(move |t, seed| (rand_at(seed, t, SALT_RAND) * n).floor())
            })?)
        }
        "run" => plain(tparam(next(), at, |n| {
            let n = n.floor().clamp(0.0, 1024.0) as i64;
            Pattern::fastcat((0..n).map(|i| Pattern::pure(Val::s(Value::Num(i as f64)))).collect())
        })?),
        "scan" => plain(tparam(next(), at, |n| {
            let n = n.floor().clamp(1.0, 256.0) as i64;
            Pattern::slowcat(
                (1..=n)
                    .map(|k| {
                        Pattern::fastcat((0..k).map(|i| Pattern::pure(Val::s(Value::Num(i as f64)))).collect())
                    })
                    .collect(),
            )
        })?),
        "choose" | "cycleChoose" => {
            let items = list(next(), at)?;
            let mut vals = vec![];
            for v in items {
                match v {
                    V::Num(x) => vals.push(Val::s(Value::Num(x))),
                    V::Pat(Kind::Plain, p) => {
                        // A plain string literal: its first value.
                        if let Some(h) = p.query_span(Span::cycles(0, 1), 0).into_iter().next() {
                            vals.push(h.value)
                        }
                    }
                    _ => return err(at, "choose takes a list of values, like [\"bd\", \"sn\"] or [0, 2, 7]"),
                }
            }
            if vals.is_empty() {
                return plain(Pattern::silence());
            }
            let p = Pattern::signal(move |t, seed| {
                let i = (rand_at(seed, t, SALT_RAND) * vals.len() as f64) as usize;
                vals[i.min(vals.len() - 1)].clone()
            });
            plain(if name == "cycleChoose" { p.segment(1) } else { p })
        }
        "cat" | "slowcat" => {
            let (k, ps) = pats_of(list(next(), at)?, at)?;
            Ok(V::Pat(k, Pattern::slowcat(ps)))
        }
        "fastcat" | "fastCat" => {
            let (k, ps) = pats_of(list(next(), at)?, at)?;
            Ok(V::Pat(k, Pattern::fastcat(ps)))
        }
        "randcat" => {
            let (k, ps) = pats_of(list(next(), at)?, at)?;
            Ok(V::Pat(k, Pattern::choose_cycle(ps, SALT_CYCLES)))
        }
        "timeCat" | "timecat" => {
            let items = list(next(), at)?;
            let mut parts = vec![];
            let mut kind = None;
            for it in items {
                let V::Tuple(t) = it else {
                    return err(at, "timeCat takes pairs: [(1, s \"bd*4\"), (1, s \"hh27*8\")]");
                };
                let mut t = t.into_iter();
                let (Some(w), Some(p), None) = (t.next(), t.next(), t.next()) else {
                    return err(at, "timeCat takes pairs of a weight and a pattern");
                };
                let w = konst(&w, at, "a timeCat weight")?;
                let (k, p) = pat_of(p, at)?;
                if kind.is_some_and(|x| x != k) {
                    return err(at, "timeCat's patterns must be the same kind");
                }
                kind = Some(k);
                parts.push((time(w.max(0.0)), p));
            }
            Ok(V::Pat(kind.unwrap_or(Kind::Control), Pattern::timecat(parts)))
        }
        "stack" => {
            let (k, ps) = pats_of(list(next(), at)?, at)?;
            Ok(V::Pat(k, Pattern::stack(ps)))
        }
        "overlay" | "append" | "slowAppend" | "slowappend" | "fastAppend" | "fastappend" => {
            let (ka, pa) = pat_of(next(), at)?;
            let (kb, pb) = pat_of(next(), at)?;
            if ka != kb {
                return err(at, format!("{name} takes two patterns of the same kind"));
            }
            Ok(V::Pat(
                ka,
                match name {
                    "overlay" => Pattern::stack(vec![pa, pb]),
                    "fastAppend" | "fastappend" => Pattern::fastcat(vec![pa, pb]),
                    _ => Pattern::slowcat(vec![pa, pb]),
                },
            ))
        }
        "fast" | "density" | "slow" | "sparsity" | "hurry" => {
            let by = next();
            let (k, p) = pat_of(next(), at)?;
            let slow = matches!(name, "slow" | "sparsity");
            let hurry = name == "hurry";
            Ok(V::Pat(
                k,
                tparam(by, at, move |x| {
                    let t = time(x);
                    let out = if slow { p.slow(t) } else { p.fast(t) };
                    if hurry { scale_pat(1.0, x, &out) } else { out }
                })?,
            ))
        }
        "rev" => {
            let (k, p) = pat_of(next(), at)?;
            Ok(V::Pat(k, p.rev()))
        }
        "palindrome" => {
            let (k, p) = pat_of(next(), at)?;
            Ok(V::Pat(k, Pattern::slowcat(vec![p.clone(), p.rev()])))
        }
        "iter" | "iter'" => {
            let n = next();
            let (k, p) = pat_of(next(), at)?;
            let back = name == "iter'";
            Ok(V::Pat(
                k,
                tparam(n, at, move |n| {
                    let n = n.round().clamp(1.0, 256.0) as i64;
                    if back {
                        Pattern::slowcat((0..n).map(|i| p.late(Time::new(i, n))).collect())
                    } else {
                        p.iter(n as u32)
                    }
                })?,
            ))
        }
        "ply" => {
            let n = next();
            let (k, p) = pat_of(next(), at)?;
            Ok(V::Pat(k, tparam(n, at, move |n| p.ply(n.round().clamp(1.0, 64.0) as u32))?))
        }
        "segment" | "discretise" => {
            let n = next();
            let (k, p) = pat_of(next(), at)?;
            Ok(V::Pat(k, tparam(n, at, move |n| p.segment(n.round().clamp(1.0, 1024.0) as u32))?))
        }
        "range" | "rangex" => {
            let (lo, hi) = (next(), next());
            let (k, p) = pat_of(next(), at)?;
            if k != Kind::Plain {
                return err(at, "range maps numbers: range 200 2000 sine");
            }
            let exp = name == "rangex";
            let map = move |v: f64, lo: f64, hi: f64| {
                if exp && lo > 0.0 && hi > 0.0 {
                    lo * (hi / lo).powf(v)
                } else {
                    lo + v * (hi - lo)
                }
            };
            if let (V::Num(lo), V::Num(hi)) = (&lo, &hi) {
                let (lo, hi) = (*lo, *hi);
                return plain(p.fmap(move |v| match v {
                    Val::S(x, l) => match number(&x) {
                        Some(n) => Val::S(Value::Num(map(n, lo, hi)), l),
                        None => Val::S(x, l),
                    },
                    other => other,
                }));
            }
            let (_, lp) = pat_of(lo, at)?;
            let (_, hp) = pat_of(hi, at)?;
            let bounds = app_both(
                &lp,
                &hp,
                Arc::new(|a, b| {
                    let mut c = Ctl::new();
                    if let (Val::S(a, _), Val::S(b, _)) = (a, b) {
                        c.insert("lo".into(), a.clone());
                        c.insert("hi".into(), b.clone());
                    }
                    Val::C(c, a.loc().union(b.loc()))
                }),
            );
            plain(app_left(
                &p,
                &bounds,
                Arc::new(move |v, b| {
                    let (Val::S(v, l), Val::C(b, bl)) = (v, b) else { return v.clone() };
                    let lo = b.get("lo").and_then(number).unwrap_or(0.0);
                    let hi = b.get("hi").and_then(number).unwrap_or(1.0);
                    let l = l.union(*bl);
                    match number(v) {
                        Some(n) => Val::S(Value::Num(map(n, lo, hi)), l),
                        None => Val::S(v.clone(), l),
                    }
                }),
            ))
        }
        "quantise" => {
            let (k, p) = pat_of(next(), at)?;
            Ok(V::Pat(
                k,
                p.fmap(|v| match v {
                    Val::S(x, l) => Val::S(number(&x).map_or(x, |n| Value::Num(n.round())), l),
                    other => other,
                }),
            ))
        }
        "toScale" => {
            let steps: Vec<f64> = list(next(), at)?
                .iter()
                .map(|v| konst(v, at, "a scale step"))
                .collect::<R<_>>()?;
            if steps.is_empty() {
                return err(at, "toScale needs at least one step");
            }
            let (k, p) = pat_of(next(), at)?;
            Ok(V::Pat(
                k,
                p.fmap(move |v| match v {
                    Val::S(x, l) => Val::S(number(&x).map_or(x, |n| Value::Num(notes::degree(&steps, n))), l),
                    other => other,
                }),
            ))
        }
        "scale" => {
            let names = next();
            let (k, p) = pat_of(next(), at)?;
            if k != Kind::Plain {
                return err(at, "scale works on numbers: n (scale \"major\" \"0 .. 7\")");
            }
            let (_, np) = match names {
                V::Pat(Kind::Plain, np) => (Kind::Plain, np),
                other => return err(at, format!("scale's first argument is a scale name like \"major\", not {}", other.kind_name())),
            };
            plain(app_left(
                &p,
                &np,
                Arc::new(|v, s| {
                    let (Val::S(v, l), Val::S(s, sl)) = (v, s) else { return v.clone() };
                    let steps = notes::scale_table(&s.to_string()).unwrap_or(&[0.0]);
                    let l = l.union(*sl);
                    match number(v) {
                        Some(n) => Val::S(Value::Num(notes::degree(steps, n)), l),
                        None => Val::S(v.clone(), l),
                    }
                }),
            ))
        }
        "arp" => {
            let mode = next();
            let (k, p) = pat_of(next(), at)?;
            Ok(V::Pat(k, wparam(mode, at, move |m| arp_order(m, &[]).map(|_| arp(&p, m.to_string())))?))
        }
        "arpeggiate" => {
            let (k, p) = pat_of(next(), at)?;
            Ok(V::Pat(k, arp(&p, "up".into())))
        }
        "rolled" | "rolledBy" => {
            let t = if name == "rolled" { V::Num(0.25) } else { next() };
            let (k, p) = pat_of(next(), at)?;
            Ok(V::Pat(k, tparam(t, at, move |t| rolled(&p, t))?))
        }
        "plyWith" => {
            let n = konst(&next(), at, "plyWith's count")?.round().clamp(1.0, 64.0) as i64;
            let f = func(next(), at)?;
            let (k, p) = pat_of(next(), at)?;
            let mut layers = vec![];
            let mut cur = p;
            for i in 0..n {
                layers.push(slice_of_events(&cur, i, n));
                if i + 1 < n {
                    cur = transform(&f, k, cur, at)?;
                }
            }
            Ok(V::Pat(k, Pattern::stack(layers)))
        }
        "bite" | "chew" => {
            let n = konst(&next(), at, "the number of bits")?.round().clamp(1.0, 256.0) as i64;
            let (_, ip) = pat_of(next(), at)?;
            let (k, p) = pat_of(next(), at)?;
            let chew = name == "chew" && k == Kind::Control;
            Ok(V::Pat(k, squeeze_with(&ip, Arc::new(move |v| {
                let i = number(v)?.floor() as i64;
                let n = n;
                Some((p.clone(), Time::new(i.rem_euclid(n), n), Time::new(1, n)))
            }), chew)))
        }
        "fit" => {
            let step = konst(&next(), at, "fit's step")?.round() as i64;
            let items = list(next(), at)?;
            if items.is_empty() {
                return err(at, "fit needs a list to pick from: fit 1 [\"bd\", \"sn\"] \"0 1\"");
            }
            let (kind, items) = pats_of(items, at)?;
            let (_, ip) = pat_of(next(), at)?;
            let len = items.len() as i64;
            Ok(V::Pat(kind, Pattern::new(move |qq| {
                let mut out = vec![];
                for h in ip.query(qq) {
                    let Val::S(v, l) = &h.value else { continue };
                    let Some(i) = number(v) else { continue };
                    let cycle = h.onset().sam().to_f64() as i64;
                    let k = (i.floor() as i64 + step * cycle).rem_euclid(len) as usize;
                    let over = h.whole.unwrap_or(h.part);
                    let Some(x) = items[k].query(q(over, qq.seed)).into_iter().next() else { continue };
                    let value = match x.value {
                        Val::S(xv, xl) => Val::S(xv, xl.union(*l)),
                        Val::C(c, xl) => Val::C(c, xl.union(*l)),
                    };
                    out.push(Hap { whole: h.whole, part: h.part, value });
                }
                out
            })))
        }
        "_xfade" | "_clutch" => {
            let start = konst(&next(), at, "the start")?;
            let cycles = konst(&next(), at, "the length")?.max(1e-3);
            let old = control(next(), at, name)?;
            let new = control(next(), at, name)?;
            let clutch = name == "_clutch";
            let progress = move |t: Time| ((t.to_f64() - start) / cycles).clamp(0.0, 1.0);
            Ok(V::Pat(Kind::Control, Pattern::new(move |qq| {
                let mut out = vec![];
                // A crossfade at equal power (gain is squared, as in Tidal);
                // a clutch hands each event over at random, the same draw
                // for both so they swap rather than overlap.
                for (p, rising) in [(&old, false), (&new, true)] {
                    for h in p.query(qq) {
                        let x = progress(h.onset());
                        let keep = if rising { x } else { 1.0 - x };
                        if clutch {
                            let r = rand_at(qq.seed, h.onset(), 0xc1u64);
                            if (rising && r < x) || (!rising && r >= x) {
                                out.push(h);
                            }
                        } else if keep > 0.0 {
                            let g = (keep * std::f64::consts::FRAC_PI_2).sin().sqrt();
                            let value = if keep >= 1.0 {
                                h.value
                            } else {
                                with_ctl(&h.value, |c| {
                                    let was = c.get("gain").and_then(number).unwrap_or(1.0);
                                    c.insert("gain".into(), Value::Num(was * g));
                                })
                            };
                            out.push(Hap { value, ..h });
                        }
                    }
                }
                out
            })))
        }
        "_jump" => {
            let start = time(konst(&next(), at, "the start")?);
            let old = control(next(), at, name)?;
            let new = control(next(), at, name)?;
            Ok(V::Pat(Kind::Control, Pattern::new(move |qq| {
                let mut out: Vec<Hap<Val>> = old.query(qq).into_iter().filter(|h| h.onset() < start).collect();
                out.extend(new.query(qq).into_iter().filter(|h| h.onset() >= start));
                out
            })))
        }
        "_once" => {
            let start = time(konst(&next(), at, "the start")?.max(0.0));
            let p = control(next(), at, name)?;
            Ok(V::Pat(Kind::Control, Pattern::new(move |qq| {
                // Cycle 0 of the pattern, from `start`.
                let inner = Span::new(qq.span.begin - start, qq.span.end - start);
                let Some(inner) = inner.intersect(Span::new(Time::ZERO, Time::ONE)) else { return vec![] };
                p.query(q(inner, qq.seed))
                    .into_iter()
                    .filter(|h| h.whole.is_none_or(|w| w.begin >= Time::ZERO && w.begin < Time::ONE))
                    .map(|h| shift_hap(h, start))
                    .collect()
            })))
        }
        "squeeze" => {
            let (_, ip) = pat_of(next(), at)?;
            let (k, ps) = pats_of(list(next(), at)?, at)?;
            if ps.is_empty() {
                return Ok(V::Pat(k, Pattern::silence()));
            }
            Ok(V::Pat(k, squeeze_into(&ip, Arc::new(move |v| {
                let i = (number(v)?.floor() as i64).rem_euclid(ps.len() as i64) as usize;
                Some((ps[i].clone(), Time::ZERO, Time::ONE))
            }))))
        }
        "chop" => {
            let n = next();
            let p = control(next(), at, "chop")?;
            Ok(V::Pat(Kind::Control, tparam(n, at, move |n| chop(&p, n.round().clamp(1.0, 1024.0) as i64))?))
        }
        "striate" | "striateBy" => {
            let n = next();
            let f = if name == "striateBy" { Some(konst(&next(), at, "striateBy's length")?.clamp(0.0, 1.0)) } else { None };
            let p = control(next(), at, name)?;
            Ok(V::Pat(Kind::Control, tparam(n, at, move |n| {
                let n = n.round().clamp(1.0, 1024.0) as i64;
                // striate: n parts side by side; striateBy: n of length f,
                // spread from the start to the end.
                let range = |i: i64| match f {
                    None => (i as f64 / n as f64, (i + 1) as f64 / n as f64),
                    Some(f) => {
                        let slot = if n > 1 { (1.0 - f) / (n - 1) as f64 } else { 0.0 };
                        (slot * i as f64, slot * i as f64 + f)
                    }
                };
                Pattern::fastcat((0..n).map(|i| {
                    let (b, e) = range(i);
                    p.fmap(move |v| ranged(&v, b, e))
                }).collect())
            })?))
        }
        "slice" | "splice" => {
            let n = next();
            let (_, ip) = pat_of(next(), at)?;
            let p = control(next(), at, name)?;
            let splice = name == "splice";
            Ok(V::Pat(Kind::Control, tparam(n, at, move |n| {
                let n = n.round().clamp(1.0, 4096.0);
                let sliced = app_left(&ip, &p, Arc::new(move |i, v| {
                    let (Val::S(iv, il), Val::C(c, vl)) = (i, v) else { return v.clone() };
                    let Some(i) = number(iv) else { return v.clone() };
                    let i = i.floor().rem_euclid(n);
                    let mut c = c.clone();
                    c.insert("begin".into(), Value::Num(i / n));
                    c.insert("end".into(), Value::Num((i + 1.0) / n));
                    Val::C(c, il.union(*vl))
                }));
                if splice { fit_speed(&sliced, 1.0 / n) } else { sliced }
            })?))
        }
        "randslice" => {
            let n = next();
            let p = control(next(), at, "randslice")?;
            Ok(V::Pat(Kind::Control, tparam(n, at, move |n| {
                let n = n.round().clamp(1.0, 4096.0);
                let p = p.clone();
                Pattern::new(move |qq| {
                    p.query(qq)
                        .into_iter()
                        .map(|h| {
                            let i = (rand_at(qq.seed, h.onset(), 0x5e1c) * n).floor().min(n - 1.0);
                            Hap { value: ranged(&h.value, i / n, (i + 1.0) / n), ..h }
                        })
                        .collect()
                })
            })?))
        }
        "loopAt" => {
            let n = next();
            let p = control(next(), at, "loopAt")?;
            Ok(V::Pat(Kind::Control, tparam(n, at, move |n| {
                if n <= 0.0 {
                    return Pattern::silence();
                }
                p.slow(time(n)).fmap(move |v| with_ctl(&v, |c| {
                    let s = c.get("speed").and_then(number).unwrap_or(1.0);
                    c.insert("speed".into(), Value::Num(s / n));
                    c.insert("unit".into(), Value::Text("c".into()));
                }))
            })?))
        }
        "wedge" => {
            let t = konst(&next(), at, "wedge's share")?;
            let (ka, pa) = pat_of(next(), at)?;
            let (kb, pb) = pat_of(next(), at)?;
            if ka != kb {
                return err(at, "wedge takes two patterns of the same kind");
            }
            if t <= 0.0 {
                return Ok(V::Pat(ka, pb));
            }
            if t >= 1.0 {
                return Ok(V::Pat(ka, pa));
            }
            let t = time(t);
            Ok(V::Pat(ka, Pattern::timecat(vec![(t, pa), (Time::ONE - t, pb)])))
        }
        "soak" => {
            let n = konst(&next(), at, "soak's count")?.round().clamp(1.0, 64.0) as usize;
            let f = func(next(), at)?;
            let (k, p) = pat_of(next(), at)?;
            let mut out = vec![p];
            for _ in 1..n {
                let last = out.last().unwrap().clone();
                out.push(transform(&f, k, last, at)?);
            }
            Ok(V::Pat(k, Pattern::slowcat(out)))
        }
        "rot" => {
            let n = next();
            let (k, p) = pat_of(next(), at)?;
            Ok(V::Pat(k, tparam(n, at, move |n| rot(&p, n.round() as i64))?))
        }
        "brak" => {
            let (k, p) = pat_of(next(), at)?;
            let squashed = Pattern::fastcat(vec![p.clone(), Pattern::silence()]).late(Time::new(1, 4));
            Ok(V::Pat(k, p.when_cycle(|c| c.rem_euclid(2) == 1, move |_| squashed)))
        }
        "press" | "pressBy" => {
            let r = if name == "press" { V::Num(0.5) } else { next() };
            let (k, p) = pat_of(next(), at)?;
            Ok(V::Pat(k, tparam(r, at, move |r| press_by(&p, r))?))
        }
        "swingBy" | "swing" => {
            let x = if name == "swing" { V::Num(1.0 / 3.0) } else { next() };
            let x = konst(&x, at, "swing's amount")?;
            let n = next();
            let (k, p) = pat_of(next(), at)?;
            Ok(V::Pat(
                k,
                tparam(n, at, move |n| {
                    let n = time(n.max(1e-3));
                    p.slow(n)
                        .within(Time::new(1, 2), Time::ONE, |p| p.late(time(x)))
                        .fast(n)
                })?,
            ))
        }
        "inside" | "outside" => {
            let n = konst(&next(), at, "inside's factor")?;
            let n = if name == "outside" { 1.0 / n.max(1e-6) } else { n };
            let f = func(next(), at)?;
            let (k, p) = pat_of(next(), at)?;
            let t = time(n.max(1e-6));
            Ok(V::Pat(k, transform(&f, k, p.slow(t), at)?.fast(t)))
        }
        "linger" | "trunc" => {
            let x = next();
            let (k, p) = pat_of(next(), at)?;
            let repeat = name == "linger";
            Ok(V::Pat(
                k,
                tparam(x, at, move |x| {
                    let len = time(x.abs().clamp(0.0, 1.0));
                    // Negative: the end of the cycle instead of its start.
                    let from = if x < 0.0 { Time::ONE - len } else { Time::ZERO };
                    copy_cycle(&p, from, len, repeat)
                })?,
            ))
        }
        "zoom" | "compress" => {
            let (s, e) = pair(next(), at)?;
            let (k, p) = pat_of(next(), at)?;
            let (s, e) = (time(s), time(e));
            Ok(V::Pat(
                k,
                if name == "zoom" {
                    zoom_cycle(&p, s, e - s)
                } else {
                    p.compress(s, e)
                },
            ))
        }
        "fastGap" | "fastgap" => {
            let n = next();
            let (k, p) = pat_of(next(), at)?;
            Ok(V::Pat(k, tparam(n, at, move |n| p.compress(Time::ZERO, time(1.0 / n.max(1.0))))?))
        }
        "shuffle" | "scramble" => {
            let n = next();
            let (k, p) = pat_of(next(), at)?;
            let shuffle = name == "shuffle";
            Ok(V::Pat(
                k,
                tparam(n, at, move |n| {
                    let n = n.round().clamp(1.0, 64.0) as i64;
                    if shuffle {
                        reslice(&p, n as u32, Arc::new(move |seed, c, slot| permutation(seed, c, n)[slot as usize]))
                    } else {
                        reslice(
                            &p,
                            n as u32,
                            Arc::new(move |seed, c, slot| {
                                let r = rand_at(seed, Time::new(c * n + slot, n), SALT_SHUFFLE);
                                (r * n as f64) as i64
                            }),
                        )
                    }
                })?,
            ))
        }
        "every" | "every'" => {
            let n = next();
            let o = if name == "every'" { konst(&next(), at, "every's offset")? } else { 0.0 };
            let f = func(next(), at)?;
            let (k, p) = pat_of(next(), at)?;
            let changed = transform(&f, k, p.clone(), at)?;
            Ok(V::Pat(
                k,
                tparam(n, at, move |n| {
                    let n = n.round() as i64;
                    if n <= 0 {
                        return p.clone();
                    }
                    let c = changed.clone();
                    p.when_cycle(move |cyc| (cyc - o as i64).rem_euclid(n) == 0, move |_| c)
                })?,
            ))
        }
        "whenmod" => {
            let a = konst(&next(), at, "whenmod's cycle")?.round() as i64;
            let b = konst(&next(), at, "whenmod's threshold")?.round() as i64;
            let f = func(next(), at)?;
            let (k, p) = pat_of(next(), at)?;
            let changed = transform(&f, k, p.clone(), at)?;
            if a <= 0 {
                return Ok(V::Pat(k, p));
            }
            Ok(V::Pat(k, p.when_cycle(move |c| c.rem_euclid(a) >= b, move |_| changed)))
        }
        "foldEvery" => {
            let ns: Vec<f64> = list(next(), at)?
                .iter()
                .map(|v| konst(v, at, "foldEvery's cycles"))
                .collect::<R<_>>()?;
            let f = func(next(), at)?;
            let (k, mut p) = pat_of(next(), at)?;
            for n in ns.into_iter().rev() {
                let n = n.round() as i64;
                if n > 0 {
                    let changed = transform(&f, k, p.clone(), at)?;
                    p = p.when_cycle(move |c| c.rem_euclid(n) == 0, move |_| changed);
                }
            }
            Ok(V::Pat(k, p))
        }
        "within" | "within'" => {
            let (s, e) = pair(next(), at)?;
            let f = func(next(), at)?;
            let (k, p) = pat_of(next(), at)?;
            let (s, e) = (time(s), time(e));
            let changed = if name == "within'" {
                // `f` applied as if the window were the whole cycle.
                let z = zoom_cycle(&p, s, e - s);
                transform(&f, k, z, at)?.compress(s, e)
            } else {
                transform(&f, k, p.clone(), at)?
            };
            Ok(V::Pat(k, p.within(s, e, move |_| changed)))
        }
        "chunk" | "chunk'" => {
            let n = konst(&next(), at, "chunk's count")?.round().clamp(1.0, 64.0) as i64;
            let f = func(next(), at)?;
            let (k, p) = pat_of(next(), at)?;
            let changed = transform(&f, k, p.clone(), at)?;
            let parts: Vec<Pattern<Val>> = (0..n)
                .map(|i| {
                    let c = changed.clone();
                    p.within(Time::new(i, n), Time::new(i + 1, n), move |_| c)
                })
                .collect();
            let parts = if name == "chunk'" { parts.into_iter().rev().collect() } else { parts };
            Ok(V::Pat(k, Pattern::slowcat(parts)))
        }
        "while" => {
            let (_, b) = pat_of(next(), at)?;
            let f = func(next(), at)?;
            let (k, p) = pat_of(next(), at)?;
            let changed = transform(&f, k, p.clone(), at)?;
            Ok(V::Pat(k, sew(&b, &changed, &p)))
        }
        "sometimesBy" | "sometimes" | "often" | "rarely" | "almostNever" | "almostAlways" | "always" | "never" => {
            let x = match name {
                "sometimesBy" => next(),
                "sometimes" => V::Num(0.5),
                "often" => V::Num(0.75),
                "rarely" => V::Num(0.25),
                "almostNever" => V::Num(0.1),
                "almostAlways" => V::Num(0.9),
                "always" => V::Num(1.0),
                _ => V::Num(0.0),
            };
            let f = func(next(), at)?;
            let (k, p) = pat_of(next(), at)?;
            let changed = transform(&f, k, p.clone(), at)?;
            Ok(V::Pat(
                k,
                tparam(x, at, move |x| {
                    let c = changed.clone();
                    // The chosen events come from `f` of the pattern, so
                    // structure changes (`fast 2`) still take part.
                    Pattern::stack(vec![
                        p.degrade_by(x, SALT_SOMETIMES),
                        c.undegrade_by(x, SALT_SOMETIMES),
                    ])
                })?,
            ))
        }
        "someCyclesBy" | "somecyclesBy" | "someCycles" | "somecycles" => {
            let x = if name.ends_with("By") { konst(&next(), at, "the chance")? } else { 0.5 };
            let f = func(next(), at)?;
            let (k, p) = pat_of(next(), at)?;
            let changed = transform(&f, k, p.clone(), at)?;
            Ok(V::Pat(k, p.somecycles_by(x, SALT_CYCLES, move |_| changed)))
        }
        "degrade" => {
            let (k, p) = pat_of(next(), at)?;
            Ok(V::Pat(k, p.degrade_by(0.5, SALT_DEGRADE)))
        }
        "degradeBy" | "unDegradeBy" => {
            let x = next();
            let (k, p) = pat_of(next(), at)?;
            let un = name == "unDegradeBy";
            Ok(V::Pat(
                k,
                tparam(x, at, move |x| {
                    if un { p.undegrade_by(x, SALT_DEGRADE) } else { p.degrade_by(x, SALT_DEGRADE) }
                })?,
            ))
        }
        "struct" => {
            let (_, b) = pat_of(next(), at)?;
            let (k, p) = pat_of(next(), at)?;
            Ok(V::Pat(k, b.structure(&p)))
        }
        "mask" => {
            let (_, b) = pat_of(next(), at)?;
            let (k, p) = pat_of(next(), at)?;
            Ok(V::Pat(k, mask(&p, &b, true)))
        }
        "sew" => {
            let (_, b) = pat_of(next(), at)?;
            let (ka, pa) = pat_of(next(), at)?;
            let (kb, pb) = pat_of(next(), at)?;
            if ka != kb {
                return err(at, "sew takes two patterns of the same kind");
            }
            Ok(V::Pat(ka, sew(&b, &pa, &pb)))
        }
        "stitch" => {
            let (_, b) = pat_of(next(), at)?;
            let (ka, pa) = pat_of(next(), at)?;
            let (kb, pb) = pat_of(next(), at)?;
            if ka != kb {
                return err(at, "stitch takes two patterns of the same kind");
            }
            let on = b.filter_haps(|h| bool::from(h.value.clone()));
            let off = b.filter_haps(|h| !bool::from(h.value.clone())).fmap(|_| Val::s(Value::Num(1.0)));
            Ok(V::Pat(ka, Pattern::stack(vec![on.structure(&pa), off.structure(&pb)])))
        }
        "inv" => {
            let (_, b) = pat_of(next(), at)?;
            plain(b.fmap(|v| Val::s(Value::Num(if bool::from(v) { 0.0 } else { 1.0 }))))
        }
        "euclid" | "euclidInv" | "euclidFull" | "euclidOff" | "eoff" => {
            let kk = konst(&next(), at, "the number of onsets")?.round().max(0.0) as u32;
            let nn = konst(&next(), at, "the number of steps")?.round().clamp(1.0, 256.0) as u32;
            let r = if matches!(name, "euclidOff" | "eoff") {
                konst(&next(), at, "the rotation")?.round() as i32
            } else {
                0
            };
            let (k, p) = pat_of(next(), at)?;
            let bools = |inv: bool| {
                let steps = crate::pattern::bjorklund(kk, nn, -r);
                Pattern::fastcat(
                    steps
                        .into_iter()
                        .map(|b| Pattern::pure(Val::s(Value::Num(if b != inv { 1.0 } else { 0.0 }))))
                        .collect(),
                )
            };
            match name {
                "euclidInv" => Ok(V::Pat(k, bools(true).structure(&p))),
                "euclidFull" => {
                    let (k2, p2) = pat_of(next(), at)?;
                    if k2 != k {
                        return err(at, "euclidFull takes two patterns of the same kind");
                    }
                    Ok(V::Pat(k, Pattern::stack(vec![bools(false).structure(&p), bools(true).structure(&p2)])))
                }
                _ => Ok(V::Pat(k, bools(false).structure(&p))),
            }
        }
        "select" => {
            let (_, fp) = pat_of(next(), at)?;
            let (k, ps) = pats_of(list(next(), at)?, at)?;
            if ps.is_empty() {
                return Ok(V::Pat(k, Pattern::silence()));
            }
            let n = ps.len();
            Ok(V::Pat(k, inner_join(&fp, Arc::new(move |v| {
                let i = ((number(v)? * n as f64).floor() as i64).clamp(0, n as i64 - 1) as usize;
                Some(ps[i].clone())
            }))))
        }
        "pickF" => {
            let (_, ip) = pat_of(next(), at)?;
            let fs = list(next(), at)?;
            let (k, p) = pat_of(next(), at)?;
            let mut ps = vec![];
            for f in fs {
                ps.push(transform(&func(f, at)?, k, p.clone(), at)?);
            }
            if ps.is_empty() {
                return Ok(V::Pat(k, p));
            }
            Ok(V::Pat(k, inner_join(&ip, Arc::new(move |v| {
                let i = (number(v)?.floor() as i64).rem_euclid(ps.len() as i64) as usize;
                Some(ps[i].clone())
            }))))
        }
        "fix" | "unfix" | "contrast" => {
            let (fa, fb) = match name {
                "contrast" => (Some(func(next(), at)?), Some(func(next(), at)?)),
                "fix" => (Some(func(next(), at)?), None),
                _ => (None, Some(func(next(), at)?)),
            };
            let want = match next() {
                V::Pat(Kind::Control, c) => c
                    .query_span(Span::cycles(0, 1), 0)
                    .into_iter()
                    .find_map(|h| match h.value {
                        Val::C(c, _) => Some(c),
                        _ => None,
                    })
                    .unwrap_or_default(),
                other => return err(at, format!("{name} matches controls, like (s \"sn\") or (n 1), not {}", other.kind_name())),
            };
            let p = control(next(), at, name)?;
            let hit = {
                let want = want.clone();
                move |v: &Val| match v {
                    Val::C(c, _) => want.iter().all(|(k, w)| c.get(k).is_some_and(|x| x == w || number(x).zip(number(w)).is_some_and(|(a, b)| a == b))),
                    _ => false,
                }
            };
            let hit2 = hit.clone();
            let yes = p.filter_haps(move |h| hit(&h.value));
            let no = p.filter_haps(move |h| !hit2(&h.value));
            let yes = match fa { Some(f) => transform(&f, Kind::Control, yes, at)?, None => yes };
            let no = match fb { Some(f) => transform(&f, Kind::Control, no, at)?, None => no };
            Ok(V::Pat(Kind::Control, Pattern::stack(vec![yes, no])))
        }
        "euclidOffFull" => {
            let kk = konst(&next(), at, "the number of onsets")?.round().max(0.0) as u32;
            let nn = konst(&next(), at, "the number of steps")?.round().clamp(1.0, 256.0) as u32;
            let r = konst(&next(), at, "the rotation")?.round() as i32;
            let (k, p) = pat_of(next(), at)?;
            let (k2, p2) = pat_of(next(), at)?;
            if k2 != k {
                return err(at, "euclidOffFull takes two patterns of the same kind");
            }
            let bools = |inv: bool| {
                Pattern::fastcat(
                    crate::pattern::bjorklund(kk, nn, -r)
                        .into_iter()
                        .map(|b| Pattern::pure(Val::s(Value::Num(if b != inv { 1.0 } else { 0.0 }))))
                        .collect(),
                )
            };
            let on: Pattern<Val> = bools(false);
            let off: Pattern<Val> = bools(true);
            Ok(V::Pat(k, Pattern::stack(vec![on.filter_haps(|h| bool::from(h.value.clone())).structure(&p), off.filter_haps(|h| bool::from(h.value.clone())).structure(&p2)])))
        }
        "wchoose" | "wrandcat" => {
            let items = list(next(), at)?;
            let mut pats = vec![];
            let mut weights = vec![];
            for it in items {
                let V::Tuple(t) = it else {
                    return err(at, format!("{name} takes pairs of a value and its weight: [(\"bd\", 1), (\"sn\", 0.5)]"));
                };
                let mut t = t.into_iter();
                let (Some(v), Some(V::Num(w))) = (t.next(), t.next()) else {
                    return err(at, format!("{name}: each pair is (value, weight)"));
                };
                pats.push(pat_of(v, at)?);
                weights.push(w.max(0.0));
            }
            let total: f64 = weights.iter().sum();
            if pats.is_empty() || total <= 0.0 {
                return plain(Pattern::silence());
            }
            let pick = move |r: f64| {
                let mut x = r * total;
                for (i, w) in weights.iter().enumerate() {
                    if x < *w {
                        return i;
                    }
                    x -= w;
                }
                weights.len() - 1
            };
            let k = pats[0].0;
            let ps: Vec<Pattern<Val>> = pats.into_iter().map(|p| p.1).collect();
            if name == "wchoose" {
                // A value: each pattern's first.
                let vals: Vec<Val> = ps
                    .iter()
                    .map(|p| p.query_span(Span::cycles(0, 1), 0).into_iter().next().map_or(Val::s(Value::Num(0.0)), |h| h.value))
                    .collect();
                return plain(Pattern::signal(move |t, seed| vals[pick(rand_at(seed, t, SALT_RAND))].clone()));
            }
            Ok(V::Pat(k, Pattern::new(move |qq| {
                let mut out = vec![];
                for span in qq.span.split_cycles() {
                    let c = span.begin.sam();
                    let i = pick(rand_at(qq.seed, c, SALT_CYCLES));
                    out.extend(ps[i].query(q(span, qq.seed)));
                }
                out
            })))
        }
        "chooseBy" => {
            let (_, sig) = pat_of(next(), at)?;
            let mut vals = vec![];
            for v in list(next(), at)? {
                match v {
                    V::Num(x) => vals.push(Val::s(Value::Num(x))),
                    V::Pat(Kind::Plain, p) => {
                        if let Some(h) = p.query_span(Span::cycles(0, 1), 0).into_iter().next() {
                            vals.push(h.value)
                        }
                    }
                    _ => return err(at, "chooseBy takes a signal and a list of values"),
                }
            }
            if vals.is_empty() {
                return plain(Pattern::silence());
            }
            plain(sig.fmap(move |v| {
                let x = match &v { Val::S(x, _) => number(x).unwrap_or(0.0), _ => 0.0 };
                let i = ((x * vals.len() as f64).floor() as i64).clamp(0, vals.len() as i64 - 1) as usize;
                vals[i].clone()
            }))
        }
        "binary" | "binaryN" => {
            let bits = if name == "binaryN" { konst(&next(), at, "the number of bits")?.round().clamp(1.0, 32.0) as u32 } else { 8 };
            plain(tparam(next(), at, move |n| {
                let n = n.round().max(0.0) as u64;
                Pattern::fastcat(
                    (0..bits)
                        .rev()
                        .map(|b| Pattern::pure(Val::s(Value::Num(((n >> b) & 1) as f64))))
                        .collect(),
                )
            })?)
        }
        "brand" => plain(signal(|t, seed| if rand_at(seed, t, SALT_RAND) < 0.5 { 1.0 } else { 0.0 })),
        "brandBy" => plain(tparam(next(), at, |p| {
            signal(move |t, seed| if rand_at(seed, t, SALT_RAND) < p { 1.0 } else { 0.0 })
        })?),
        "envL" => plain(signal(|t, _| t.to_f64().clamp(0.0, 1.0))),
        "envR" => plain(signal(|t, _| 1.0 - t.to_f64().clamp(0.0, 1.0))),
        "envLR" => plain(signal(|t, _| 1.0 - t.to_f64().clamp(0.0, 1.0))),
        "envRR" => plain(signal(|t, _| t.to_f64().clamp(0.0, 1.0))),
        "fadeIn" | "fadeOut" => {
            let n = konst(&next(), at, "the number of cycles")?.max(0.01);
            let (k, p) = pat_of(next(), at)?;
            let out = name == "fadeOut";
            Ok(V::Pat(k, p.filter_haps(move |h| {
                let x = (h.onset().to_f64() / n).clamp(0.0, 1.0);
                let keep = if out { 1.0 - x } else { x };
                rand_at(0, h.onset(), SALT_DEGRADE) < keep
            })))
        }
        "superimpose" => {
            let f = func(next(), at)?;
            let (k, p) = pat_of(next(), at)?;
            let other = transform(&f, k, p.clone(), at)?;
            Ok(V::Pat(k, Pattern::stack(vec![p, other])))
        }
        "off" => {
            let t = next();
            let f = func(next(), at)?;
            let (k, p) = pat_of(next(), at)?;
            let changed = transform(&f, k, p.clone(), at)?;
            let later = tparam(t, at, move |t| changed.late(time(t)))?;
            Ok(V::Pat(k, Pattern::stack(vec![p, later])))
        }
        "layer" => {
            let fs = list(next(), at)?;
            let (k, p) = pat_of(next(), at)?;
            let mut out = vec![];
            for f in fs {
                out.push(transform(&func(f, at)?, k, p.clone(), at)?);
            }
            Ok(V::Pat(k, Pattern::stack(out)))
        }
        "jux" | "juxBy" => {
            let amount = if name == "juxBy" { konst(&next(), at, "jux's width")? } else { 1.0 };
            let f = func(next(), at)?;
            let p = control(next(), at, name)?;
            let changed = transform(&f, Kind::Control, p.clone(), at)?;
            Ok(V::Pat(
                Kind::Control,
                Pattern::stack(vec![set_pan(&p, 0.5 - amount / 2.0), set_pan(&changed, 0.5 + amount / 2.0)]),
            ))
        }
        "stut" => {
            let count = konst(&next(), at, "stut's count")?.round().clamp(1.0, 32.0) as i64;
            let fb = konst(&next(), at, "stut's feedback")?;
            let t = konst(&next(), at, "stut's time")?;
            let p = control(next(), at, "stut")?;
            let mut layers = vec![p.clone()];
            for x in 1..count {
                let g = fb + (1.0 - fb) * ((count - x) as f64 / count as f64);
                layers.push(scale_pat(g, 1.0, &p).late(time(t * x as f64)));
            }
            Ok(V::Pat(Kind::Control, Pattern::stack(layers)))
        }
        "echo" => {
            let count = konst(&next(), at, "echo's count")?.round().clamp(1.0, 32.0) as i64;
            let t = konst(&next(), at, "echo's time")?;
            let fb = konst(&next(), at, "echo's feedback")?;
            let p = control(next(), at, "echo")?;
            let layers = (0..count)
                .map(|x| scale_pat(fb.powi(x as i32), 1.0, &p).late(time(t * x as f64)))
                .collect();
            Ok(V::Pat(Kind::Control, Pattern::stack(layers)))
        }
        "stutWith" | "echoWith" | "stutter" => {
            let count = konst(&next(), at, "the count")?.round().clamp(1.0, 32.0) as i64;
            let t = konst(&next(), at, "the time")?;
            let f = if name == "stutter" { None } else { Some(func(next(), at)?) };
            let (k, p) = pat_of(next(), at)?;
            let mut layers = vec![p.clone()];
            let mut cur = p;
            for x in 1..count {
                if let Some(f) = &f {
                    cur = transform(f, k, cur, at)?;
                }
                layers.push(cur.late(time(t * x as f64)));
            }
            Ok(V::Pat(k, Pattern::stack(layers)))
        }
        "ghost" => {
            let p = control(next(), at, "ghost")?;
            let g = |by: f64| scale_pat(0.7, 1.25, &p).late(time(by));
            Ok(V::Pat(Kind::Control, Pattern::stack(vec![p.clone(), g(0.125 * 2.5), g(0.125 * 1.5)])))
        }
        "spread" | "slowspread" | "fastspread" => {
            let f = func(next(), at)?;
            let args = list(next(), at)?;
            let x = next();
            let mut out = vec![];
            let mut kind = None;
            for a in args {
                let g = func(apply(&f, a, at)?, at)?;
                let (k, p) = pat_of(apply(&g, x.clone(), at)?, at)?;
                if kind.is_some_and(|x| x != k) {
                    return err(at, "spread's results must be the same kind");
                }
                kind = Some(k);
                out.push(p);
            }
            let k = kind.unwrap_or(Kind::Control);
            Ok(V::Pat(k, if name == "fastspread" { Pattern::fastcat(out) } else { Pattern::slowcat(out) }))
        }
        other => err(at, format!("'{other}' isn't available yet")),
    }
}

/// `p`'s events where `b` is true (`keep`) or false.
fn mask(p: &Pattern<Val>, b: &Pattern<Val>, keep: bool) -> Pattern<Val> {
    let b = b.filter_haps(move |h| bool::from(h.value.clone()) == keep);
    app_left(p, &b, Arc::new(|v, _| v.clone()))
}

fn sew(b: &Pattern<Val>, on: &Pattern<Val>, off: &Pattern<Val>) -> Pattern<Val> {
    Pattern::stack(vec![mask(on, b, true), mask(off, b, false)])
}

/// Chords strummed: an event's notes (events sharing a span) start one
/// after another over `t` of its length (`t` < 0: from the top down).
fn rolled(p: &Pattern<Val>, t: f64) -> Pattern<Val> {
    let p = p.clone();
    let t = time(t.clamp(-1.0, 1.0));
    Pattern::new(move |qq| {
        let mut haps = p.query(qq);
        haps.sort_by_key(|h| h.whole.map(|w| (w.begin, w.end)));
        let mut out = Vec::with_capacity(haps.len());
        let mut i = 0;
        while i < haps.len() {
            let w = haps[i].whole;
            let mut j = i;
            while j < haps.len() && haps[j].whole == w {
                j += 1;
            }
            let n = (j - i) as i64;
            for (k, h) in haps[i..j].iter().enumerate() {
                let Some(whole) = w else {
                    out.push(h.clone());
                    continue;
                };
                let k = if t < Time::ZERO { n - 1 - k as i64 } else { k as i64 };
                let shift = whole.duration() * (if t < Time::ZERO { -t } else { t }) * Time::new(k, n.max(1));
                let begin = whole.begin + shift;
                let nw = Span::new(begin, whole.end);
                let Some(part) = h.part.intersect(nw) else { continue };
                out.push(Hap {
                    whole: Some(nw),
                    part: if h.part.begin <= begin { Span::new(begin, part.end) } else { part },
                    value: h.value.clone(),
                });
            }
            i = j;
        }
        out
    })
}

/// A control value changed in place (other values as they are).
fn with_ctl(v: &Val, f: impl FnOnce(&mut Ctl)) -> Val {
    match v {
        Val::C(c, l) => {
            let mut c = c.clone();
            f(&mut c);
            Val::C(c, *l)
        }
        other => other.clone(),
    }
}

/// Part `[b, e)` of the part of the sample an event already plays (its
/// `begin` … `end`, the whole file if it says none).
fn ranged(v: &Val, b: f64, e: f64) -> Val {
    with_ctl(v, |c| {
        let b0 = c.get("begin").and_then(number).unwrap_or(0.0);
        let e0 = c.get("end").and_then(number).unwrap_or(1.0);
        c.insert("begin".into(), Value::Num(b0 + (e0 - b0) * b));
        c.insert("end".into(), Value::Num(b0 + (e0 - b0) * e));
    })
}

/// Every event cut into `n`, each playing its share of the sample.
fn chop(p: &Pattern<Val>, n: i64) -> Pattern<Val> {
    let p = p.clone();
    Pattern::new(move |qq| {
        let mut out = vec![];
        for h in p.query(qq) {
            let Some(w) = h.whole else {
                out.push(h);
                continue;
            };
            let d = w.duration() / Time::whole(n);
            for i in 0..n {
                let sw = Span::new(w.begin + d * Time::whole(i), w.begin + d * Time::whole(i + 1));
                let Some(part) = h.part.intersect(sw) else { continue };
                if part.begin == part.end && sw.begin != sw.end {
                    continue;
                }
                out.push(Hap {
                    whole: Some(sw),
                    part,
                    value: ranged(&h.value, i as f64 / n as f64, (i + 1) as f64 / n as f64),
                });
            }
        }
        out
    })
}

/// `splice`: each event's speed set so its piece (`size` of the sample)
/// lasts exactly the event (`unit "c"`, times any speed it had).
fn fit_speed(p: &Pattern<Val>, size: f64) -> Pattern<Val> {
    let p = p.clone();
    Pattern::new(move |qq| {
        p.query(qq)
            .into_iter()
            .map(|h| {
                let d = h.whole.map_or(1.0, |w| w.duration().to_f64()).max(1e-6);
                let value = with_ctl(&h.value, |c| {
                    let s = c.get("speed").and_then(number).unwrap_or(1.0);
                    c.insert("speed".into(), Value::Num(s * size / d));
                    c.insert("unit".into(), Value::Text("c".into()));
                });
                Hap { value, ..h }
            })
            .collect()
    })
}

/// Slice `i` of `n` of every event of `p` (what `plyWith` stacks).
fn slice_of_events(p: &Pattern<Val>, i: i64, n: i64) -> Pattern<Val> {
    let p = p.clone();
    Pattern::new(move |qq| {
        let mut out = vec![];
        for h in p.query(qq) {
            let Some(w) = h.whole else { continue };
            let d = w.duration() / Time::whole(n);
            let sw = Span::new(w.begin + d * Time::whole(i), w.begin + d * Time::whole(i + 1));
            if let Some(part) = h.part.intersect(sw) {
                out.push(Hap {
                    whole: Some(sw),
                    part,
                    value: h.value.clone(),
                });
            }
        }
        out
    })
}

/// Each event of `ip` plays a piece of a pattern squeezed into it: `pick`
/// gives the pattern, where in its cycle the piece starts and how long it
/// is (`bite`: a bit of the cycle; `squeeze`: a whole cycle).
type Pick = Arc<dyn Fn(&Value) -> Option<(Pattern<Val>, Time, Time)> + Send + Sync>;

fn squeeze_into(ip: &Pattern<Val>, pick: Pick) -> Pattern<Val> {
    squeeze_with(ip, pick, false)
}

/// [`squeeze_into`]; with `chew`, a sound's speed follows how much its
/// piece was squeezed or stretched (`chew`).
fn squeeze_with(ip: &Pattern<Val>, pick: Pick, chew: bool) -> Pattern<Val> {
    let ip = ip.clone();
    Pattern::new(move |qq| {
        let mut out = vec![];
        for a in ip.query(qq) {
            let (Some(w), Val::S(v, _)) = (a.whole, &a.value) else { continue };
            let Some((p, start, len)) = pick(v) else { continue };
            if w.duration() <= Time::ZERO {
                continue;
            }
            let c = w.begin.sam();
            // Event time → source time and back.
            let to_src = |t: Time| c + start + (t - w.begin) / w.duration() * len;
            let to_dst = |t: Time| w.begin + (t - c - start) / len * w.duration();
            let factor = (len / w.duration()).to_f64();
            for h in p.query(q(a.part.with_time(to_src), qq.seed)) {
                let whole = h.whole.map(|x| x.with_time(to_dst));
                let part = h.part.with_time(to_dst);
                let Some(part) = part.intersect(a.part) else { continue };
                let value = if chew {
                    with_ctl(&h.value, |c| {
                        let s = c.get("speed").and_then(number).unwrap_or(1.0);
                        c.insert("speed".into(), Value::Num(s * factor));
                    })
                } else {
                    h.value
                };
                out.push(Hap { whole, part, value });
            }
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tidal::parse::expression;

    fn ev(src: &str) -> V {
        eval(&expression(src, 0, src.len()).unwrap(), &Env::default())
            .unwrap_or_else(|e| panic!("{src}: {}", e.message))
    }

    fn haps(src: &str, cycles: i64) -> Vec<(Time, Val)> {
        let V::Pat(_, p) = ev(src) else { panic!("{src}: not a pattern") };
        p.onsets(Span::cycles(0, cycles), 0)
            .into_iter()
            .map(|h| (h.part.begin, h.value))
            .collect()
    }

    fn ctl(src: &str, cycles: i64, key: &str) -> Vec<String> {
        haps(src, cycles)
            .into_iter()
            .map(|(_, v)| match v {
                Val::C(c, _) => c.get(key).map_or("-".into(), |v| v.to_string()),
                Val::S(v, _) => v.to_string(),
            })
            .collect()
    }

    fn times(src: &str, cycles: i64) -> Vec<String> {
        haps(src, cycles).into_iter().map(|(t, _)| t.to_string()).collect()
    }

    #[test]
    fn strums_plies_bites_and_squeezes() {
        assert_eq!(times(r#"rolledBy 0.5 $ n "c'maj""#, 1), ["0", "1/6", "1/3"]);
        assert_eq!(times(r#"rolledBy (-0.5) $ n "c'maj""#, 1), ["0", "1/6", "1/3"]);
        assert_eq!(ctl(r#"rolledBy (-0.5) $ n "c'maj""#, 1, "n"), ["7", "4", "0"]);
        assert_eq!(times(r#"plyWith 2 (|* gain 0.5) $ s "bd" # gain 1"#, 1), ["0", "1/2"]);
        assert_eq!(ctl(r#"plyWith 2 (|* gain 0.5) $ s "bd" # gain 1"#, 1, "gain"), ["1", "0.5"]);
        assert_eq!(ctl(r#"bite 2 "1 0" $ n "0 1 2 3""#, 1, "n"), ["2", "3", "0", "1"]);
        assert_eq!(times(r#"squeeze "0 1" [s "a*2", s "b"]"#, 1), ["0", "1/4", "1/2"]);
        assert_eq!(ctl(r#"squeeze "0 1" [s "a*2", s "b"]"#, 1, "s"), ["a", "a", "b"]);
        assert_eq!(times(r#"wedge (1/4) (s "a") (s "b c")"#, 1), ["0", "1/4", "5/8"]);
        assert_eq!(times(r#"soak 2 (fast 2) $ s "a""#, 2), ["0", "1", "3/2"]);
    }

    #[test]
    fn samples_cut_up() {
        assert_eq!(times(r#"chop 4 $ s "break""#, 1), ["0", "1/4", "1/2", "3/4"]);
        assert_eq!(ctl(r#"chop 4 $ s "break""#, 1, "begin"), ["0", "0.25", "0.5", "0.75"]);
        assert_eq!(ctl(r#"chop 2 $ s "break" # begin 0.5"#, 1, "begin"), ["0.5", "0.75"]);
        assert_eq!(ctl(r#"chop 2 $ s "break" # begin 0.5"#, 1, "end"), ["0.75", "1"]);
        // striate: the whole pattern n times, each time the next part.
        assert_eq!(ctl(r#"striate 2 $ s "a b""#, 1, "s"), ["a", "b", "a", "b"]);
        assert_eq!(ctl(r#"striate 2 $ s "a b""#, 1, "begin"), ["0", "0", "0.5", "0.5"]);
        assert_eq!(ctl(r#"striateBy 3 0.5 $ s "a""#, 1, "begin"), ["0", "0.25", "0.5"]);
        assert_eq!(ctl(r#"striateBy 3 0.5 $ s "a""#, 1, "end"), ["0.5", "0.75", "1"]);
        // slice: the index pattern's structure, the piece it names (wrapping).
        assert_eq!(times(r#"slice 4 "0 2 5" $ s "bev""#, 1), ["0", "1/3", "2/3"]);
        assert_eq!(ctl(r#"slice 4 "0 2 5" $ s "bev""#, 1, "begin"), ["0", "0.5", "0.25"]);
        // splice: the piece lasts its event (unit "c", speed = size / length).
        assert_eq!(ctl(r#"splice 4 "0 1" $ s "bev""#, 1, "speed"), ["0.5", "0.5"]);
        assert_eq!(ctl(r#"splice 4 "0 1" $ s "bev""#, 1, "unit"), ["c", "c"]);
        assert_eq!(ctl(r#"splice 4 "0 1" $ s "bev" # speed 2"#, 1, "speed"), ["1", "1"]);
        // loopAt: slower, and the sample stretched by speed to fit.
        assert_eq!(times(r#"loopAt 2 $ chop 4 $ s "break""#, 2), ["0", "1/2", "1", "3/2"]);
        assert_eq!(ctl(r#"loopAt 2 $ s "break""#, 2, "speed"), ["0.5"]);
        assert_eq!(ctl(r#"loopAt 2 $ s "break""#, 2, "unit"), ["c"]);
        let picks = ctl(r#"randslice 8 $ s "break*32""#, 1, "begin");
        assert_eq!(picks.len(), 32);
        let distinct: std::collections::BTreeSet<_> = picks.iter().collect();
        assert!(distinct.len() >= 5, "{picks:?}");
        assert!(picks.iter().all(|b| b.parse::<f64>().unwrap() < 1.0));
    }

    #[test]
    fn chew_and_fit() {
        // chew: bite, and the speed follows the squeeze (a bit of 1/4
        // played in 1/2: half speed).
        assert_eq!(ctl(r#"chew 4 "0 1" $ s "a*4""#, 1, "speed"), ["0.5", "0.5"]);
        assert_eq!(ctl(r#"chew 4 "0 [1 2]" $ s "a*4" # speed 2"#, 1, "speed"), ["1", "2", "2"]);
        assert_eq!(ctl(r#"chew 2 "0 1" $ n "0 1 2 3""#, 1, "n"), ["0", "1", "2", "3"]);
        // fit: the list, stepping on by `step` each cycle.
        assert_eq!(ctl(r#"s (fit 1 ["bd", "sn", "hh"] "0 1")"#, 3, "s"), ["bd", "sn", "sn", "hh", "hh", "bd"]);
    }

    #[test]
    fn fix_select_and_bits() {
        assert_eq!(ctl(r#"fix (# gain 2) (s "sn") $ s "bd sn""#, 1, "gain"), ["-", "2"]);
        assert_eq!(ctl(r#"unfix (# gain 2) (s "sn") $ s "bd sn""#, 1, "gain"), ["2", "-"]);
        assert_eq!(times(r#"struct (binaryN 4 5) $ s "a""#, 1), ["1/4", "3/4"]);
        assert_eq!(ctl(r#"select "<0 0.5>" [s "a", s "b"]"#, 2, "s"), ["a", "b"]);
        assert_eq!(ctl(r#"pickF "<0 1>" [id, fast 2] $ s "a""#, 2, "s"), ["a", "a", "a"]);
        let picks = ctl(r#"segment 64 $ n (wchoose [(0, 1), (7, 3)])"#, 4, "n");
        let sevens = picks.iter().filter(|n| *n == "7").count();
        assert!(sevens > 150 && sevens < 230, "{sevens} of {}", picks.len());
        assert!(times(r#"fadeOut 4 $ s "hh*16""#, 4).iter().filter(|t| t.starts_with('3')).count() < 8);
    }

    #[test]
    fn sounds_and_controls_combine() {
        assert_eq!(ctl(r#"s "bd sn""#, 1, "s"), ["bd", "sn"]);
        assert_eq!(ctl(r#"s "bd*2 sn" # gain 1.2"#, 1, "gain"), ["1.2", "1.2", "1.2"]);
        // Structure from the left, values from the right at the same time.
        assert_eq!(ctl(r#"s "bd*4" # n "0 1""#, 1, "n"), ["0", "0", "1", "1"]);
        assert_eq!(ctl(r#"n "0 1" + n "<12 7>""#, 2, "n"), ["12", "13", "7", "8"]);
        assert_eq!(ctl(r#"n "0 2" |+ n 12"#, 1, "n"), ["12", "14"]);
        assert_eq!(ctl(r#"n ("0 2" + "<12 7>")"#, 2, "n"), ["12", "14", "7", "9"]);
        assert_eq!(ctl(r#"s "drum:3""#, 1, "n"), ["3"]);
        assert_eq!(ctl(r#"note "c e g a4""#, 1, "note"), ["0", "4", "7", "-3"]);
        assert_eq!(ctl(r#"n "c'maj""#, 1, "n"), ["0", "4", "7"]);
        // `|<` keeps the left's values; `|+|` takes structure from both.
        assert_eq!(ctl(r#"n "1 2" |< n "5 6 7""#, 1, "n"), ["1", "2"]);
        assert_eq!(times(r#"n "1 2" |+| n "5 6 7""#, 1), ["0", "1/3", "1/2", "2/3"]);
    }

    #[test]
    fn time_functions() {
        assert_eq!(times(r#"fast 2 $ s "bd sn""#, 1), ["0", "1/4", "1/2", "3/4"]);
        assert_eq!(times(r#"slow 2 $ s "bd sn""#, 2), ["0", "1"]);
        assert_eq!(times(r#"0.25 ~> s "bd""#, 1), ["1/4"]);
        assert_eq!(times(r#"(1/4) <~ s "bd sn""#, 1), ["1/4", "3/4"]);
        assert_eq!(times(r#"fast "<1 2>" $ s "bd""#, 2), ["0", "1", "3/2"]);
        assert_eq!(ctl(r#"rev $ s "bd sn hh""#, 1, "s"), ["hh", "sn", "bd"]);
        assert_eq!(ctl(r#"every 2 (fast 2) $ s "bd sn""#, 2, "s"), ["bd", "sn", "bd", "sn", "bd", "sn"]);
        assert_eq!(ctl(r#"every 2 (# speed 2) $ s "bd""#, 2, "speed"), ["2", "-"]);
        assert_eq!(ctl(r#"whenmod 4 2 (# speed 2) $ s "bd""#, 4, "speed"), ["-", "-", "2", "2"]);
        assert_eq!(times(r#"off 0.25 (# crush 4) $ s "bd""#, 1), ["0", "1/4"]);
        assert_eq!(ctl(r#"jux rev $ s "bd sn""#, 1, "pan"), ["0", "1", "0", "1"]);
        assert_eq!(times(r#"ply 2 $ s "bd sn""#, 1), ["0", "1/4", "1/2", "3/4"]);
        assert_eq!(times(r#"press $ s "bd sn""#, 1), ["1/4", "3/4"]);
        assert_eq!(ctl(r#"iter 4 $ s "a b c d""#, 2, "s"), ["a", "b", "c", "d", "b", "c", "d", "a"]);
        assert_eq!(ctl(r#"palindrome $ s "a b c""#, 2, "s"), ["a", "b", "c", "c", "b", "a"]);
        assert_eq!(ctl(r#"linger 0.25 $ n "0 1 2 3 4 5 6 7""#, 1, "n"), ["0", "1", "0", "1", "0", "1", "0", "1"]);
        assert_eq!(ctl(r#"zoom (0.25, 0.75) $ n "0 1 2 3""#, 1, "n"), ["1", "2"]);
        assert_eq!(ctl(r#"rot 1 $ n "0 1 2""#, 1, "n"), ["1", "2", "0"]);
    }

    #[test]
    fn composition_sections_and_lists() {
        assert_eq!(ctl(r#"(rev . fast 2) $ n "0 1""#, 1, "n"), ["1", "0", "1", "0"]);
        assert_eq!(ctl(r#"every 2 ((# speed 2) . rev) $ n "0 1""#, 1, "speed"), ["2", "2"]);
        assert_eq!(ctl(r#"stack [s "bd", s "hh*2"]"#, 1, "s").len(), 3);
        assert_eq!(ctl(r#"cat [s "bd", s "sn"]"#, 2, "s"), ["bd", "sn"]);
        assert_eq!(ctl(r#"fastcat [s "bd", s "sn"]"#, 1, "s"), ["bd", "sn"]);
        assert_eq!(ctl(r#"n (run 4)"#, 1, "n"), ["0", "1", "2", "3"]);
        assert_eq!(ctl(r#"n "0 .. 3""#, 1, "n"), ["0", "1", "2", "3"]);
        assert_eq!(ctl(r#"n (scale "major" "0 .. 7")"#, 1, "n"), ["0", "2", "4", "5", "7", "9", "11", "12"]);
        assert_eq!(ctl(r#"n (toScale [0, 2, 7] "0 1 2 3")"#, 1, "n"), ["0", "2", "7", "12"]);
        assert_eq!(ctl(r#"arp "up" $ n "c'maj""#, 1, "n"), ["0", "4", "7"]);
        assert_eq!(ctl(r#"arp "<down up>" $ n "c'maj""#, 2, "n"), ["7", "4", "0", "0", "4", "7"]);
        assert_eq!(times(r#"arp "up" $ n "c'maj""#, 1), ["0", "1/3", "2/3"]);
        assert_eq!(ctl(r#"s "bd*4" # speed (range 1 2 saw)"#, 1, "speed"), ["1", "1.25", "1.5", "1.75"]);
        assert_eq!(ctl(r#"n (segment 4 $ range 1 5 saw)"#, 1, "n"), ["1", "2", "3", "4"]);
        assert_eq!(ctl(r#"spread fast [1, 2] $ s "bd""#, 2, "s"), ["bd", "bd", "bd"]);
    }

    #[test]
    fn structure_and_euclid() {
        assert_eq!(times(r#"struct "t ~ t t" $ s "bd""#, 1), ["0", "1/2", "3/4"]);
        assert_eq!(times(r#"mask "1 0" $ s "hh*4""#, 1), ["0", "1/4"]);
        assert_eq!(times(r#"euclid 3 8 $ s "bd""#, 1), ["0", "3/8", "3/4"]);
        assert_eq!(times(r#"s "bd(3,8)""#, 1), ["0", "3/8", "3/4"]);
        // Tidal rotates earlier: (3,8,2) is x..x.x.. shifted two steps left.
        assert_eq!(times(r#"s "bd(3,8,2)""#, 1), ["1/8", "1/2", "3/4"]);
        assert_eq!(times(r#"euclidOff 3 8 2 $ s "bd""#, 1), ["1/8", "1/2", "3/4"]);
        assert_eq!(times(r#"euclidInv 3 8 $ s "bd""#, 1).len(), 5);
        assert_eq!(ctl(r#"sew "t f" (n "0*4") (n "1*4")"#, 1, "n"), ["0", "0", "1", "1"]);
    }

    #[test]
    fn chance_is_deterministic_and_balanced() {
        let a = times(r#"sometimes (# speed 2) $ s "hh*16""#, 8);
        assert_eq!(a.len(), 128);
        let fast = ctl(r#"sometimes (# speed 2) $ s "hh*16""#, 8, "speed");
        let changed = fast.iter().filter(|s| *s == "2").count();
        assert!((40..88).contains(&changed), "{changed}");
        assert_eq!(fast, ctl(r#"sometimes (# speed 2) $ s "hh*16""#, 8, "speed"));
        let kept = times(r#"degradeBy 0.9 $ s "hh*16""#, 8).len();
        assert!(kept < 30, "{kept}");
        // The chosen events take the function's structure.
        let d = times(r#"always (fast 2) $ s "bd""#, 1);
        assert_eq!(d, ["0", "1/2"]);
    }

    #[test]
    fn echoes_and_layers() {
        let g: Vec<String> = ctl(r#"stut 3 0.5 0.125 $ s "bd""#, 1, "gain")
            .iter()
            .map(|g| g.parse::<f64>().map_or(g.clone(), |x| format!("{x:.3}")))
            .collect();
        assert_eq!(g, ["-", "0.833", "0.667"]);
        assert_eq!(ctl(r#"echo 3 0.2 0.5 $ s "bd""#, 1, "gain"), ["-", "0.5", "0.25"]);
        assert_eq!(times(r#"echoWith 3 (1/8) (|* speed 2) $ s "bd""#, 1), ["0", "1/8", "1/4"]);
        assert_eq!(ctl(r#"superimpose (fast 2) $ s "bd""#, 1, "s").len(), 3);
        assert_eq!(ctl(r#"layer [rev, fast 2] $ s "a b""#, 1, "s").len(), 6);
        assert_eq!(times(r#"hurry 2 $ s "bd sn""#, 1).len(), 4);
        assert_eq!(ctl(r#"hurry 2 $ s "bd""#, 1, "speed"), ["2", "2"]);
    }

    #[test]
    fn friendly_errors() {
        let e = |src: &str| {
            eval(&expression(src, 0, src.len()).unwrap(), &Env::default())
                .err()
                .unwrap()
                .message
        };
        assert!(e("evry 4 (fast 2) $ s \"bd\"").contains("did you mean every"));
        assert!(e("s \"bd\" # \"0 1\"").contains("same kind"));
        assert!(e("ur 4 \"a\" [] []").contains("not in Patch's Tidal yet"));
        assert!(e("s \"bd [sn\"").contains("never closed"));
    }
}
