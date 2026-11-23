//! Mini-notation: a whole rhythm in one string.
//!
//! ```text
//! bd sd          a sequence: each step an equal share of the cycle
//! [bd bd] sd     a group takes one step
//! <bd sd cp>     one per cycle, in turn
//! bd, hh hh      layers (a stack)
//! bd | sd        one of them each cycle, at random
//! {a b c}%4      polymeter: the steps at 4 per cycle
//! bd*2 sd/2      faster, slower
//! bd!3  bd !     replicate (a lone `!` repeats the step before)
//! bd@3 sd        weight: bd takes 3 of 4 shares;  `_` extends the step before
//! hh?  hh?0.3    dropped half the time / 30 % of the time
//! bd(3,8,2)      Euclidean: 3 onsets over 8 steps, rotated by 2
//! ~              a rest
//! ```
//!
//! Words and numbers are values (`bd`, `0.25`, `c4`, `bd:3`). The result is
//! a [`Pat`]: nothing is evaluated here.

use crate::ast::{Pat, Step, Transform, Value};
use crate::time::Time;

#[derive(Clone, Debug, PartialEq)]
pub struct MiniError {
    /// Byte offset in the string.
    pub at: usize,
    pub message: String,
}

impl std::fmt::Display for MiniError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "at {}: {}", self.at, self.message)
    }
}

/// Parse `src`; each `?` and `|` gets the next seed from `seed` up, so two
/// `?` in one pattern drop different events.
pub fn parse(src: &str, seed: u64) -> Result<Pat, MiniError> {
    let mut p = Parser {
        s: src.as_bytes(),
        i: 0,
        seed,
        depth: 0,
        atoms: None,
    };
    p.all()
}

/// A word of the string and where it is: its value and byte range.
pub type Atom = (Value, usize, usize);

/// [`parse`], with every word's place kept: each atom of the result is
/// `Num(i)`, the index of its word in the table returned beside it. An
/// editor lights a word up while its events play (a step repeated with `!`
/// is the same word, so its copies share an index).
pub fn parse_located(src: &str, seed: u64) -> Result<(Pat, Vec<Atom>), MiniError> {
    let mut p = Parser {
        s: src.as_bytes(),
        i: 0,
        seed,
        depth: 0,
        atoms: Some(Vec::new()),
    };
    let pat = p.all()?;
    Ok((pat, p.atoms.unwrap_or_default()))
}

const MAX_DEPTH: usize = 64;

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    seed: u64,
    depth: usize,
    /// When locating: every word met, in order (see [`parse_located`]).
    atoms: Option<Vec<Atom>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Seq,
    Alt,
}

impl Parser<'_> {
    fn all(&mut self) -> Result<Pat, MiniError> {
        let pat = self.stack(None)?;
        self.ws();
        if self.i < self.s.len() {
            return Err(self.err(format!("unexpected '{}'", self.s[self.i] as char)));
        }
        Ok(pat)
    }

    fn err(&self, message: impl Into<String>) -> MiniError {
        MiniError {
            at: self.i,
            message: message.into(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn ws(&mut self) {
        while self.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn next_seed(&mut self) -> u64 {
        let s = self.seed;
        self.seed += 1;
        s
    }

    /// Sequences separated by `,` (a stack) or `|` (a choice), up to `close`.
    fn stack(&mut self, close: Option<u8>) -> Result<Pat, MiniError> {
        self.layers(close, Kind::Seq)
    }

    fn layers(&mut self, close: Option<u8>, kind: Kind) -> Result<Pat, MiniError> {
        let mut parts = vec![self.sequence(close)?];
        let mut sep = None;
        loop {
            self.ws();
            match self.peek() {
                Some(c @ (b',' | b'|')) => {
                    if sep.is_some_and(|s| s != c) {
                        return Err(self.err("use either ',' or '|' at one level, not both"));
                    }
                    sep = Some(c);
                    self.i += 1;
                    parts.push(self.sequence(close)?);
                }
                _ => break,
            }
        }
        let parts: Vec<Pat> = parts
            .into_iter()
            .map(|steps| match kind {
                Kind::Seq => seq_of(steps),
                Kind::Alt => Pat::Alt(steps.into_iter().map(|s| s.pat).collect()),
            })
            .collect();
        Ok(match (parts.len(), sep) {
            (1, _) => parts.into_iter().next().unwrap(),
            (_, Some(b'|')) => Pat::Choose {
                pats: parts,
                seed: self.next_seed(),
            },
            _ => Pat::Stack(parts),
        })
    }

    fn sequence(&mut self, close: Option<u8>) -> Result<Vec<Step>, MiniError> {
        let mut steps: Vec<Step> = Vec::new();
        loop {
            self.ws();
            // At the end with a bracket open: the caller reports it, at the
            // bracket.
            let Some(c) = self.peek() else { break };
            if Some(c) == close || c == b',' || c == b'|' {
                break;
            }
            if matches!(c, b']' | b'>' | b'}' | b')') {
                return Err(self.err(format!("unexpected '{}'", c as char)));
            }
            // A lone `!` repeats the step before; `_` lengthens it.
            if c == b'!' && self.s.get(self.i + 1).is_none_or(|n| !n.is_ascii_digit()) {
                self.i += 1;
                let last = steps
                    .last()
                    .cloned()
                    .ok_or_else(|| self.err("'!' with nothing to repeat"))?;
                steps.push(last);
                continue;
            }
            if c == b'_'
                && self
                    .s
                    .get(self.i + 1)
                    .is_none_or(|n| n.is_ascii_whitespace())
            {
                self.i += 1;
                let last = steps.last_mut().ok_or(MiniError {
                    at: self.i,
                    message: "'_' with nothing to extend".into(),
                })?;
                last.weight = last.weight + Time::ONE;
                continue;
            }
            self.step(&mut steps)?;
        }
        if steps.is_empty() {
            if self.i >= self.s.len() && close.is_some() {
                return Ok(vec![Step {
                    pat: Pat::Silence,
                    weight: Time::ONE,
                }]);
            }
            return Err(self.err("empty sequence"));
        }
        Ok(steps)
    }

    fn step(&mut self, steps: &mut Vec<Step>) -> Result<(), MiniError> {
        let mut pat = self.term()?;
        let mut weight = Time::ONE;
        let mut copies = 1u32;
        loop {
            match self.peek() {
                Some(b'*') => {
                    self.i += 1;
                    pat = pat.apply(Transform::Fast(self.factor()?));
                }
                Some(b'/') => {
                    self.i += 1;
                    pat = pat.apply(Transform::Slow(self.factor()?));
                }
                Some(b'@') => {
                    self.i += 1;
                    weight = self.factor()?;
                }
                Some(b'!') => {
                    self.i += 1;
                    let n = self.count()?;
                    copies = n.clamp(1, 256);
                }
                Some(b'?') => {
                    self.i += 1;
                    let prob = if self.peek().is_some_and(|c| c.is_ascii_digit() || c == b'.') {
                        self.number()?
                    } else {
                        0.5
                    };
                    if !(0.0..=1.0).contains(&prob) {
                        return Err(self.err("'?' takes 0 … 1"));
                    }
                    pat = pat.apply(Transform::Degrade {
                        prob,
                        seed: self.next_seed(),
                    });
                }
                Some(b'(') => {
                    self.i += 1;
                    let pulses = self.count()?;
                    if !self.eat(b',') {
                        return Err(self.err("Euclid takes (onsets, steps[, rotation])"));
                    }
                    let n = self.count()?;
                    let rotate = if self.eat(b',') {
                        self.ws();
                        self.word_text()?
                            .parse::<i32>()
                            .map_err(|_| self.err("rotation is a whole number"))?
                    } else {
                        0
                    };
                    if !self.eat(b')') {
                        return Err(self.err("missing ')'"));
                    }
                    if n == 0 || n > 512 {
                        return Err(self.err("Euclid steps are 1 … 512"));
                    }
                    pat = pat.apply(Transform::Euclid {
                        pulses,
                        steps: n,
                        rotate,
                    });
                }
                _ => break,
            }
        }
        for _ in 0..copies {
            steps.push(Step {
                pat: pat.clone(),
                weight,
            });
        }
        Ok(())
    }

    fn term(&mut self) -> Result<Pat, MiniError> {
        self.ws();
        let open = self.i;
        let Some(c) = self.peek() else {
            return Err(self.err("expected a step"));
        };
        let nest = |p: &mut Self| {
            p.depth += 1;
            if p.depth > MAX_DEPTH {
                Err(p.err("nested too deeply"))
            } else {
                Ok(())
            }
        };
        let pat = match c {
            b'~' => {
                self.i += 1;
                Pat::Silence
            }
            b'[' => {
                self.i += 1;
                nest(self)?;
                let p = self.stack(Some(b']'))?;
                self.expect(b']', open)?;
                self.depth -= 1;
                p
            }
            b'<' => {
                self.i += 1;
                nest(self)?;
                let p = self.layers(Some(b'>'), Kind::Alt)?;
                self.expect(b'>', open)?;
                self.depth -= 1;
                p
            }
            b'{' => {
                self.i += 1;
                nest(self)?;
                let mut pats = vec![self.sequence(Some(b'}'))?];
                while self.eat(b',') {
                    pats.push(self.sequence(Some(b'}'))?);
                }
                self.expect(b'}', open)?;
                self.depth -= 1;
                let steps = if self.peek() == Some(b'%') {
                    self.i += 1;
                    Some(self.factor()?)
                } else {
                    None
                };
                Pat::Poly {
                    pats: pats
                        .into_iter()
                        .map(|s| s.into_iter().map(|s| s.pat).collect())
                        .collect(),
                    steps,
                }
            }
            _ => {
                let w = self.word_text()?;
                let v = Value::atom(&w);
                match &mut self.atoms {
                    Some(table) => {
                        table.push((v, open, self.i));
                        Pat::Atom(Value::Num((table.len() - 1) as f64))
                    }
                    None => Pat::Atom(v),
                }
            }
        };
        Ok(pat)
    }

    fn expect(&mut self, c: u8, open: usize) -> Result<(), MiniError> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(MiniError {
                at: open,
                message: format!("'{}' is never closed", self.s[open] as char),
            })
        }
    }

    fn word_text(&mut self) -> Result<String, MiniError> {
        let start = self.i;
        while self.peek().is_some_and(|c| {
            c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'#' | b':' | b'-' | b'\'')
        }) {
            self.i += 1;
        }
        if start == self.i {
            return Err(self.err(match self.peek() {
                Some(c) => format!("unexpected '{}'", c as char),
                None => "expected a step".into(),
            }));
        }
        Ok(String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }

    fn factor(&mut self) -> Result<Time, MiniError> {
        let at = self.i;
        let w = self.word_text()?;
        let t = Time::parse(&w).ok_or(MiniError {
            at,
            message: format!("'{w}' is not a number"),
        })?;
        if t <= Time::ZERO {
            return Err(MiniError {
                at,
                message: "must be more than 0".into(),
            });
        }
        Ok(t)
    }

    fn number(&mut self) -> Result<f64, MiniError> {
        let at = self.i;
        let w = self.word_text()?;
        w.parse().map_err(|_| MiniError {
            at,
            message: format!("'{w}' is not a number"),
        })
    }

    fn count(&mut self) -> Result<u32, MiniError> {
        self.ws();
        let at = self.i;
        let w = self.word_text()?;
        w.parse().map_err(|_| MiniError {
            at,
            message: format!("'{w}' is not a whole number"),
        })
    }
}

fn seq_of(steps: Vec<Step>) -> Pat {
    if steps.len() == 1 && steps[0].weight == Time::ONE {
        steps.into_iter().next().unwrap().pat
    } else {
        Pat::Seq(steps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::sample;

    fn play(src: &str, cycles: i64) -> Vec<(String, String)> {
        let p = parse(src, 0).unwrap_or_else(|e| panic!("{src}: {e}"));
        sample(&p, 0, cycles, 0)
            .into_iter()
            .map(|(t, v)| (t.to_string(), v.to_string()))
            .collect()
    }

    fn words(src: &str, cycles: i64) -> Vec<String> {
        play(src, cycles).into_iter().map(|x| x.1).collect()
    }

    fn at(src: &str) -> Vec<String> {
        play(src, 1).into_iter().map(|x| x.0).collect()
    }

    #[test]
    fn the_strudel_example() {
        assert_eq!(
            play("bd*4 [hh hh] sd", 1),
            [
                ("0", "bd"),
                ("1/12", "bd"),
                ("1/6", "bd"),
                ("1/4", "bd"),
                ("1/3", "hh"),
                ("1/2", "hh"),
                ("2/3", "sd")
            ]
            .map(|(a, b)| (a.to_string(), b.to_string()))
        );
    }

    #[test]
    fn groups_alternation_and_layers() {
        assert_eq!(words("<a b c>", 4), ["a", "b", "c", "a"]);
        assert_eq!(words("a <b c>", 2), ["a", "b", "a", "c"]);
        assert_eq!(words("<0.2 0.4 0.8 1>", 4), ["0.2", "0.4", "0.8", "1"]);
        let mut layered = words("a, b c", 1);
        layered.sort();
        assert_eq!(layered, ["a", "b", "c"]);
        assert_eq!(words("<a b, c>", 2).len(), 4);
    }

    #[test]
    fn weights_replicate_and_rests() {
        assert_eq!(at("a@3 b"), ["0", "3/4"]);
        assert_eq!(at("a _ _ b"), ["0", "3/4"]);
        assert_eq!(words("a!3 b", 1), ["a", "a", "a", "b"]);
        assert_eq!(words("a ! b", 1), ["a", "a", "b"]);
        assert_eq!(at("a ~ b ~"), ["0", "1/2"]);
        assert_eq!(words("a/2", 4), ["a", "a"]);
    }

    #[test]
    fn euclid_polymeter_and_chance() {
        assert_eq!(at("bd(3,8)"), ["0", "3/8", "3/4"]);
        assert_eq!(at("bd(3,8,2)").len(), 3);
        assert_eq!(
            words("{a b c}%4", 2),
            ["a", "b", "c", "a", "b", "c", "a", "b"]
        );
        assert_eq!(words("{a b c, d e}", 1).len(), 6);
        let kept = words("hh*16?", 4).len();
        assert!(kept > 16 && kept < 48, "{kept}");
        assert_eq!(words("hh*16?0", 1).len(), 16);
        assert_eq!(words("hh*16?1", 1).len(), 0);
        let chosen = words("a | b", 32);
        assert!(chosen.contains(&"a".into()) && chosen.contains(&"b".into()));
    }

    #[test]
    fn located_words_keep_their_place() {
        let (p, atoms) = parse_located("bd*2 [~ sn:3]!2 <hh 0.5>", 0).unwrap();
        let places: Vec<(String, usize, usize)> =
            atoms.iter().map(|(v, a, b)| (v.to_string(), *a, *b)).collect();
        assert_eq!(
            places,
            [("bd", 0, 2), ("sn:3", 8, 12), ("hh", 17, 19), ("0.5", 20, 23)]
                .map(|(w, a, b)| (w.to_string(), a, b))
        );
        // The atoms are indices into the table; `!` copies share one.
        let idx = words_of(&p, 1);
        assert_eq!(idx, ["0", "0", "1", "1", "2"]);
    }

    fn words_of(p: &Pat, cycles: i64) -> Vec<String> {
        sample(p, 0, cycles, 0).into_iter().map(|x| x.1.to_string()).collect()
    }

    #[test]
    fn errors_point_at_the_problem() {
        let e = parse("bd [sd hh", 0).unwrap_err();
        assert_eq!(e.at, 3);
        assert!(e.message.contains("never closed"), "{e}");
        assert!(parse("bd*0", 0).is_err());
        assert!(parse("bd(3)", 0).is_err());
        assert!(parse("", 0).is_err());
        assert!(parse("a ] b", 0).is_err());
        assert!(parse("a, b | c", 0).is_err());
        let deep = "[".repeat(100) + "a" + &"]".repeat(100);
        assert!(parse(&deep, 0).unwrap_err().message.contains("deeply"));
    }
}
