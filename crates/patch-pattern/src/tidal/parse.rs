//! Tidal code as expressions: Haskell's application, operators with their
//! fixities, sections (`(# speed 2)`, `(0.25 <~)`), lists and tuples.
//! Statements start at the beginning of a line; an indented line carries on
//! the statement above (Haskell's layout, the way Tidal is written).

use super::lex::{LexError, Tok, Token, blank_comments, lex};

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Num(f64),
    Str(String),
    Var(String),
    App(Box<Node>, Box<Node>),
    /// `a op b` (`op` as written: `#`, `|+|`, `$`, `.`, `<~`, …).
    Bin(String, Box<Node>, Box<Node>),
    /// `(a op)`: waits for its right operand.
    SectionL(Box<Node>, String),
    /// `(op b)`: waits for its left operand.
    SectionR(String, Box<Node>),
    /// `(op)` alone: the operator as a function of two.
    OpFn(String),
    List(Vec<Node>),
    Tuple(Vec<Node>),
    Neg(Box<Node>),
    /// `\x y -> body`: a function written in place.
    Lambda(Vec<String>, Box<Node>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub expr: Expr,
    /// Byte offsets in the source (start, end).
    pub at: usize,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SyntaxError {
    pub at: usize,
    pub message: String,
}

impl From<LexError> for SyntaxError {
    fn from(e: LexError) -> SyntaxError {
        SyntaxError {
            at: e.at,
            message: e.message,
        }
    }
}

/// Precedence and associativity, as Haskell and Tidal declare them.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Assoc {
    Left,
    Right,
}

fn fixity(op: &str) -> Option<(u8, Assoc)> {
    Some(match op {
        "$" => (0, Assoc::Right),
        "." => (9, Assoc::Right),
        "*" | "/" => (7, Assoc::Left),
        "+" | "-" => (6, Assoc::Left),
        "<>" => (6, Assoc::Right),
        "<~" | "~>" => (5, Assoc::Left),
        "#" => (1, Assoc::Left),
        // The structure operators: |+| |+ +| and friends.
        o if is_structure_op(o) => (1, Assoc::Left),
        _ => return None,
    })
}

/// `|op|`, `|op`, `op|` with op one of + - * / % < > (and `#`).
pub fn is_structure_op(o: &str) -> bool {
    let core = o.trim_start_matches('|').trim_end_matches('|');
    (o.starts_with('|') || o.ends_with('|'))
        && o.len() > core.len()
        && matches!(core, "+" | "-" | "*" | "/" | "%" | "<" | ">" | "#")
}

struct P<'a> {
    toks: &'a [Token],
    i: usize,
    src_len: usize,
    depth: usize,
}

const MAX_DEPTH: usize = 128;

impl P<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.i).map(|t| &t.tok)
    }

    fn at(&self) -> usize {
        self.toks.get(self.i).map_or(self.src_len, |t| t.at)
    }

    fn last_end(&self) -> usize {
        self.i
            .checked_sub(1)
            .and_then(|i| self.toks.get(i))
            .map_or(0, |t| t.end)
    }

    fn err<T>(&self, message: impl Into<String>) -> Result<T, SyntaxError> {
        Err(SyntaxError {
            at: self.at(),
            message: message.into(),
        })
    }

    fn starts_atom(&self) -> bool {
        matches!(
            self.peek(),
            Some(Tok::Ident(_) | Tok::Num(_) | Tok::Str(_) | Tok::Open | Tok::LBracket)
        )
    }

    /// An expression whose operators all bind tighter than `min`.
    fn expr(&mut self, min: u8) -> Result<Node, SyntaxError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return self.err("nested too deeply");
        }
        if self.peek() == Some(&Tok::Op("\\".into())) {
            let l = self.lambda();
            self.depth -= 1;
            return l;
        }
        let mut left = if self.peek() == Some(&Tok::Op("-".into())) {
            // Unary minus (`-0.25`), as tight as `+`.
            let at = self.at();
            self.i += 1;
            let e = self.expr(7)?;
            let end = e.end;
            Node {
                expr: Expr::Neg(Box::new(e)),
                at,
                end,
            }
        } else {
            self.app()?
        };
        loop {
            let (op, at) = match self.peek() {
                Some(Tok::Op(o)) => (o.clone(), self.at()),
                Some(Tok::Tick(name)) => (format!("`{name}`"), self.at()),
                _ => break,
            };
            let (prec, assoc) = match fixity(&op) {
                Some(f) => f,
                None if op.starts_with('`') => (9, Assoc::Left),
                None => {
                    return Err(SyntaxError {
                        at,
                        message: format!("'{op}' isn't an operator Patch's Tidal knows"),
                    });
                }
            };
            if prec < min {
                break;
            }
            // `(a op)`: a section, closed by the parenthesis.
            if self.toks.get(self.i + 1).map(|t| &t.tok) == Some(&Tok::Close) {
                break;
            }
            self.i += 1;
            let next = if assoc == Assoc::Left { prec + 1 } else { prec };
            let right = self.expr(next)?;
            let (start, end) = (left.at, right.end);
            left = Node {
                expr: Expr::Bin(op, Box::new(left), Box::new(right)),
                at: start,
                end,
            };
        }
        self.depth -= 1;
        Ok(left)
    }

    /// `\x y -> body`: the body reaches as far right as it can, as in
    /// Haskell.
    fn lambda(&mut self) -> Result<Node, SyntaxError> {
        let at = self.at();
        self.i += 1;
        let mut params = vec![];
        loop {
            match self.peek().cloned() {
                Some(Tok::Ident(n)) => {
                    params.push(n);
                    self.i += 1;
                }
                Some(Tok::Op(o)) if o == "->" => {
                    self.i += 1;
                    break;
                }
                _ => return self.err("a function written in place is \\x -> …: names, then ->"),
            }
        }
        if params.is_empty() {
            return self.err("\\ needs at least one name before ->");
        }
        let body = self.expr(0)?;
        let end = body.end;
        Ok(Node {
            expr: Expr::Lambda(params, Box::new(body)),
            at,
            end,
        })
    }

    /// Function application: atoms side by side.
    fn app(&mut self) -> Result<Node, SyntaxError> {
        if !self.starts_atom() {
            return match self.peek() {
                None => self.err("this statement stops too soon"),
                Some(Tok::Op(o)) => self.err(format!("'{o}' needs something before it")),
                Some(Tok::Close) => self.err("a ')' without a '('"),
                Some(Tok::RBracket) => self.err("a ']' without a '['"),
                Some(Tok::Comma) => self.err("a ',' outside a list"),
                Some(_) => self.err("expected an expression"),
            };
        }
        let mut f = self.atom()?;
        while self.starts_atom() {
            let a = self.atom()?;
            let (at, end) = (f.at, a.end);
            f = Node {
                expr: Expr::App(Box::new(f), Box::new(a)),
                at,
                end,
            };
        }
        Ok(f)
    }

    fn atom(&mut self) -> Result<Node, SyntaxError> {
        let at = self.at();
        let tok = self.peek().cloned();
        let expr = match tok {
            Some(Tok::Ident(n)) => {
                self.i += 1;
                Expr::Var(n)
            }
            Some(Tok::Num(x)) => {
                self.i += 1;
                Expr::Num(x)
            }
            Some(Tok::Str(s)) => {
                self.i += 1;
                Expr::Str(s)
            }
            Some(Tok::LBracket) => {
                self.i += 1;
                let mut items = vec![];
                if self.peek() != Some(&Tok::RBracket) {
                    loop {
                        items.push(self.expr(0)?);
                        match self.peek() {
                            Some(Tok::Comma) => self.i += 1,
                            Some(Tok::RBracket) => break,
                            _ => return self.err("expected ',' or ']' in this list"),
                        }
                    }
                }
                self.i += 1;
                Expr::List(items)
            }
            Some(Tok::Open) => {
                self.i += 1;
                return self.paren(at);
            }
            _ => return self.err("expected an expression"),
        };
        Ok(Node {
            expr,
            at,
            end: self.last_end(),
        })
    }

    /// After `(`: a section, an operator, a tuple or a grouped expression.
    fn paren(&mut self, at: usize) -> Result<Node, SyntaxError> {
        let close = |p: &mut Self, expr: Expr| -> Result<Node, SyntaxError> {
            if p.peek() != Some(&Tok::Close) {
                return p.err("expected ')'");
            }
            p.i += 1;
            Ok(Node {
                expr,
                at,
                end: p.last_end(),
            })
        };
        // `(op)` or `(op b)`; `(- 1)` / `(-1)` is a negative number.
        let op = match self.peek() {
            Some(Tok::Op(o)) if o == "\\" => None,
            Some(Tok::Op(o)) => Some(o.clone()),
            Some(Tok::Tick(n)) => Some(format!("`{n}`")),
            _ => None,
        };
        if let Some(op) = op {
            if op == "-" && self.toks.get(self.i + 1).map(|t| &t.tok) != Some(&Tok::Close) {
                let e = self.expr(0)?;
                return close(self, e.expr);
            }
            if fixity(&op).is_none() && !op.starts_with('`') {
                return self.err(format!("'{op}' isn't an operator Patch's Tidal knows"));
            }
            self.i += 1;
            if self.peek() == Some(&Tok::Close) {
                return close(self, Expr::OpFn(op));
            }
            let b = self.expr(0)?;
            return close(self, Expr::SectionR(op, Box::new(b)));
        }
        let first = self.expr(0)?;
        match self.peek() {
            // `(a op)`
            Some(Tok::Op(o)) if self.toks.get(self.i + 1).map(|t| &t.tok) == Some(&Tok::Close) => {
                let o = o.clone();
                self.i += 1;
                close(self, Expr::SectionL(Box::new(first), o))
            }
            Some(Tok::Tick(n))
                if self.toks.get(self.i + 1).map(|t| &t.tok) == Some(&Tok::Close) =>
            {
                let o = format!("`{n}`");
                self.i += 1;
                close(self, Expr::SectionL(Box::new(first), o))
            }
            Some(Tok::Comma) => {
                let mut items = vec![first];
                while self.peek() == Some(&Tok::Comma) {
                    self.i += 1;
                    items.push(self.expr(0)?);
                }
                close(self, Expr::Tuple(items))
            }
            _ => {
                let mut n = close(self, first.expr)?;
                n.at = at;
                Ok(n)
            }
        }
    }
}

/// One expression from `src[from..to]` (offsets stay the source's).
pub fn expression(src: &str, from: usize, to: usize) -> Result<Node, SyntaxError> {
    let toks: Vec<Token> = lex(&src[from..to])?
        .into_iter()
        .map(|mut t| {
            t.at += from;
            t.end += from;
            t
        })
        .collect();
    if toks.is_empty() {
        return Err(SyntaxError {
            at: from,
            message: "nothing here".into(),
        });
    }
    let mut p = P {
        toks: &toks,
        i: 0,
        src_len: to,
        depth: 0,
    };
    let e = p.expr(0)?;
    if p.i < toks.len() {
        return p.err(match &toks[p.i].tok {
            Tok::Close => "a ')' without a '('".to_string(),
            Tok::RBracket => "a ']' without a '['".to_string(),
            Tok::Comma => "a ',' outside a list or tuple".to_string(),
            _ => "unexpected text after the expression".to_string(),
        });
    }
    Ok(e)
}

/// A statement's text: where it starts and ends in the source.
#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    pub at: usize,
    pub end: usize,
}

/// The statements of `src` (comments blanked): each starts at a line's
/// first column, and indented lines belong to the statement above.
pub fn chunks(clean: &str) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = vec![];
    let mut offset = 0;
    for line in clean.split_inclusive('\n') {
        let body = line.trim_end();
        let blank = body.trim().is_empty();
        let starts = !blank && !line.starts_with([' ', '\t']);
        if starts {
            out.push(Chunk {
                at: offset,
                end: offset + body.len(),
            });
        } else if !blank {
            match out.last_mut() {
                Some(c) => c.end = offset + body.len(),
                // An indented first line starts the first statement.
                None => out.push(Chunk {
                    at: offset + (line.len() - line.trim_start().len()),
                    end: offset + body.len(),
                }),
            }
        }
        offset += line.len();
    }
    out.into_iter().flat_map(|c| do_block(clean, c)).collect()
}

/// `do` and the statements under it (each at the first one's column, as
/// Haskell's layout reads them); any other statement as it is.
fn do_block(clean: &str, c: Chunk) -> Vec<Chunk> {
    let text = &clean[c.at..c.end];
    let Some(rest) = text.strip_prefix("do") else { return vec![c] };
    if !rest.starts_with(char::is_whitespace) {
        return vec![c];
    }
    let first = c.at + 2 + (rest.len() - rest.trim_start().len());
    if first >= c.end {
        return vec![];
    }
    let line_start = clean[..first].rfind('\n').map_or(0, |i| i + 1);
    let col = clean[line_start..first].chars().count();
    let mut out: Vec<Chunk> = vec![Chunk { at: first, end: first }];
    let mut offset = line_start;
    for line in clean[line_start..c.end].split_inclusive('\n') {
        let body = line.trim_end();
        let indent = line.len() - line.trim_start().len();
        let here = offset.max(first);
        if !body.trim().is_empty() && here < offset + body.len() {
            if offset > line_start && indent <= col {
                out.push(Chunk { at: offset + indent, end: offset + body.len() });
            } else if let Some(last) = out.last_mut() {
                last.end = offset + body.len();
            }
        }
        offset += line.len();
    }
    out
}

/// Comments removed (offsets kept) and statements found.
pub fn statements(src: &str) -> (String, Vec<Chunk>) {
    let clean = blank_comments(src);
    let chunks = chunks(&clean);
    (clean, chunks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(n: &Node) -> String {
        match &n.expr {
            Expr::Num(x) => format!("{x}"),
            Expr::Str(s) => format!("{s:?}"),
            Expr::Var(v) => v.clone(),
            Expr::App(f, a) => format!("({} {})", show(f), show(a)),
            Expr::Bin(o, a, b) => format!("[{} {o} {}]", show(a), show(b)),
            Expr::SectionL(a, o) => format!("({} {o})", show(a)),
            Expr::SectionR(o, b) => format!("({o} {})", show(b)),
            Expr::OpFn(o) => format!("({o})"),
            Expr::List(v) => format!("[{}]", v.iter().map(show).collect::<Vec<_>>().join(", ")),
            Expr::Tuple(v) => format!("<{}>", v.iter().map(show).collect::<Vec<_>>().join(", ")),
            Expr::Neg(e) => format!("-{}", show(e)),
            Expr::Lambda(ps, b) => format!("(\\{} -> {})", ps.join(" "), show(b)),
        }
    }

    fn parse(s: &str) -> String {
        show(&expression(s, 0, s.len()).unwrap())
    }

    #[test]
    fn fixities_follow_haskell() {
        assert_eq!(
            parse(r#"d1 $ every 4 (fast 2) $ s "bd sn" # gain 1.2 # speed "1 2""#),
            r#"[d1 $ [((every 4) (fast 2)) $ [[(s "bd sn") # (gain 1.2)] # (speed "1 2")]]]"#
        );
        assert_eq!(parse("rev . fast 2"), "[rev . (fast 2)]");
        assert_eq!(parse("0.25 <~ p"), "[0.25 <~ p]");
        assert_eq!(parse(r#"n ("0 2" + "<12 7>")"#), r#"(n ["0 2" + "<12 7>"])"#);
        assert_eq!(parse("1/8 + 1/4"), "[[1 / 8] + [1 / 4]]");
    }

    #[test]
    fn sections_tuples_and_negatives() {
        assert_eq!(parse("(# speed 2)"), "(# (speed 2))");
        assert_eq!(parse("(0.25 ~>)"), "(0.25 ~>)");
        assert_eq!(parse("(1/8)"), "[1 / 8]");
        assert_eq!(parse("(-0.25)"), "-0.25");
        assert_eq!(parse("within (0, 0.5) (fast 2)"), "((within <0, 0.5>) (fast 2))");
        assert_eq!(parse("[s \"a\", s \"b\"]"), r#"[(s "a"), (s "b")]"#);
        assert_eq!(parse("(+)"), "(+)");
    }

    #[test]
    fn errors_point_at_the_problem() {
        let src = "s \"bd\" # # gain 1";
        let e = expression(src, 0, src.len()).unwrap_err();
        assert_eq!(e.at, 9);
        let src = "fast 2 (s \"bd\"";
        assert!(expression(src, 0, src.len()).is_err());
    }

    #[test]
    fn statements_start_at_the_first_column() {
        let src = "d1 $ s \"bd\"\n  # gain 1\n\n-- a comment\nd2 $ s \"hh*2\"\nhush\n";
        let (clean, c) = statements(src);
        assert_eq!(c.len(), 3);
        assert_eq!(&clean[c[0].at..c[0].end], "d1 $ s \"bd\"\n  # gain 1");
        assert_eq!(&clean[c[1].at..c[1].end], "d2 $ s \"hh*2\"");
        assert_eq!(&clean[c[2].at..c[2].end], "hush");
    }
}
