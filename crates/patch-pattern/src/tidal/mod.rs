//! Tidal: TidalCycles' language, native to Patch.
//!
//! Code is written the way Tidal is written (`d1 $ every 4 (fast 2) $ s
//! "bd*2 [~ sn]" # gain 1.1`), and runs on Patch's own pattern algebra,
//! clock and instrument: no Haskell, no SuperCollider, no other process.
//! Everything here is Patch's own implementation of Tidal's published
//! behaviour; none of Tidal's source is used (see
//! `docs/third-party-engines.md`).
//!
//! * [`lex`], [`parse`]: Haskell's shapes (application, operators with
//!   their fixities, sections, lists, tuples, layout).
//! * [`eval`]: values, the vocabulary (`fast`, `every`, `jux`, `off`, …)
//!   and the controls (`s`, `n`, `gain`, `cutoff`, `room`, …).
//! * [`notes`]: note names, chords and scales.
//! * [`Session`]: what is playing, channel by channel (`d1` … `d16`, `p
//!   "name"`), as Tidal keeps it: evaluating a block changes only the
//!   channels it names; `hush`, `mute`, `solo` and `setcps` act on it.
//! * [`Tidal`]: the [`Backend`] the Tidal Unit's service runs.

pub mod editor;
pub mod eval;
pub mod help;
pub mod lex;
pub mod notes;
pub mod parse;

use crate::backend::{Backend, Capabilities, Definition, LayerInfo, Loaded, PatternError};
use crate::event::{Event, Target};
use crate::pattern::Pattern;
use crate::time::{Span, Time};
use eval::{Env, Kind, V, Val};
use parse::{Expr, Node};
use std::collections::BTreeMap;

/// Channels a session holds at most.
pub const MAX_CHANNELS: usize = 32;

/// Tidal's cycle is four beats: `setcps (bpm/60/4)`.
pub const CYCLE_BEATS: i64 = 4;

/// A statement of Tidal code.
#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    /// `d1 $ …`, `p "name" $ …`: the expression's source range, and how
    /// it takes over (`xfade 1 $ …`).
    Play { channel: String, at: usize, end: usize, how: How },
    /// `once $ …`: one cycle of it, now.
    Once { at: usize, end: usize },
    /// `d1 silence`, `d1 $ silence`.
    Silence(String),
    Hush,
    /// `setcps 0.5`, `setbpm 120` (as cycles per second).
    Cps(f64),
    /// `let name = …`, `let name args = …` (`name` holds the arguments too:
    /// `"wob n"`).
    Let { name: String, at: usize, end: usize },
    Mute(String, bool),
    Solo(String, bool),
    UnmuteAll,
    UnsoloAll,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TidalError {
    /// Byte offset in the code it was found in.
    pub at: usize,
    pub message: String,
}

impl TidalError {
    fn new(at: usize, message: impl Into<String>) -> TidalError {
        TidalError {
            at,
            message: message.into(),
        }
    }

    /// As a located error in `src` (1-based line and column, by character).
    pub fn located(&self, src: &str) -> PatternError {
        let at = self.at.min(src.len());
        let before = &src[..src.floor_char_boundary(at)];
        let line = before.matches('\n').count() + 1;
        let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        PatternError {
            line,
            col,
            message: self.message.clone(),
        }
    }
}

impl From<parse::SyntaxError> for TidalError {
    fn from(e: parse::SyntaxError) -> TidalError {
        TidalError::new(e.at, e.message)
    }
}

impl From<eval::EvalError> for TidalError {
    fn from(e: eval::EvalError) -> TidalError {
        TidalError::new(e.at, e.message)
    }
}

/// `d1` … `d16` (any `dN`), or `p` with a name or number.
/// How a channel's new pattern takes over from the one it had.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum How {
    /// At once (`d1 $ …`, `jump 1 $ …`).
    Now,
    /// `xfade`, `xfadeIn 1 t`: a crossfade over t cycles.
    Xfade(f64),
    /// `clutch`, `clutchIn 1 t`: the old one's events give way to the
    /// new one's at random over t cycles.
    Clutch(f64),
    /// `jumpIn 1 n`: after n cycles.
    Jump(f64),
    /// `jumpIn' 1 n`, `jumpMod 1 n`: at the next cycle that is a multiple
    /// of n.
    JumpMod(f64),
}

/// A channel in the middle of a change: how its code says so. The code is
/// `_xfade START CYCLES (OLD) $ NEW`, `_clutch START CYCLES (OLD) $ NEW`,
/// `_jump START (OLD) $ NEW` or `_once START $ NEW`.
#[derive(Clone, Debug, PartialEq)]
pub struct Transition<'a> {
    pub kind: &'a str,
    pub start: f64,
    /// Cycles it takes (`_jump`: 0, `_once`: 1).
    pub cycles: f64,
    pub old: Option<&'a str>,
    /// The new code, and where it starts in the channel's code.
    pub new: &'a str,
    pub new_at: usize,
}

impl Transition<'_> {
    /// Whether it is over at cycle `now` (`None`: not playing, so over).
    pub fn done(&self, now: Option<f64>) -> bool {
        now.is_none_or(|c| c >= self.start + self.cycles)
    }
}

/// The transition a channel's code is in, if it is in one.
pub fn transition(code: &str) -> Option<Transition<'_>> {
    let lead = code.len() - code.trim_start().len();
    let rest = &code[lead..];
    let (kind, nums) = ["_xfade", "_clutch", "_jump", "_once"]
        .iter()
        .find(|k| rest.starts_with(**k) && rest[k.len()..].starts_with(' '))
        .map(|k| (*k, if matches!(*k, "_jump" | "_once") { 1 } else { 2 }))?;
    let mut i = lead + kind.len();
    let mut num = || -> Option<f64> {
        let t = &code[i..];
        let skip = t.len() - t.trim_start().len();
        let len = t[skip..].find(|c: char| c.is_whitespace()).unwrap_or(t.len() - skip);
        let x = t[skip..skip + len].parse().ok()?;
        i += skip + len;
        Some(x)
    };
    let start = num()?;
    let cycles = match (kind, nums) {
        (_, 2) => num()?,
        ("_once", _) => 1.0,
        _ => 0.0,
    };
    let skip = |i: &mut usize| *i += code[*i..].len() - code[*i..].trim_start().len();
    skip(&mut i);
    let mut old = None;
    if kind != "_once" {
        if !code[i..].starts_with('(') {
            return None;
        }
        // The matching parenthesis (strings may hold any).
        let (mut depth, mut in_str, mut close) = (0i32, false, None);
        for (k, c) in code[i..].char_indices() {
            match c {
                '"' => in_str = !in_str,
                '(' if !in_str => depth += 1,
                ')' if !in_str => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(i + k);
                        break;
                    }
                }
                _ => {}
            }
        }
        let close = close?;
        old = Some(&code[i + 1..close]);
        i = close + 1;
        skip(&mut i);
    }
    if !code[i..].starts_with('$') {
        return None;
    }
    let i = i + 1;
    let new_at = i + code[i..].len() - code[i..].trim_start().len();
    Some(Transition { kind, start, cycles, old, new: &code[new_at..], new_at })
}

/// A cycle as code: exact in 1/4096ths, as patterns count time.
fn cycle_code(x: f64) -> String {
    fmt_num((x * 4096.0).round() / 4096.0)
}

/// The code for a change from `old` to `new` starting at cycle `now`.
fn transition_code(how: How, now: f64, old: &str, new: &str) -> String {
    match how {
        How::Now => new.to_string(),
        How::Xfade(t) => format!("_xfade {} {} ({old}) $ {new}", cycle_code(now), fmt_num(t)),
        How::Clutch(t) => format!("_clutch {} {} ({old}) $ {new}", cycle_code(now), fmt_num(t)),
        How::Jump(n) => format!("_jump {} ({old}) $ {new}", cycle_code(now + n)),
        How::JumpMod(n) => {
            let at = ((now / n).floor() + 1.0) * n;
            format!("_jump {} ({old}) $ {new}", cycle_code(at))
        }
    }
}

/// Channels `once` uses (one cycle each, then gone).
const ONCE: &[&str] = &["once", "once2", "once3", "once4"];

/// The head of a statement as a word and its arguments (`xfadeIn 1 8`).
fn head_words(n: &Node) -> Option<(&str, Vec<&Node>)> {
    match &n.expr {
        Expr::Var(v) => Some((v.as_str(), vec![])),
        Expr::App(f, a) => {
            let (w, mut args) = head_words(f)?;
            args.push(a);
            Some((w, args))
        }
        _ => None,
    }
}

fn channel_of(head: &Node) -> Option<String> {
    match &head.expr {
        Expr::Var(v) => {
            let n = v.strip_prefix('d')?;
            let k: u32 = n.parse().ok()?;
            (1..=MAX_CHANNELS as u32).contains(&k).then(|| format!("d{k}"))
        }
        Expr::App(f, a) if f.expr == Expr::Var("p".into()) => match &a.expr {
            Expr::Str(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            Expr::Num(x) if *x >= 1.0 && x.fract() == 0.0 => Some(format!("d{}", *x as u32)),
            _ => None,
        },
        _ => None,
    }
}

/// `mute 1`, `solo "bass"`: the channel named.
fn channel_arg(a: &Node) -> Option<String> {
    match &a.expr {
        Expr::Num(x) if *x >= 1.0 && x.fract() == 0.0 => Some(format!("d{}", *x as u32)),
        Expr::Str(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Expr::Var(v) => channel_of(a).or_else(|| Some(v.clone())),
        _ => None,
    }
}

/// The head and argument of `f x` or `f $ x`.
fn split_app(n: &Node) -> Option<(&Node, &Node)> {
    match &n.expr {
        Expr::App(f, a) => Some((f, a)),
        Expr::Bin(op, f, a) if op == "$" => Some((f, a)),
        _ => None,
    }
}

fn number(n: &Node, env: &Env) -> Result<f64, TidalError> {
    match eval::eval(n, env)? {
        V::Num(x) => Ok(x),
        other => Err(TidalError::new(
            n.at,
            format!("expected a number, got {}", other.kind_name()),
        )),
    }
}

const STATEMENT_HELP: &str = "a line starts with d1 … d16 (d1 $ s \"bd*4\"), p \"name\", hush, setcps, let, mute or solo";

/// One statement from its text (`clean[at..end]`).
fn statement(clean: &str, at: usize, end: usize, env: &Env) -> Result<Stmt, TidalError> {
    let text = &clean[at..end];
    if let Some(rest) = text.strip_prefix("let ") {
        let Some(eq) = rest.find('=') else {
            return Err(TidalError::new(at, "let needs a name and '=': let beat = s \"bd*4\""));
        };
        let head = rest[..eq].trim();
        let ident = |w: &str| {
            w.starts_with(|c: char| c.is_alphabetic() || c == '_')
                && w.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '\'')
        };
        if head.is_empty() || !head.split_whitespace().all(ident) {
            return Err(TidalError::new(
                at,
                "let names a pattern (let beat = s \"bd*4\") or a function and its arguments (let wob n = fast n . rev)",
            ));
        }
        // The value from its first character: saved as `let name = value`,
        // a space kept here would grow by one on every round trip.
        let after = &rest[eq + 1..];
        let from = at + 4 + eq + 1 + (after.len() - after.trim_start().len());
        return Ok(Stmt::Let {
            name: head.split_whitespace().collect::<Vec<_>>().join(" "),
            at: from,
            end,
        });
    }
    let node = parse::expression(clean, at, end)?;
    if let Expr::Var(v) = &node.expr {
        match v.as_str() {
            "hush" => return Ok(Stmt::Hush),
            "unmuteAll" => return Ok(Stmt::UnmuteAll),
            "unsoloAll" => return Ok(Stmt::UnsoloAll),
            _ => {}
        }
    }
    let Some((head, body)) = split_app(&node) else {
        return Err(TidalError::new(at, STATEMENT_HELP));
    };
    if let Some(channel) = channel_of(head) {
        if body.expr == Expr::Var("silence".into()) {
            return Ok(Stmt::Silence(channel));
        }
        return Ok(Stmt::Play {
            channel,
            at: body.at,
            end: body.end,
            how: How::Now,
        });
    }
    // `once $ …`, and the transitions: `xfadeIn 1 8 $ …`.
    if let Some((f, args)) = head_words(head) {
        let n = |k: usize, what: &str| -> Result<f64, TidalError> {
            let v = number(args[k], env)?;
            if v > 0.0 && v <= 1024.0 {
                Ok(v)
            } else {
                Err(TidalError::new(args[k].at, format!("{f}: {what} is a number of cycles above 0")))
            }
        };
        let arity = match f {
            "once" | "asap" => Some(0),
            "xfade" | "clutch" | "jump" => Some(1),
            "xfadeIn" | "clutchIn" | "jumpIn" | "jumpIn'" | "jumpMod" => Some(2),
            _ => None,
        };
        if let Some(arity) = arity {
            if args.len() != arity {
                let usage = match arity {
                    0 => format!("{f} $ s \"bd sn\""),
                    1 => format!("{f} 1 $ s \"bd sn\""),
                    _ => format!("{f} 1 4 $ s \"bd sn\""),
                };
                return Err(TidalError::new(head.at, format!("{f} is written {usage}")));
            }
            if arity == 0 {
                return Ok(Stmt::Once { at: body.at, end: body.end });
            }
            let channel = channel_arg(args[0])
                .ok_or_else(|| TidalError::new(args[0].at, format!("{f} takes a channel: {f} 1 or {f} \"name\"")))?;
            let how = match f {
                "xfade" => How::Xfade(4.0),
                "clutch" => How::Clutch(2.0),
                "jump" => How::Now,
                "xfadeIn" => How::Xfade(n(1, "the length")?),
                "clutchIn" => How::Clutch(n(1, "the length")?),
                "jumpIn" => How::Jump(n(1, "the wait")?),
                _ => How::JumpMod(n(1, "the multiple")?),
            };
            return Ok(Stmt::Play { channel, at: body.at, end: body.end, how });
        }
    }
    if let Expr::Var(f) = &head.expr {
        let chan = || {
            channel_arg(body).ok_or_else(|| {
                TidalError::new(body.at, format!("{f} takes a channel: {f} 1 or {f} \"name\""))
            })
        };
        match f.as_str() {
            "setcps" => {
                let cps = number(body, env)?;
                if !(0.05..=5.0).contains(&cps) {
                    return Err(TidalError::new(body.at, "setcps takes 0.05 … 5 cycles a second (setcps (128/60/4))"));
                }
                return Ok(Stmt::Cps(cps));
            }
            "setbpm" => {
                let bpm = number(body, env)?;
                if !(30.0..=300.0).contains(&bpm) {
                    return Err(TidalError::new(body.at, "setbpm takes 30 … 300"));
                }
                return Ok(Stmt::Cps(bpm / 60.0 / CYCLE_BEATS as f64));
            }
            "mute" => return Ok(Stmt::Mute(chan()?, true)),
            "unmute" => return Ok(Stmt::Mute(chan()?, false)),
            "solo" => return Ok(Stmt::Solo(chan()?, true)),
            "unsolo" => return Ok(Stmt::Solo(chan()?, false)),
            "anticipate" | "anticipateIn" | "interpolate" | "interpolateIn" => {
                return Err(TidalError::new(head.at, format!("{f} isn't in Patch's Tidal yet; use d1 $ …")));
            }
            _ => {}
        }
    }
    Err(TidalError::new(at, STATEMENT_HELP))
}

/// The statements of some code, in order.
pub fn statements(src: &str, env: &Env) -> Result<(String, Vec<Stmt>), Vec<TidalError>> {
    let (clean, chunks) = parse::statements(src);
    let mut out = vec![];
    let mut errors = vec![];
    for c in chunks {
        match statement(&clean, c.at, c.end, env) {
            Ok(s) => out.push(s),
            Err(e) => errors.push(e),
        }
    }
    if errors.is_empty() {
        Ok((clean, out))
    } else {
        Err(errors)
    }
}

/// What plays: Tidal's state, channel by channel, as text. A session is
/// saved and reloaded as the canonical code [`Session::code`] writes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Session {
    pub cps: Option<f64>,
    /// `let`s, in order.
    pub lets: Vec<(String, String)>,
    /// Channels in the order first played: (name, expression).
    pub channels: Vec<(String, String)>,
    pub muted: Vec<String>,
    pub soloed: Vec<String>,
}

fn toggle(list: &mut Vec<String>, name: &str, on: bool) {
    list.retain(|n| n != name);
    if on {
        list.push(name.to_string());
    }
}

/// How a channel is written: `d3`, or `p "bass"`.
fn channel_code(name: &str) -> String {
    match name.strip_prefix('d').and_then(|n| n.parse::<u32>().ok()) {
        Some(_) => name.to_string(),
        None => format!("p {name:?}"),
    }
}

fn channel_ref(name: &str) -> String {
    match name.strip_prefix('d').and_then(|n| n.parse::<u32>().ok()) {
        Some(k) => k.to_string(),
        None => format!("{name:?}"),
    }
}

impl Session {
    /// The `let`s, bound in order. Each is its own source of places (see
    /// [`Session::sources`]).
    fn env(&self) -> Result<Env, Vec<TidalError>> {
        let mut env = Env::default();
        for (j, (name, code)) in self.lets.iter().enumerate() {
            let node = parse::expression(code, 0, code.len()).map_err(|e| vec![e.into()])?;
            let source = (self.channels.len() + j) as u16;
            let (name, v) = eval::with_source(source, || bind_let(name, node, &env)).map_err(|e| vec![e.into()])?;
            env.bind(&name, v);
        }
        Ok(env)
    }

    /// The code an event's places (`Event::locs`) count in: source `i` is
    /// channel `i`'s expression, then each `let`'s, in order.
    pub fn sources(&self) -> Vec<&str> {
        self.channels
            .iter()
            .map(|c| c.1.as_str())
            .chain(self.lets.iter().map(|l| l.1.as_str()))
            .collect()
    }

    /// A whole script (a saved session) from the start.
    pub fn parse(code: &str) -> Result<Session, Vec<TidalError>> {
        let mut s = Session::default();
        s.apply(code)?;
        Ok(s)
    }

    /// Evaluate a block on top of what plays: each statement in order, the
    /// way Tidal does (`d1` replaces d1, `hush` clears, `let` binds). Every
    /// new expression is evaluated here, so an error is found (and placed
    /// in the block) before anything changes. On error nothing changes.
    pub fn apply(&mut self, block: &str) -> Result<Vec<String>, Vec<TidalError>> {
        self.apply_at(block, None)
    }

    /// Finished transitions settled to their new code, finished `once`s
    /// gone, at cycle `now` (`None`: not playing, so all of them).
    pub fn settle(&mut self, now: Option<f64>) {
        self.channels.retain(|(name, code)| {
            !(ONCE.contains(&name.as_str()) && transition(code).is_some_and(|t| t.done(now)))
        });
        for (_, code) in &mut self.channels {
            while let Some(t) = transition(code)
                && t.kind != "_once"
                && t.done(now)
            {
                *code = t.new.to_string();
            }
        }
    }

    /// [`Session::apply`] at cycle `now` of what plays: transitions start
    /// there and `once` plays from there (`None`: not playing; the new
    /// code takes over at once and `once` waits for the start).
    pub fn apply_at(&mut self, block: &str, now: Option<f64>) -> Result<Vec<String>, Vec<TidalError>> {
        let mut next = self.clone();
        next.settle(now);
        let mut env = next.env()?;
        let (clean, chunks) = parse::statements(block);
        let mut errors = vec![];
        let mut touched = vec![];
        for c in chunks {
            let stmt = match statement(&clean, c.at, c.end, &env) {
                Ok(s) => s,
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            };
            match stmt {
                Stmt::Once { at, end } => {
                    let node = match parse::expression(&clean, at, end) {
                        Ok(n) => n,
                        Err(e) => {
                            errors.push(e.into());
                            continue;
                        }
                    };
                    if let Err(e) = playable(&node, &env, "once", at) {
                        errors.push(e);
                        continue;
                    }
                    let start = now.unwrap_or(0.0);
                    let busy = |n: &str| next.channels.iter().any(|c| c.0 == n);
                    let name = ONCE.iter().find(|n| !busy(n)).unwrap_or(&ONCE[0]).to_string();
                    let text = format!("_once {} $ {}", cycle_code(start), reindent(&clean[at..end]));
                    match next.channels.iter_mut().find(|c| c.0 == name) {
                        Some(c) => c.1 = text,
                        None => {
                            if next.channels.len() >= MAX_CHANNELS {
                                errors.push(TidalError::new(at, format!("at most {MAX_CHANNELS} channels")));
                                continue;
                            }
                            next.channels.push((name.clone(), text))
                        }
                    }
                    touched.push(name);
                }
                Stmt::Play { channel, at, end, how } => {
                    let node = match parse::expression(&clean, at, end) {
                        Ok(n) => n,
                        Err(e) => {
                            errors.push(e.into());
                            continue;
                        }
                    };
                    if let Err(e) = playable(&node, &env, &channel, at) {
                        errors.push(e);
                        continue;
                    }
                    let mut text = reindent(&clean[at..end]);
                    if let Some(now) = now
                        && how != How::Now
                    {
                        let old = next.channels.iter().find(|c| c.0 == channel).map_or("silence", |c| c.1.as_str());
                        text = transition_code(how, now, old, &text);
                    }
                    match next.channels.iter_mut().find(|c| c.0 == channel) {
                        Some(c) => c.1 = text,
                        None => {
                            if next.channels.len() >= MAX_CHANNELS {
                                errors.push(TidalError::new(at, format!("at most {MAX_CHANNELS} channels")));
                                continue;
                            }
                            next.channels.push((channel.clone(), text))
                        }
                    }
                    touched.push(channel);
                }
                Stmt::Silence(channel) => {
                    next.channels.retain(|c| c.0 != channel);
                    touched.push(channel);
                }
                Stmt::Hush => {
                    touched.extend(next.channels.drain(..).map(|c| c.0));
                }
                Stmt::Cps(cps) => next.cps = Some(cps),
                Stmt::Let { name, at, end } => {
                    let node = match parse::expression(&clean, at, end) {
                        Ok(n) => n,
                        Err(e) => {
                            errors.push(e.into());
                            continue;
                        }
                    };
                    match bind_let(&name, node, &env) {
                        Ok((n, v)) => env.bind(&n, v),
                        Err(e) => {
                            errors.push(e.into());
                            continue;
                        }
                    }
                    let text = reindent(&clean[at..end]);
                    match next.lets.iter_mut().find(|l| l.0 == name) {
                        Some(l) => l.1 = text,
                        None => next.lets.push((name, text)),
                    }
                }
                Stmt::Mute(c, on) => toggle(&mut next.muted, &c, on),
                Stmt::Solo(c, on) => toggle(&mut next.soloed, &c, on),
                Stmt::UnmuteAll => next.muted.clear(),
                Stmt::UnsoloAll => next.soloed.clear(),
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        // A changed `let` may break a channel that uses it: the whole
        // session must still compile.
        next.compile().map_err(|e| {
            e.into_iter()
                .map(|e| TidalError::new(0, format!("after this block: {}", e.message)))
                .collect::<Vec<_>>()
        })?;
        *self = next;
        Ok(touched)
    }

    /// The session as Tidal code: what is saved and shown.
    pub fn code(&self) -> String {
        let mut out = String::new();
        if let Some(cps) = self.cps {
            out.push_str(&format!("setcps {}\n", fmt_num(cps)));
        }
        for (name, code) in &self.lets {
            out.push_str(&format!("let {name} = {code}\n"));
        }
        for (name, code) in &self.channels {
            out.push_str(&format!("{} $ {code}\n", channel_code(name)));
        }
        for m in &self.muted {
            out.push_str(&format!("mute {}\n", channel_ref(m)));
        }
        for s in &self.soloed {
            out.push_str(&format!("solo {}\n", channel_ref(s)));
        }
        out
    }

    /// Every channel's pattern, ready to query.
    pub fn compile(&self) -> Result<Compiled, Vec<TidalError>> {
        let env = self.env()?;
        let mut layers = vec![];
        let mut errors = vec![];
        for (i, (name, code)) in self.channels.iter().enumerate() {
            let r = parse::expression(code, 0, code.len())
                .map_err(TidalError::from)
                .and_then(|n| eval::with_source(i as u16, || eval::eval(&n, &env)).map_err(TidalError::from));
            match r {
                Ok(V::Pat(Kind::Control, p)) => layers.push(Channel {
                    name: name.clone(),
                    pat: p,
                    muted: self.muted.contains(name),
                }),
                Ok(_) => errors.push(TidalError::new(0, format!("{name} isn't a control pattern"))),
                Err(e) => errors.push(TidalError::new(0, format!("{name}: {}", e.message))),
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let soloed = self.soloed.iter().filter(|s| self.channels.iter().any(|c| &c.0 == *s)).count() > 0;
        if soloed {
            for l in &mut layers {
                if !self.soloed.contains(&l.name) {
                    l.muted = true;
                }
            }
        }
        Ok(Compiled {
            layers,
            cps: self.cps,
        })
    }
}

/// A `let`'s value: the pattern it names, or (with arguments, `"wob n"`)
/// the function it defines. The name to bind it under, and the value.
fn bind_let(head: &str, body: Node, env: &Env) -> Result<(String, V), eval::EvalError> {
    let mut words = head.split_whitespace();
    let name = words.next().unwrap_or("").to_string();
    let params: Vec<String> = words.map(String::from).collect();
    if params.is_empty() {
        return Ok((name, eval::eval(&body, env)?));
    }
    let at = body.at;
    Ok((name, V::Fun(eval::Func::lambda(params, body, env.clone(), at))))
}

/// Continuation lines indented, so the text stays one statement.
/// Whether a channel's expression plays: a control pattern.
fn playable(node: &Node, env: &Env, channel: &str, at: usize) -> Result<(), TidalError> {
    match eval::eval(node, env) {
        Ok(V::Pat(Kind::Control, _)) => Ok(()),
        Ok(V::Pat(Kind::Plain, _) | V::Num(_)) => Err(TidalError::new(
            at,
            format!("{channel} plays sounds: give it a control pattern, like s \"bd*4\" or n \"0 2\" # s \"superpiano\""),
        )),
        Ok(other) => Err(TidalError::new(
            at,
            format!("{channel} needs a pattern, this is {} (an argument missing?)", other.kind_name()),
        )),
        Err(e) => Err(e.into()),
    }
}

fn reindent(text: &str) -> String {
    let mut lines = text.lines();
    let mut out = lines.next().unwrap_or("").trim_end().to_string();
    for l in lines {
        if l.trim().is_empty() {
            continue;
        }
        out.push('\n');
        if !l.starts_with([' ', '\t']) {
            out.push_str("  ");
        }
        out.push_str(l.trim_end());
    }
    out
}

fn fmt_num(x: f64) -> String {
    let s = format!("{x:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

pub struct Channel {
    pub name: String,
    pub pat: Pattern<Val>,
    pub muted: bool,
}

/// A session ready to query.
pub struct Compiled {
    pub layers: Vec<Channel>,
    pub cps: Option<f64>,
}

impl Compiled {
    pub fn events(&self, span: Span) -> Vec<Event> {
        let mut out = vec![];
        for (i, l) in self.layers.iter().enumerate() {
            if l.muted {
                continue;
            }
            for h in l.pat.onsets(span, 0) {
                let (Some(whole), Val::C(mut c, loc)) = (h.whole, h.value) else { continue };
                let s = c.remove("s").unwrap_or(crate::ast::Value::Text(String::new()));
                out.push(Event {
                    layer: i as u16,
                    whole,
                    target: Target::Instrument,
                    value: s,
                    params: c,
                    loc: None,
                    locs: loc.ranges().iter().map(|&(s, a, b)| [s as u32, a as u32, b as u32]).collect(),
                });
            }
        }
        out.sort_by(|a, b| a.onset().cmp(&b.onset()).then(a.layer.cmp(&b.layer)));
        out
    }

    /// What each channel plays over its first cycles, and what Patch can't
    /// play of it: shown when the code is evaluated.
    pub fn survey(&self, cycles: i64) -> (Vec<LayerInfo>, Vec<String>) {
        let mut infos = vec![];
        let mut warnings = vec![];
        let folders = crate::samples::index();
        for l in &self.layers {
            let mut sounds: Vec<String> = vec![];
            let mut unknown: Vec<String> = vec![];
            let mut unused: Vec<String> = vec![];
            for h in l.pat.onsets(Span::cycles(0, cycles), 0) {
                let Val::C(c, _) = h.value else { continue };
                let name = c.get("s").map(|v| v.to_string()).unwrap_or_default();
                let n = c.get("n").and_then(crate::ast::Value::num).unwrap_or(0.0);
                if name.is_empty() {
                    if !unknown.contains(&"(no s)".to_string()) {
                        unknown.push("(no s)".into());
                    }
                } else if crate::instrument::sound(&name, n).is_none() && folders.get(&name).is_none() {
                    if !unknown.contains(&name) {
                        unknown.push(name.clone());
                    }
                } else if !sounds.contains(&name) && sounds.len() < 8 {
                    sounds.push(name.clone());
                }
                for k in c.keys() {
                    if !crate::instrument::USED.contains(&k.as_str()) && !unused.contains(k) {
                        unused.push(k.clone());
                    }
                }
            }
            let mut target = if sounds.is_empty() { "silent".to_string() } else { sounds.join(" ") };
            if l.muted {
                target.push_str(" · muted");
            }
            infos.push(LayerInfo {
                name: l.name.clone(),
                target,
            });
            for u in unknown {
                warnings.push(if u == "(no s)" {
                    format!("{}: events without a sound; add # s \"…\"", l.name)
                } else {
                    let hint = suggest_sound(&u).map(|s| format!(" (try {s})")).unwrap_or_default();
                    format!("{}: Patch has no sound '{u}'{hint}", l.name)
                });
            }
            if !unused.is_empty() {
                warnings.push(format!("{}: {} change nothing yet", l.name, unused.join(", ")));
            }
        }
        (infos, warnings)
    }
}

/// The closest sound name, for a word Patch has no sound for.
fn suggest_sound(name: &str) -> Option<&'static str> {
    let (drums, synths) = crate::instrument::names();
    let lower = name.to_ascii_lowercase();
    drums
        .into_iter()
        .chain(synths)
        .find(|s| lower.contains(*s) && s.len() >= 2)
}

/// Patch's native Tidal engine: Tidal code in, events out.
#[derive(Default)]
pub struct Tidal {
    slots: BTreeMap<u32, Loadout>,
}

enum Loadout {
    Tidal(Compiled),
    Program(crate::program::Compiled),
}

impl Backend for Tidal {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            engine: "patch-tidal".into(),
            version: patch_core::VERSION.into(),
            languages: vec!["tidal".into()],
            program: true,
            euclidean: true,
            polymeter: true,
            probability: true,
            deterministic: true,
            source_locations: false,
            sandboxed: true,
            out_of_process: false,
            unsupported: vec![],
        }
    }

    fn load(&mut self, slot: u32, def: &Definition) -> Result<Loaded, Vec<PatternError>> {
        let t0 = std::time::Instant::now();
        match def {
            Definition::Program { program } => {
                program.validate().map_err(|e| vec![PatternError::new(e)])?;
                let layers = program
                    .layers
                    .iter()
                    .map(|l| LayerInfo {
                        name: l.name.clone(),
                        target: l.target.label(),
                    })
                    .collect();
                self.slots.insert(slot, Loadout::Program(program.compile()));
                Ok(Loaded {
                    layers,
                    eval_us: t0.elapsed().as_micros() as u64,
                    ..Default::default()
                })
            }
            Definition::Source { language, code, .. } => {
                if language != "tidal" {
                    return Err(vec![PatternError::new(format!(
                        "the Tidal engine speaks 'tidal', not '{language}'"
                    ))]);
                }
                let located = |e: Vec<TidalError>| e.iter().map(|e| e.located(code)).collect::<Vec<_>>();
                let session = Session::parse(code).map_err(located)?;
                let compiled = session.compile().map_err(located)?;
                let (layers, warnings) = compiled.survey(8);
                let bpm = compiled.cps.map(|c| c * 60.0 * CYCLE_BEATS as f64);
                self.slots.insert(slot, Loadout::Tidal(compiled));
                Ok(Loaded {
                    layers,
                    eval_us: t0.elapsed().as_micros() as u64,
                    bpm,
                    warnings,
                })
            }
        }
    }

    fn query(&mut self, slot: u32, span: Span) -> Result<Vec<Event>, PatternError> {
        match self.slots.get(&slot) {
            Some(Loadout::Tidal(c)) => Ok(c.events(span)),
            Some(Loadout::Program(c)) => Ok(c.events(span)),
            None => Err(PatternError::new("nothing loaded")),
        }
    }

    fn unload(&mut self, slot: u32) {
        self.slots.remove(&slot);
    }
}

/// What Ctrl+Enter evaluates: the selection, else the block of lines
/// around the cursor (blank lines separate blocks; on a blank line, the
/// block above). Byte range, and the first and last line (0-based).
pub fn block_at(code: &str, anchor: usize, cursor: usize) -> (usize, usize, usize, usize) {
    let fix = |i: usize| code.floor_char_boundary(i.min(code.len()));
    let (a, c) = (fix(anchor), fix(cursor));
    let line_of = |i: usize| code[..i].matches('\n').count();
    if a != c {
        let (b, e) = (a.min(c), a.max(c));
        return (b, e, line_of(b), line_of(e));
    }
    let lines: Vec<(usize, &str)> = code
        .split_inclusive('\n')
        .scan(0, |at, l| {
            let start = *at;
            *at += l.len();
            Some((start, l))
        })
        .collect();
    if lines.is_empty() {
        return (0, 0, 0, 0);
    }
    let blank = |k: usize| lines[k].1.trim().is_empty();
    let mut k = line_of(c).min(lines.len() - 1);
    while k > 0 && blank(k) {
        k -= 1;
    }
    if blank(k) {
        return (c, c, k, k);
    }
    let (mut first, mut last) = (k, k);
    while first > 0 && !blank(first - 1) {
        first -= 1;
    }
    while last + 1 < lines.len() && !blank(last + 1) {
        last += 1;
    }
    let begin = lines[first].0;
    let end = lines[last].0 + lines[last].1.trim_end_matches('\n').len();
    (begin, end, first, last)
}

/// Seconds an event lasts at `cps` cycles a second.
pub fn seconds(whole: Span, cps: f64) -> f64 {
    whole.duration().to_f64() / cps.max(1e-3)
}

/// Cycles per second at `bpm` (Tidal's four-beat cycle).
pub fn cps(bpm: f64) -> f64 {
    bpm / 60.0 / CYCLE_BEATS as f64
}

/// One cycle as a Patch time (for tests and previews).
pub fn cycle(n: i64) -> Time {
    Time::whole(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn let_functions_and_do_blocks() {
        let s = Session::parse(
            "let wob n = (# pan 0) . fast n\nlet two a b = stack [a, b]\ndo d1 $ wob 2 $ s \"bd\"\n   d2 $ two (s \"sn\") (s \"hh\")\n     # gain 0.8",
        )
        .unwrap();
        assert_eq!(s.channels.len(), 2, "{:?}", s.channels);
        assert_eq!(s.channels[1].1, "two (s \"sn\") (s \"hh\")\n     # gain 0.8");
        let ev = s.compile().unwrap().events(Span::cycles(0, 1));
        assert_eq!(ev.iter().filter(|e| e.layer == 0).count(), 2);
        assert_eq!(ev.iter().filter(|e| e.layer == 1).count(), 2);
        // Round trip: the session as code parses back the same.
        assert_eq!(Session::parse(&s.code()).unwrap(), s);
        // A function's own words count in its let's source.
        let src = s.sources();
        let w: Vec<&str> = ev
            .iter()
            .filter(|e| e.layer == 0)
            .flat_map(|e| e.locs.iter().map(|[k, a, b]| &src[*k as usize][*a as usize..*b as usize]))
            .collect();
        assert_eq!(w, ["bd", "bd"]);
        assert!(Session::parse("let 2x = s \"bd\"").is_err());
    }

    fn sounds_at(s: &Session, from: i64, to: i64) -> Vec<(f64, String, f64)> {
        s.compile()
            .unwrap()
            .events(Span::cycles(from, to))
            .iter()
            .map(|e| (e.onset().to_f64(), e.value.to_string(), e.param("gain").unwrap_or(1.0)))
            .collect()
    }

    #[test]
    fn lambdas_in_place() {
        let s = Session::parse("d1 $ every 2 (\\x -> x # speed 2) $ s \"bd\"\nd2 $ (\\p n -> fast n p) (s \"hh\") 2").unwrap();
        let ev = s.compile().unwrap().events(Span::cycles(0, 2));
        let speeds: Vec<_> = ev.iter().filter(|e| e.layer == 0).map(|e| e.param("speed")).collect();
        assert_eq!(speeds, [Some(2.0), None]);
        assert_eq!(ev.iter().filter(|e| e.layer == 1).count(), 4);
        assert_eq!(Session::parse(&s.code()).unwrap(), s);
        assert!(Session::parse("d1 $ \\ -> s \"bd\"").is_err());
    }

    #[test]
    fn transitions_and_once() {
        let mut s = Session::parse("d1 $ s \"bd*2\"\nd2 $ s \"hh\"").unwrap();
        // At cycle 10: a four-cycle crossfade on d1, a jump on d2 at 12.
        s.apply_at("xfade 1 $ s \"sn*2\"\njumpIn' 2 4 $ s \"cp\"", Some(10.5)).unwrap();
        assert_eq!(s.channels[0].1, "_xfade 10.5 4 (s \"bd*2\") $ s \"sn*2\"");
        assert_eq!(s.channels[1].1, "_jump 12 (s \"hh\") $ s \"cp\"");
        assert_eq!(Session::parse(&s.code()).unwrap(), s, "round trip");
        let at = |from, to| sounds_at(&s, from, to);
        // Before the fade: bd at full, no sn; halfway both, quieter; after: sn.
        let c10 = at(10, 11);
        assert!(c10.contains(&(10.0, "bd".into(), 1.0)) && !c10.iter().any(|e| e.1 == "sn"));
        let c12 = at(12, 13);
        let bd = c12.iter().find(|e| e.1 == "bd").unwrap().2;
        let sn = c12.iter().find(|e| e.1 == "sn").unwrap().2;
        assert!(bd < 1.0 && sn < 1.0 && bd > 0.5 && sn > 0.5, "{bd} {sn}");
        assert!(at(15, 16).iter().all(|e| e.1 != "bd"));
        // d2: hh until 12, cp from 12.
        assert!(at(11, 12).iter().any(|e| e.1 == "hh") && !at(11, 12).iter().any(|e| e.1 == "cp"));
        assert!(at(12, 13).iter().any(|e| e.1 == "cp") && !at(12, 13).iter().any(|e| e.1 == "hh"));
        // The next evaluation after they end settles them to the new code.
        s.apply_at("d3 $ s \"rim\"", Some(16.0)).unwrap();
        assert_eq!(s.channels[0].1, "s \"sn*2\"");
        assert_eq!(s.channels[1].1, "s \"cp\"");
        // clutch: each event is the old or the new one, never both.
        let mut c = Session::parse("d1 $ s \"bd*8\"").unwrap();
        c.apply_at("clutchIn 1 4 $ s \"sn*8\"", Some(0.0)).unwrap();
        let ev = sounds_at(&c, 0, 4);
        assert_eq!(ev.len(), 32);
        assert!(ev[..8].iter().filter(|e| e.1 == "bd").count() > ev[24..].iter().filter(|e| e.1 == "bd").count());
        // once: one cycle, from now, on a channel of its own.
        let mut o = Session::parse("d1 $ s \"bd\"").unwrap();
        o.apply_at("once $ s \"cp*2\"", Some(3.25)).unwrap();
        assert_eq!(o.channels[1].0, "once");
        let cps: Vec<f64> = sounds_at(&o, 0, 8).into_iter().filter(|e| e.1 == "cp").map(|e| e.0).collect();
        assert_eq!(cps, [3.25, 3.75]);
        o.apply_at("once $ s \"rim\"", Some(3.5)).unwrap();
        assert_eq!(o.channels[2].0, "once2", "both play");
        o.apply_at("d1 $ s \"bd\"", Some(5.0)).unwrap();
        assert_eq!(o.channels.len(), 1, "gone when done");
        // Not playing: the new code at once, once waits for the start.
        let mut n = Session::parse("d1 $ s \"bd\"").unwrap();
        n.apply_at("xfadeIn 1 8 $ s \"sn\"", None).unwrap();
        assert_eq!(n.channels[0].1, "s \"sn\"");
        assert!(Session::parse("xfadeIn 1 $ s \"sn\"").unwrap_err()[0].message.contains("xfadeIn 1 4"));
        assert!(Session::parse("anticipate 1 $ s \"sn\"").is_err());
    }

    #[test]
    fn a_transition_finds_its_new_code() {
        let t = transition("_xfade 2 4 (_jump 1 (s \"(\") $ s \"a\") $ s \"b\"\n  # gain 1").unwrap();
        assert_eq!((t.kind, t.start, t.cycles), ("_xfade", 2.0, 4.0));
        assert_eq!(t.old, Some("_jump 1 (s \"(\") $ s \"a\""));
        assert_eq!(t.new, "s \"b\"\n  # gain 1");
        assert!(transition("s \"bd\"").is_none());
        assert!(transition("_once 3 $ s \"x\"").is_some_and(|t| t.old.is_none() && t.cycles == 1.0));
        // The editor finds the channel by what was typed.
        let kept = "_xfade 2 4 (s \"a\") $ s \"b c\"";
        let m = editor::SourceMap::find(kept, "d1 $ s \"b c\"", 0).unwrap();
        let c = kept.find('c').unwrap();
        assert_eq!(m.range(c, c + 1), Some((10, 11)));
        assert_eq!(m.range(kept.find('a').unwrap(), kept.find('a').unwrap() + 1), None);
    }

    #[test]
    fn events_know_the_words_they_came_from() {
        let s = Session::parse(
            "let hats = s \"hh*2\"\nd1 $ s \"bd*2 sn\" # n \"<0 3>\"\nd2 $ every 2 (fast 2) hats # gain \"0 .. 3\"",
        )
        .unwrap();
        let src = s.sources();
        let words = |e: &Event| -> Vec<&str> {
            e.locs.iter().map(|[k, a, b]| &src[*k as usize][*a as usize..*b as usize]).collect()
        };
        let ev = s.compile().unwrap().events(Span::cycles(0, 1));
        let d1: Vec<Vec<&str>> = ev.iter().filter(|e| e.layer == 0).map(words).collect();
        assert_eq!(d1, [vec!["bd", "0"], vec!["bd", "0"], vec!["sn", "0"]]);
        // A let's words are its own source; a range lights up whole.
        let d2: Vec<Vec<&str>> = ev.iter().filter(|e| e.layer == 1).map(words).collect();
        assert_eq!(d2.len(), 4, "{d2:?}");
        assert!(d2.iter().all(|w| w == &["hh", "0 .. 3"]), "{d2:?}");
    }

    fn sounds(s: &Session, cycles: i64) -> Vec<String> {
        s.compile()
            .unwrap()
            .events(Span::cycles(0, cycles))
            .into_iter()
            .map(|e| format!("{}:{}", s.compile().unwrap().layers[e.layer as usize].name, e.value))
            .collect()
    }

    #[test]
    fn a_block_changes_only_the_channels_it_names() {
        let mut s = Session::default();
        s.apply("d1 $ s \"bd*2\"\nd2 $ s \"hh\"").unwrap();
        assert_eq!(sounds(&s, 1), ["d1:bd", "d2:hh", "d1:bd"]);
        s.apply("d2 $ s \"sn\"").unwrap();
        assert_eq!(sounds(&s, 1), ["d1:bd", "d2:sn", "d1:bd"]);
        s.apply("d1 silence").unwrap();
        assert_eq!(sounds(&s, 1), ["d2:sn"]);
        s.apply("hush").unwrap();
        assert!(sounds(&s, 1).is_empty());
    }

    #[test]
    fn errors_leave_everything_playing_and_point_into_the_block() {
        let mut s = Session::default();
        s.apply("d1 $ s \"bd\"").unwrap();
        let block = "d2 $ s \"hh\"\n\nd1 $ fast 2 $ s \"sn\" # gian 1";
        let e = s.apply(block).unwrap_err();
        assert_eq!(e.len(), 1);
        let loc = e[0].located(block);
        assert_eq!((loc.line, loc.col), (3, 24));
        assert!(e[0].message.contains("did you mean gain"), "{}", e[0].message);
        // Nothing changed, d2 included.
        assert_eq!(sounds(&s, 1), ["d1:bd"]);
    }

    #[test]
    fn the_session_round_trips_as_code() {
        let mut s = Session::default();
        s.apply(
            "setcps (130/60/4)\nlet beat = s \"bd*4\"\nd1 $ beat\n  # gain 1.1\np \"hats\" $ s \"hh*8\"\nmute 1\nsolo \"hats\"",
        )
        .unwrap();
        let code = s.code();
        assert!(code.contains("setcps 0.541667"), "{code}");
        assert!(code.contains("d1 $ beat\n  # gain 1.1"), "{code}");
        assert!(code.contains("p \"hats\" $ s \"hh*8\""), "{code}");
        let back = Session::parse(&code).unwrap();
        assert_eq!(back.channels, s.channels);
        assert_eq!(back.muted, ["d1"]);
        assert_eq!(back.soloed, ["hats"]);
        assert_eq!(sounds(&back, 1).len(), 8, "solo: only the hats");
    }

    #[test]
    fn mute_solo_and_let() {
        let mut s = Session::default();
        s.apply("let kick = s \"bd*2\"\nd1 $ kick\nd2 $ s \"hh\"").unwrap();
        s.apply("mute 2").unwrap();
        assert_eq!(sounds(&s, 1), ["d1:bd", "d1:bd"]);
        s.apply("unmute 2\nsolo 2").unwrap();
        assert_eq!(sounds(&s, 1), ["d2:hh"]);
        s.apply("unsoloAll").unwrap();
        assert_eq!(sounds(&s, 1).len(), 3);
        // A let that breaks a channel is refused as a whole.
        assert!(s.apply("let kick = \"0 1\"").is_err());
        assert_eq!(sounds(&s, 1).len(), 3);
    }

    #[test]
    fn statements_that_are_not_tidal_say_what_is() {
        let mut s = Session::default();
        let e = s.apply("s \"bd\"").unwrap_err();
        assert!(e[0].message.contains("d1"), "{}", e[0].message);
        let e = s.apply("d1 $ \"bd sn\"").unwrap_err();
        assert!(e[0].message.contains("control pattern"), "{}", e[0].message);
        let e = s.apply("d1 $ fast 2").unwrap_err();
        assert!(e[0].message.contains("argument missing"), "{}", e[0].message);
    }

    #[test]
    fn the_block_under_the_cursor() {
        let code = "d1 $ s \"bd\"\n  # gain 1\n\nd2 $ s \"hh\"\n\n\nd3 $ s \"cp\"";
        let at = |needle: &str| code.find(needle).unwrap();
        let (b, e, f, l) = block_at(code, at("gain"), at("gain"));
        assert_eq!(&code[b..e], "d1 $ s \"bd\"\n  # gain 1");
        assert_eq!((f, l), (0, 1));
        let (b, e, f, _) = block_at(code, at("hh"), at("hh"));
        assert_eq!(&code[b..e], "d2 $ s \"hh\"");
        assert_eq!(f, 3);
        // On a blank line: the block above.
        let blank = at("d3") - 1;
        let (b, e, _, _) = block_at(code, blank, blank);
        assert_eq!(&code[b..e], "d2 $ s \"hh\"");
        // A selection is what's selected.
        let (b, e, _, _) = block_at(code, at("d2"), at("d3") + 2);
        assert_eq!(&code[b..e], "d2 $ s \"hh\"\n\n\nd3");
        assert_eq!(block_at("", 0, 0), (0, 0, 0, 0));
    }

    /// Lines as Tidal's own tutorials write them: every one evaluates and
    /// plays something Patch has a sound for.
    #[test]
    fn classic_lines_play() {
        let lines = [
            r#"d1 $ sound "bd*2 [~ bd] [~ bd] bd""#,
            r#"d1 $ s "bd*4" # gain (range 0.8 1.2 rand)"#,
            r#"d1 $ every 3 (0.25 <~) $ s "bd*2 cp*2 hh sn""#,
            r#"d1 $ whenmod 8 6 (rev) $ sound "bd*2 arpy*2 cp hh*4""#,
            r#"d1 $ jux (iter 4) $ sound "arpy arpy:1 arpy:2 arpy:3""#,
            r#"d1 $ n (off 0.25 (+ 12) $ off 0.125 (+ 7) $ "c(3,8) a(3,8,2) f(3,8) e(3,8,4)") # sound "superpiano" # legato 2"#,
            r#"d1 $ sometimesBy 0.3 (# speed 2) $ s "hh*8""#,
            r#"d1 $ chunk 4 (hurry 2) $ s "bd sn:2 [~ bd] sn:2""#,
            r#"d1 $ stut 4 0.6 (1/8) $ s "bd sn""#,
            r#"d1 $ n "0 .. 7" # s "superpiano" # octave "<4 5>""#,
            r#"d1 $ note (scale "major" "0 2 4 6") # s "supersaw" # cutoff 1200 # room 0.3 # sz 0.8"#,
            r#"d1 $ slow 2 $ s "superpiano" <| n "c'maj e'min""#,
            r#"d1 $ s "drum*8" # n (irand 8)"#,
            r#"d1 $ rarely (# crush 4) $ s "bd*8""#,
            r#"d1 $ fast "<1 [2 4]>" $ s "hh""#,
            r#"d1 $ ply "<1 2 3>" $ s "bd sn""#,
            r#"d1 $ (# pan sine) $ s "hh*8""#,
            r#"d1 $ s "bd*2" # delay 0.5 # delaytime 0.25 # delayfeedback 0.5"#,
            r#"d1 $ arp "<up downup>" $ n "<c'maj7 e'min9>" # s "pluck""#,
            r#"d1 $ euclidFull 3 8 (s "bd") (s "hh27")"#,
            r#"d1 $ sew "t <f t> <t f> f" (n "0 .. 7" # s "superfm") (s "cp*8")"#,
            "d1 $ s \"bd*4\"\n  # gain 1.1\n  # speed \"1 2\"",
            r#"d1 $ rot "<1 2>" $ n "0 2 4 7" # s "superpiano""#,
            r#"d1 $ inside 2 rev $ n "0 1 2 3 4 5 6 7" # s "supersquare""#,
            r#"d1 $ swingBy (1/3) 4 $ s "hh*8""#,
            r#"d1 $ layer [rev, fast 2] $ s "arpy*2 sn""#,
            r#"d1 $ shuffle 4 $ n "0 1 2 3" # s "drum""#,
        ];
        for line in lines {
            let mut s = Session::default();
            s.apply(line).unwrap_or_else(|e| panic!("{line}: {}", e[0].message));
            let c = s.compile().unwrap();
            let ev = c.events(Span::cycles(0, 4));
            assert!(!ev.is_empty(), "{line}: silent");
            for e in &ev {
                crate::instrument::play(e, 0.5).unwrap_or_else(|why| panic!("{line}: {why}"));
            }
        }
    }

    #[test]
    fn the_engine_loads_and_surveys() {
        let mut t = Tidal::default();
        let code = "setbpm 120\nd1 $ s \"bd arpeggio\" # vowel \"a\"\nd2 $ n \"0 4\" # s \"superpiano\"";
        let l = t
            .load(
                0,
                &Definition::Source {
                    language: "tidal".into(),
                    code: code.into(),
                    cycle_beats: Time::whole(4),
                },
            )
            .unwrap();
        assert_eq!(l.bpm, Some(120.0));
        assert_eq!(l.layers.len(), 2);
        assert!(l.warnings.iter().any(|w| w.contains("no sound 'arpeggio'")), "{:?}", l.warnings);
        assert!(l.warnings.iter().any(|w| w.contains("vowel")), "{:?}", l.warnings);
        let ev = t.query(0, Span::cycles(0, 1)).unwrap();
        assert_eq!(ev.len(), 4);
        assert!(ev.iter().all(|e| e.target == Target::Instrument));
        let piano = ev.iter().find(|e| e.value == crate::ast::Value::Text("superpiano".into())).unwrap();
        let p = crate::instrument::play(piano, seconds(piano.whole, 0.5)).unwrap();
        assert_eq!(p.params.note, 60.0);
        assert_eq!(p.params.sustain, 1.0);
    }
}
