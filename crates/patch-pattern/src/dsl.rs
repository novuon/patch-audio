//! The Pattern language: what a person types in the Pattern surface.
//!
//! One layer per line, a target and its pattern, then transformations:
//!
//! ```text
//! cycle 4                                  // beats per cycle (a 4/4 bar)
//! drums: s("bd*4 [hh hh] sd").every(4, fast(2))
//! hats:  s("hh*8?").sometimes(rev)
//! deck(a).filter("<200 800 1600 400>")
//! stem("vocals").sometimes(mute)           // deck A's vocals; deck(b).stem(…)
//! visual.intensity("<0.2 0.4 0.8 1>")
//! visual.hue(sine.slow(4))
//! scene("<tunnel kaleido>/2")
//! kit("t1", "kick ~ snare ~")              // a temporary kit's slots
//! sample("dm-0123456789abcdef", "1 ~ 0.5 ~")
//! sample("derived:deck-a:vocals:phrase-last:beats-2")   // resolved by the service
//! ```
//!
//! It is parsed into the [`Program`] AST and nothing else: there is no
//! host-language evaluation, no loops, no I/O. A program that doesn't parse
//! is reported line by line and never replaces the one running.

use crate::ast::{Pat, Signal, Transform, Value};
use crate::event::{DeckParam, MidiKind, MixerParam, Target};
use crate::mini;
use crate::program::{Layer, Program};
use crate::time::Time;
use patch_core::command::Deck;
use patch_core::material::{AssetId, KitId, SampleSource};

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct DslError {
    /// 1-based.
    pub line: usize,
    pub col: usize,
    pub message: String,
}

impl std::fmt::Display for DslError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.message)
    }
}

/// Turns a semantic sample address (`derived:…`) into an asset id. The
/// Derived Material service provides one; without it such addresses are an
/// error. Pattern never cuts audio itself.
pub trait Resolver {
    fn resolve(&self, address: &str) -> Result<AssetId, String>;
}

struct NoResolver;
impl Resolver for NoResolver {
    fn resolve(&self, address: &str) -> Result<AssetId, String> {
        Err(format!(
            "'{address}': derived material needs the Patch service"
        ))
    }
}

pub fn parse(src: &str) -> Result<Program, Vec<DslError>> {
    parse_with(src, &NoResolver)
}

pub fn parse_with(src: &str, resolver: &dyn Resolver) -> Result<Program, Vec<DslError>> {
    let mut program = Program::default();
    let mut errors = Vec::new();
    // Each line's random choices start from a seed of their own, so editing
    // one line doesn't reshuffle the others.
    for (n, raw) in src.lines().enumerate() {
        let line = n + 1;
        let code = strip_comment(raw);
        if code.trim().is_empty() {
            continue;
        }
        let mut cx = Cx {
            line,
            seed: (line as u64) << 16,
            resolver,
        };
        match cx.line(code, &mut program) {
            Ok(Some(layer)) => {
                if program.layers.iter().any(|l| l.name == layer.name) {
                    errors.push(DslError {
                        line,
                        col: 1,
                        message: format!("'{}' is already a layer", layer.name),
                    });
                } else {
                    program.layers.push(layer);
                }
            }
            Ok(None) => {}
            Err(e) => errors.push(e),
        }
    }
    if errors.is_empty()
        && let Err(e) = program.validate()
    {
        errors.push(DslError {
            line: 1,
            col: 1,
            message: e,
        });
    }
    if errors.is_empty() {
        Ok(program)
    } else {
        Err(errors)
    }
}

fn strip_comment(s: &str) -> &str {
    // Outside strings: `//` or `#` to the end of the line.
    let b = s.as_bytes();
    let mut quote = None;
    for i in 0..b.len() {
        match (quote, b[i]) {
            (None, q @ (b'"' | b'\'')) => quote = Some(q),
            (Some(q), c) if c == q => quote = None,
            (None, b'#') => return &s[..i],
            (None, b'/') if b.get(i + 1) == Some(&b'/') => return &s[..i],
            _ => {}
        }
    }
    s
}

// ── Tokens ───────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Num(String),
    Str(String),
    Punct(char),
}

#[derive(Clone, Debug)]
struct Token {
    tok: Tok,
    col: usize,
}

fn lex(s: &str, line: usize) -> Result<Vec<Token>, DslError> {
    let b: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let err = |col: usize, m: String| DslError {
        line,
        col,
        message: m,
    };
    while i < b.len() {
        let c = b[i];
        let col = i + 1;
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_alphabetic() || c == '_' {
            let st = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == '_') {
                i += 1;
            }
            out.push(Token {
                tok: Tok::Ident(b[st..i].iter().collect()),
                col,
            });
        } else if c.is_ascii_digit()
            || (c == '-'
                && b.get(i + 1)
                    .is_some_and(|d| d.is_ascii_digit() || *d == '.'))
            || (c == '.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit()))
        {
            let st = i;
            i += 1;
            while i < b.len()
                && (b[i].is_ascii_digit()
                    || b[i] == '.'
                    || (b[i] == '/' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit())))
            {
                i += 1;
            }
            out.push(Token {
                tok: Tok::Num(b[st..i].iter().collect()),
                col,
            });
        } else if c == '"' || c == '\'' {
            let st = i + 1;
            i += 1;
            while i < b.len() && b[i] != c {
                i += 1;
            }
            if i >= b.len() {
                return Err(err(col, "a string is never closed".into()));
            }
            out.push(Token {
                tok: Tok::Str(b[st..i].iter().collect()),
                col,
            });
            i += 1;
        } else if "().,:=".contains(c) {
            out.push(Token {
                tok: Tok::Punct(c),
                col,
            });
            i += 1;
        } else {
            return Err(err(col, format!("unexpected '{c}'")));
        }
    }
    Ok(out)
}

// ── Calls ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct Call {
    name: String,
    col: usize,
    args: Option<Vec<Arg>>,
}

#[derive(Clone, Debug)]
enum Arg {
    Num(String, usize),
    Str(String, usize),
    Chain(Vec<Call>),
}

impl Arg {
    fn col(&self) -> usize {
        match self {
            Arg::Num(_, c) | Arg::Str(_, c) => *c,
            Arg::Chain(c) => c.first().map_or(1, |c| c.col),
        }
    }
}

struct Toks {
    t: Vec<Token>,
    i: usize,
    end_col: usize,
}

impl Toks {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i).map(|t| &t.tok)
    }
    fn col(&self) -> usize {
        self.t.get(self.i).map_or(self.end_col, |t| t.col)
    }
    fn punct(&mut self, c: char) -> bool {
        if self.peek() == Some(&Tok::Punct(c)) {
            self.i += 1;
            true
        } else {
            false
        }
    }
}

struct Cx<'a> {
    line: usize,
    seed: u64,
    resolver: &'a dyn Resolver,
}

impl Cx<'_> {
    fn err(&self, col: usize, m: impl Into<String>) -> DslError {
        DslError {
            line: self.line,
            col,
            message: m.into(),
        }
    }

    fn next_seed(&mut self) -> u64 {
        self.seed += 1;
        self.seed
    }

    fn line(&mut self, code: &str, program: &mut Program) -> Result<Option<Layer>, DslError> {
        let toks = lex(code, self.line)?;
        let mut t = Toks {
            end_col: code.chars().count() + 1,
            t: toks,
            i: 0,
        };
        // Settings: `cycle 4`, `seed 7`.
        if let (Some(Tok::Ident(k)), Some(Tok::Num(v))) =
            (t.t.first().map(|x| &x.tok), t.t.get(1).map(|x| &x.tok))
        {
            let col = t.t[1].col;
            match k.as_str() {
                "cycle" | "bar" => {
                    let beats = Time::parse(v)
                        .filter(|b| *b > Time::ZERO)
                        .ok_or_else(|| self.err(col, "a cycle is more than 0 beats"))?;
                    program.cycle_beats = beats;
                    return Ok(None);
                }
                "seed" => {
                    program.seed = v
                        .parse()
                        .map_err(|_| self.err(col, "a seed is a whole number"))?;
                    return Ok(None);
                }
                _ => {}
            }
        }
        // `name:` (or `name =`) before the layer.
        let mut name = None;
        if let (Some(Tok::Ident(n)), Some(Tok::Punct(':' | '='))) =
            (t.t.first().map(|x| &x.tok), t.t.get(1).map(|x| &x.tok))
        {
            name = Some(n.clone());
            t.i = 2;
        }
        let chain = self.chain(&mut t)?;
        if t.i < t.t.len() {
            return Err(self.err(t.col(), "expected '.' or the end of the line"));
        }
        let (target, pat) = self.layer(chain)?;
        let name = name.unwrap_or_else(|| format!("d{}", program.layers.len() + 1));
        Ok(Some(Layer {
            name,
            target,
            pat,
            muted: false,
        }))
    }

    fn chain(&mut self, t: &mut Toks) -> Result<Vec<Call>, DslError> {
        let mut calls = vec![self.call(t)?];
        while t.punct('.') {
            calls.push(self.call(t)?);
        }
        Ok(calls)
    }

    fn call(&mut self, t: &mut Toks) -> Result<Call, DslError> {
        let col = t.col();
        let Some(Tok::Ident(name)) = t.peek().cloned() else {
            return Err(self.err(col, "expected a name"));
        };
        t.i += 1;
        let args = if t.punct('(') {
            let mut args = Vec::new();
            if !t.punct(')') {
                loop {
                    let acol = t.col();
                    let a = match t.peek().cloned() {
                        Some(Tok::Num(n)) => {
                            t.i += 1;
                            Arg::Num(n, acol)
                        }
                        Some(Tok::Str(s)) => {
                            t.i += 1;
                            Arg::Str(s, acol)
                        }
                        Some(Tok::Ident(_)) => Arg::Chain(self.chain(t)?),
                        _ => return Err(self.err(acol, "expected a value")),
                    };
                    args.push(a);
                    if t.punct(')') {
                        break;
                    }
                    if !t.punct(',') {
                        return Err(self.err(t.col(), "expected ',' or ')'"));
                    }
                }
            }
            Some(args)
        } else {
            None
        };
        Ok(Call { name, col, args })
    }

    // ── Layers: a target, its pattern, then transformations ──────────────

    fn layer(&mut self, calls: Vec<Call>) -> Result<(Target, Pat), DslError> {
        let mut it = calls.into_iter().peekable();
        let head = it.next().unwrap();
        let args = head.args.clone().unwrap_or_default();
        let deck_of = |c: &Call, args: &[Arg], cx: &Cx| -> Result<Deck, DslError> {
            match args.first() {
                Some(Arg::Chain(ch)) if ch.len() == 1 && ch[0].args.is_none() => {
                    parse_deck(&ch[0].name)
                }
                Some(Arg::Str(s, _)) => parse_deck(s),
                _ => None,
            }
            .ok_or_else(|| cx.err(c.col, "deck is a, b, c or d"))
        };
        let (target, pat) = match head.name.as_str() {
            "s" | "sound" => (Target::Sound, self.pattern_arg(&head, args.first())?),
            "n" | "note" => (Target::Note, self.pattern_arg(&head, args.first())?),
            "scene" => (Target::Scene, self.pattern_arg(&head, args.first())?),
            "ai" => (Target::Ai, self.pattern_arg(&head, args.first())?),
            "xfade" | "crossfader" => (Target::Crossfader, self.pattern_arg(&head, args.first())?),
            "control" => {
                let name = self.word_arg(&head, args.first())?;
                (
                    Target::Control { name },
                    self.pattern_arg(&head, args.get(1))?,
                )
            }
            "kit" => {
                let kit = KitId(self.word_arg(&head, args.first())?);
                if !kit.valid() {
                    return Err(self.err(head.col, "a kit name is letters, digits, '-' and '_'"));
                }
                (
                    Target::Sample {
                        source: SampleSource::Kit(kit),
                    },
                    self.pattern_arg(&head, args.get(1))?,
                )
            }
            "sample" => {
                let addr = self.word_arg(&head, args.first())?;
                let id = if addr.starts_with("dm-") {
                    AssetId(addr)
                } else {
                    self.resolver
                        .resolve(&addr)
                        .map_err(|e| self.err(args[0].col(), e))?
                };
                if !id.valid() {
                    return Err(self.err(args[0].col(), "not a derived-material id (dm-…)"));
                }
                let pat = match args.get(1) {
                    Some(a) => self.pattern_arg(&head, Some(a))?,
                    None => Pat::Atom(Value::Num(1.0)),
                };
                (
                    Target::Sample {
                        source: SampleSource::Asset(id),
                    },
                    pat,
                )
            }
            "stem" => {
                let stem = self.word_arg(&head, args.first())?;
                let pat = match args.get(1) {
                    Some(a) => self.pattern_arg(&head, Some(a))?,
                    None => Pat::Atom(Value::atom("unmute")),
                };
                (
                    Target::Stem {
                        deck: Deck::A,
                        stem,
                    },
                    pat,
                )
            }
            "visual" => {
                let p = it.next().ok_or_else(|| {
                    self.err(
                        head.col,
                        "visual.<parameter>(…): intensity, symmetry, hue, …",
                    )
                })?;
                let a = p.args.clone().unwrap_or_default();
                (
                    Target::Visual {
                        param: p.name.clone(),
                    },
                    self.pattern_arg(&p, a.first())?,
                )
            }
            "mixer" => {
                let strip = self.word_arg(&head, args.first())?;
                let p = it
                    .next()
                    .ok_or_else(|| self.err(head.col, "mixer(…).gain(…) or .mute(…)"))?;
                let param = match p.name.as_str() {
                    "gain" => MixerParam::Gain,
                    "mute" => MixerParam::Mute,
                    _ => return Err(self.err(p.col, "mixer(…).gain(…) or .mute(…)")),
                };
                let a = p.args.clone().unwrap_or_default();
                (
                    Target::Mixer { strip, param },
                    self.pattern_arg(&p, a.first())?,
                )
            }
            "midi" => {
                let channel = match args.first() {
                    Some(Arg::Num(n, _)) => n.parse::<u8>().ok().filter(|c| (1..=16).contains(c)),
                    _ => None,
                }
                .ok_or_else(|| self.err(head.col, "midi(1 … 16)"))?;
                let p = it
                    .next()
                    .ok_or_else(|| self.err(head.col, "midi(ch).note(…) or .cc(n, …)"))?;
                let a = p.args.clone().unwrap_or_default();
                match p.name.as_str() {
                    "note" => (
                        Target::Midi {
                            channel,
                            kind: MidiKind::Note,
                        },
                        self.pattern_arg(&p, a.first())?,
                    ),
                    "cc" => {
                        let n = match a.first() {
                            Some(Arg::Num(n, _)) => n.parse::<u8>().ok().filter(|n| *n < 128),
                            _ => None,
                        }
                        .ok_or_else(|| self.err(p.col, "cc(0 … 127, …)"))?;
                        (
                            Target::Midi {
                                channel,
                                kind: MidiKind::Cc(n),
                            },
                            self.pattern_arg(&p, a.get(1))?,
                        )
                    }
                    _ => return Err(self.err(p.col, "midi(ch).note(…) or .cc(n, …)")),
                }
            }
            name if name == "deck" || (name.len() == 5 && name.starts_with("deck")) => {
                let deck = if name == "deck" {
                    deck_of(&head, &args, self)?
                } else {
                    parse_deck(&name[4..])
                        .ok_or_else(|| self.err(head.col, "deck is a, b, c or d"))?
                };
                let p = it.next().ok_or_else(|| {
                    self.err(
                        head.col,
                        "deck(a).<filter|volume|low|mid|high|fx|transport|loop|jump|cue|stem>(…)",
                    )
                })?;
                let a = p.args.clone().unwrap_or_default();
                if p.name == "stem" {
                    let stem = self.word_arg(&p, a.first())?;
                    let pat = match a.get(1) {
                        Some(x) => self.pattern_arg(&p, Some(x))?,
                        None => Pat::Atom(Value::atom("unmute")),
                    };
                    (Target::Stem { deck, stem }, pat)
                } else {
                    let param = DeckParam::parse(&p.name).ok_or_else(|| {
                        self.err(p.col, format!("decks have no '{}' (filter, volume, low, mid, high, fx, transport, loop, jump, cue, stem)", p.name))
                    })?;
                    (
                        Target::Deck { deck, param },
                        self.pattern_arg(&p, a.first())?,
                    )
                }
            }
            other => {
                return Err(self.err(head.col, format!("'{other}' is not a target (s, note, deck, stem, mixer, xfade, visual, scene, kit, sample, midi, control, ai)")));
            }
        };
        let mut pat = pat;
        for call in it {
            pat = pat.apply(self.transform(&call)?);
        }
        Ok((target, pat))
    }

    fn word_arg(&self, c: &Call, a: Option<&Arg>) -> Result<String, DslError> {
        match a {
            Some(Arg::Str(s, _)) => Ok(s.clone()),
            Some(Arg::Chain(ch)) if ch.len() == 1 && ch[0].args.is_none() => Ok(ch[0].name.clone()),
            Some(Arg::Num(n, _)) => Ok(n.clone()),
            _ => Err(self.err(c.col, format!("{}(…) needs a name", c.name))),
        }
    }

    /// A pattern: a mini-notation string, a number, or a signal expression
    /// (`sine.range(0.2, 0.8).slow(4)`).
    fn pattern_arg(&mut self, c: &Call, a: Option<&Arg>) -> Result<Pat, DslError> {
        match a {
            Some(Arg::Str(s, col)) => mini::parse(s, self.next_seed() << 8).map_err(|e| {
                self.err(
                    col + 1 + s.get(..e.at).unwrap_or(s).chars().count(),
                    e.message,
                )
            }),
            Some(Arg::Num(n, col)) => n
                .parse::<f64>()
                .map(|x| Pat::Atom(Value::Num(x)))
                .map_err(|_| self.err(*col, "not a number")),
            Some(Arg::Chain(ch)) => {
                let first = &ch[0];
                let sig = Signal::parse(&first.name).filter(|_| first.args.is_none()).ok_or_else(|| {
                    self.err(first.col, format!("'{}' is not a pattern: use \"…\", a number or a signal (sine, saw, tri, square, rand)", first.name))
                })?;
                let mut p = Pat::Signal(sig);
                for t in &ch[1..] {
                    p = p.apply(self.transform(t)?);
                }
                Ok(p)
            }
            None => Err(self.err(c.col, format!("{}(…) needs a pattern", c.name))),
        }
    }

    fn time_arg(&self, c: &Call, i: usize) -> Result<Time, DslError> {
        match c.args.as_ref().and_then(|a| a.get(i)) {
            Some(Arg::Num(n, col)) | Some(Arg::Str(n, col)) => {
                Time::parse(n).ok_or_else(|| self.err(*col, format!("'{n}' is not a number")))
            }
            _ => Err(self.err(c.col, format!("{} needs a number", c.name))),
        }
    }

    fn num_arg(&self, c: &Call, i: usize) -> Result<f64, DslError> {
        self.time_arg(c, i).map(Time::to_f64)
    }

    fn count_arg(&self, c: &Call, i: usize) -> Result<u32, DslError> {
        let t = self.time_arg(c, i)?;
        if t.den() != 1 || t < Time::ZERO || t > Time::whole(4096) {
            return Err(self.err(c.col, format!("{} needs a whole number 0 … 4096", c.name)));
        }
        Ok(t.num() as u32)
    }

    fn fn_arg(&mut self, c: &Call, i: usize) -> Result<Transform, DslError> {
        match c.args.as_ref().and_then(|a| a.get(i)) {
            Some(Arg::Chain(ch)) => {
                let mut ts = ch
                    .iter()
                    .map(|x| self.transform(x))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(if ts.len() == 1 {
                    ts.remove(0)
                } else {
                    Transform::Chain(ts)
                })
            }
            _ => Err(self.err(
                c.col,
                format!("{} needs a function, e.g. {}(…, fast(2))", c.name, c.name),
            )),
        }
    }

    fn prob_arg(&self, c: &Call, i: usize) -> Result<f64, DslError> {
        let p = self.num_arg(c, i)?;
        if (0.0..=1.0).contains(&p) {
            Ok(p)
        } else {
            Err(self.err(c.col, "a probability is 0 … 1"))
        }
    }

    fn transform(&mut self, c: &Call) -> Result<Transform, DslError> {
        let positive = |cx: &Cx, t: Time| {
            if t > Time::ZERO && t <= Time::whole(1024) {
                Ok(t)
            } else {
                Err(cx.err(
                    c.col,
                    format!("{} takes more than 0 and at most 1024", c.name),
                ))
            }
        };
        let chance = |p: f64, cx: &mut Cx, f: Transform| Transform::Sometimes {
            prob: p,
            seed: cx.next_seed(),
            then: Box::new(f),
        };
        Ok(match c.name.as_str() {
            "fast" | "density" => Transform::Fast(positive(self, self.time_arg(c, 0)?)?),
            "slow" | "sparsity" => Transform::Slow(positive(self, self.time_arg(c, 0)?)?),
            "early" | "rotate" | "rotL" => Transform::Early(self.time_arg(c, 0)?),
            "late" | "rotR" => Transform::Late(self.time_arg(c, 0)?),
            "rev" => Transform::Rev,
            "palindrome" => Transform::Palindrome,
            "ply" => Transform::Ply(self.count_arg(c, 0)?.max(1)),
            "segment" => Transform::Segment(self.count_arg(c, 0)?.max(1)),
            "iter" => Transform::Iter(self.count_arg(c, 0)?.max(1)),
            "euclid" => {
                let steps = self.count_arg(c, 1)?;
                if steps == 0 || steps > 512 {
                    return Err(self.err(c.col, "euclid steps are 1 … 512"));
                }
                Transform::Euclid {
                    pulses: self.count_arg(c, 0)?,
                    steps,
                    rotate: if c.args.as_ref().is_some_and(|a| a.len() > 2) {
                        self.num_arg(c, 2)? as i32
                    } else {
                        0
                    },
                }
            }
            "degrade" => Transform::Degrade {
                prob: 0.5,
                seed: self.next_seed(),
            },
            "degradeBy" | "degrade_by" => Transform::Degrade {
                prob: self.prob_arg(c, 0)?,
                seed: self.next_seed(),
            },
            "every" | "firstOf" => {
                let n = self.count_arg(c, 0)?;
                if n == 0 {
                    return Err(self.err(c.col, "every needs 1 or more"));
                }
                Transform::Every {
                    n,
                    offset: 0,
                    then: Box::new(self.fn_arg(c, 1)?),
                }
            }
            "lastOf" => {
                let n = self.count_arg(c, 0)?.max(1);
                Transform::Every {
                    n,
                    offset: n as i64 - 1,
                    then: Box::new(self.fn_arg(c, 1)?),
                }
            }
            "sometimes" => {
                let f = self.fn_arg(c, 0)?;
                chance(0.5, self, f)
            }
            "often" => {
                let f = self.fn_arg(c, 0)?;
                chance(0.75, self, f)
            }
            "rarely" => {
                let f = self.fn_arg(c, 0)?;
                chance(0.25, self, f)
            }
            "almostNever" => {
                let f = self.fn_arg(c, 0)?;
                chance(0.1, self, f)
            }
            "almostAlways" => {
                let f = self.fn_arg(c, 0)?;
                chance(0.9, self, f)
            }
            "always" => self.fn_arg(c, 0)?,
            "never" => Transform::Id,
            "sometimesBy" => {
                let p = self.prob_arg(c, 0)?;
                let f = self.fn_arg(c, 1)?;
                chance(p, self, f)
            }
            "somecycles" | "somecyclesBy" => {
                let (p, i) = if c.name == "somecycles" {
                    (0.5, 0)
                } else {
                    (self.prob_arg(c, 0)?, 1)
                };
                Transform::SomeCycles {
                    prob: p,
                    seed: self.next_seed(),
                    then: Box::new(self.fn_arg(c, i)?),
                }
            }
            "within" => {
                let (b, e) = (self.time_arg(c, 0)?, self.time_arg(c, 1)?);
                if !(Time::ZERO <= b && b < e && e <= Time::ONE) {
                    return Err(self.err(c.col, "within(begin, end, …) with 0 ≤ begin < end ≤ 1"));
                }
                Transform::Within {
                    begin: b,
                    end: e,
                    then: Box::new(self.fn_arg(c, 2)?),
                }
            }
            "chunk" => Transform::Chunk {
                n: self.count_arg(c, 0)?.max(1),
                then: Box::new(self.fn_arg(c, 1)?),
            },
            "off" => Transform::Off {
                by: self.time_arg(c, 0)?,
                then: Box::new(self.fn_arg(c, 1)?),
            },
            "superimpose" => Transform::Superimpose(Box::new(self.fn_arg(c, 0)?)),
            "add" => Transform::Add(self.num_arg(c, 0)?),
            "mul" => Transform::Mul(self.num_arg(c, 0)?),
            "range" => Transform::Range {
                lo: self.num_arg(c, 0)?,
                hi: self.num_arg(c, 1)?,
            },
            "set" => {
                let v = match c.args.as_ref().and_then(|a| a.first()) {
                    Some(Arg::Num(n, _)) | Some(Arg::Str(n, _)) => Value::atom(n),
                    Some(Arg::Chain(ch)) if ch.len() == 1 => Value::atom(&ch[0].name),
                    _ => return Err(self.err(c.col, "set(value)")),
                };
                Transform::Set(v)
            }
            w @ ("mute" | "unmute" | "solo" | "unsolo") => Transform::Set(Value::atom(w)),
            "id" => Transform::Id,
            other => {
                return Err(self.err(c.col, format!("'{other}' is not a transformation")));
            }
        })
    }
}

fn parse_deck(s: &str) -> Option<Deck> {
    match s.to_ascii_lowercase().as_str() {
        "a" | "1" => Some(Deck::A),
        "b" | "2" => Some(Deck::B),
        "c" | "3" => Some(Deck::C),
        "d" | "4" => Some(Deck::D),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Span;

    const SET: &str = r#"
cycle 4
drums: s("bd*4 [hh hh] sd").every(4, fast(2))
hats:  s("hh*8?").sometimes(rev)    // a comment
deck(a).filter("<200 800 1600 400>")
stem("vocals").sometimes(mute)
visual.intensity("<0.2 0.4 0.8 1>")
visual.symmetry("<2 4 8 16>")
visual.hue(sine.range(0.2, 0.8).slow(4))
deckB.fx("<echo ~ off ~>")
"#;

    #[test]
    fn the_examples_parse() {
        let p = parse(SET).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(p.layers.len(), 8);
        assert_eq!(p.layers[0].name, "drums");
        assert_eq!(p.layers[2].name, "d3");
        assert_eq!(
            p.layers[3].target,
            Target::Stem {
                deck: Deck::A,
                stem: "vocals".into()
            }
        );
        assert_eq!(
            p.layers[4].target,
            Target::Visual {
                param: "intensity".into()
            }
        );
        let c = p.compile();
        let ev = c.events(Span::cycles(0, 4));
        assert!(ev.iter().any(|e| e.target == Target::Sound));
        assert!(
            ev.windows(2).all(|w| w[0].onset() <= w[1].onset()),
            "in time order"
        );
    }

    #[test]
    fn source_and_json_give_the_same_events() {
        let p = parse(SET).unwrap();
        let back: Program = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        let span = Span::cycles(0, 16);
        assert_eq!(p.compile().events(span), back.compile().events(span));
    }

    #[test]
    fn editing_one_line_keeps_the_others_random_choices() {
        let a = parse("s(\"hh*16?\")\ns(\"sd*16?\")").unwrap();
        let b = parse("s(\"hh*16?\")\ns(\"cp*16?\")").unwrap();
        let first = |p: &Program| {
            p.compile()
                .events_where(Span::cycles(0, 4), |_| true)
                .into_iter()
                .filter(|e| e.layer == 0)
                .collect::<Vec<_>>()
        };
        assert_eq!(first(&a), first(&b));
    }

    #[test]
    fn errors_are_located() {
        let e = parse("drums: s(\"bd [sd\")").unwrap_err();
        assert_eq!(e[0].line, 1);
        assert_eq!(e[0].col, 14, "{e:?}");
        let e = parse("s(\"bd\")\ndeck(z).filter(\"0\")\nwobble(\"x\")").unwrap_err();
        assert_eq!(e.len(), 2);
        assert_eq!((e[0].line, e[1].line), (2, 3));
        assert!(parse("s(\"bd\").fast(0)").is_err());
        assert!(parse("s(\"bd\").every(0, rev)").is_err());
        assert!(parse("s(\"bd\")\ns(\"bd\")").is_ok());
        assert!(parse("x: s(\"bd\")\nx: s(\"sd\")").is_err());
        assert!(
            parse("sample(\"derived:deck-a:vocals:phrase-last\")").unwrap_err()[0]
                .message
                .contains("service")
        );
    }

    #[test]
    fn samples_use_typed_ids_and_a_resolver() {
        struct R;
        impl Resolver for R {
            fn resolve(&self, a: &str) -> Result<AssetId, String> {
                assert_eq!(a, "derived:deck-a:vocals:phrase-last");
                Ok(AssetId("dm-00000000000000aa".into()))
            }
        }
        let p = parse_with("sample(\"derived:deck-a:vocals:phrase-last\", \"1 ~ 0.5 ~\")\nkit(t1, \"kick ~ snare ~\")", &R).unwrap();
        assert_eq!(
            p.layers[0].target,
            Target::Sample {
                source: SampleSource::Asset(AssetId("dm-00000000000000aa".into()))
            }
        );
        assert_eq!(
            p.layers[1].target,
            Target::Sample {
                source: SampleSource::Kit(KitId("t1".into()))
            }
        );
    }
}
