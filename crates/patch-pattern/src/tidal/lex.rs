//! Tokens of Tidal code: Haskell's lexical shapes, as much of them as live
//! code uses. Operators are runs of symbol characters (`#`, `|+|`, `<~`,
//! `$`, `.`), so `|+|` is one token, as in Haskell.

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Num(f64),
    Str(String),
    Op(String),
    Open,
    Close,
    LBracket,
    RBracket,
    Comma,
    /// `` `div` ``: a function used as an operator.
    Tick(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub tok: Tok,
    /// Byte offsets of the token in the source.
    pub at: usize,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LexError {
    pub at: usize,
    pub message: String,
}

fn symbol(c: char) -> bool {
    matches!(
        c,
        '!' | '#' | '$' | '%' | '&' | '*' | '+' | '.' | '/' | '<' | '=' | '>' | '?' | '@'
            | '\\' | '^' | '|' | '-' | '~' | ':'
    )
}

/// The source without comments (`-- …` to the end of a line, `{- … -}`),
/// each comment character replaced by a space so offsets still point at
/// the original text.
pub fn blank_comments(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if c == b'"' || c == b'\n' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            in_str = true;
            i += 1;
            continue;
        }
        if c == b'{' && b.get(i + 1) == Some(&b'-') {
            let mut depth = 0;
            while i < b.len() {
                if b[i] == b'{' && b.get(i + 1) == Some(&b'-') {
                    depth += 1;
                    out[i] = b' ';
                    out[i + 1] = b' ';
                    i += 2;
                } else if b[i] == b'-' && b.get(i + 1) == Some(&b'}') {
                    depth -= 1;
                    out[i] = b' ';
                    out[i + 1] = b' ';
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if b[i] != b'\n' {
                        out[i] = b' ';
                    }
                    i += 1;
                }
            }
            continue;
        }
        // `--` starts a comment unless it is part of a longer operator
        // (`-->`), as in Haskell.
        if c == b'-' && b.get(i + 1) == Some(&b'-') {
            let mut j = i;
            while j < b.len() && b[j] == b'-' {
                j += 1;
            }
            let longer_op = j < b.len() && symbol(b[j] as char);
            let after_op = i > 0 && symbol(b[i - 1] as char);
            if !longer_op && !after_op {
                while i < b.len() && b[i] != b'\n' {
                    out[i] = b' ';
                    i += 1;
                }
                continue;
            }
        }
        i += 1;
    }
    // Only ASCII bytes were replaced by spaces, inside comments: still UTF-8
    // (a multi-byte character in a comment became several spaces).
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

pub fn lex(src: &str) -> Result<Vec<Token>, LexError> {
    let mut out = Vec::new();
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let end_of = |k: usize| chars.get(k).map_or(src.len(), |c| c.0);
    let mut k = 0;
    while k < chars.len() {
        let (at, c) = chars[k];
        if c.is_whitespace() {
            k += 1;
            continue;
        }
        let tok = match c {
            '(' => {
                k += 1;
                Tok::Open
            }
            ')' => {
                k += 1;
                Tok::Close
            }
            '[' => {
                k += 1;
                Tok::LBracket
            }
            ']' => {
                k += 1;
                Tok::RBracket
            }
            ',' => {
                k += 1;
                Tok::Comma
            }
            '"' => {
                k += 1;
                let mut s = String::new();
                loop {
                    match chars.get(k) {
                        None | Some((_, '\n')) => {
                            return Err(LexError {
                                at,
                                message: "this string is never closed (a '\"' is missing)"
                                    .into(),
                            });
                        }
                        Some((_, '"')) => {
                            k += 1;
                            break;
                        }
                        Some((_, '\\')) if chars.get(k + 1).is_some_and(|c| c.1 == '"') => {
                            s.push('"');
                            k += 2;
                        }
                        Some((_, ch)) => {
                            s.push(*ch);
                            k += 1;
                        }
                    }
                }
                Tok::Str(s)
            }
            '`' => {
                k += 1;
                let mut s = String::new();
                while let Some((_, ch)) = chars.get(k) {
                    if *ch == '`' {
                        break;
                    }
                    s.push(*ch);
                    k += 1;
                }
                if chars.get(k).is_none() {
                    return Err(LexError {
                        at,
                        message: "a '`' is never closed".into(),
                    });
                }
                k += 1;
                Tok::Tick(s.trim().to_string())
            }
            c if c.is_ascii_digit() => {
                while chars.get(k).is_some_and(|c| c.1.is_ascii_digit()) {
                    k += 1;
                }
                // A fraction, but not `0..7` (a range).
                if chars.get(k).is_some_and(|c| c.1 == '.')
                    && chars.get(k + 1).is_some_and(|c| c.1.is_ascii_digit())
                {
                    k += 1;
                    while chars.get(k).is_some_and(|c| c.1.is_ascii_digit()) {
                        k += 1;
                    }
                }
                if chars.get(k).is_some_and(|c| c.1 == 'e' || c.1 == 'E')
                    && chars.get(k + 1).is_some_and(|c| {
                        c.1.is_ascii_digit()
                            || (c.1 == '-' && chars.get(k + 2).is_some_and(|d| d.1.is_ascii_digit()))
                    })
                {
                    k += 2;
                    while chars.get(k).is_some_and(|c| c.1.is_ascii_digit()) {
                        k += 1;
                    }
                }
                let text = &src[at..end_of(k)];
                Tok::Num(text.parse().map_err(|_| LexError {
                    at,
                    message: format!("'{text}' is not a number"),
                })?)
            }
            c if c.is_alphabetic() || c == '_' => {
                while chars
                    .get(k)
                    .is_some_and(|c| c.1.is_alphanumeric() || c.1 == '_' || c.1 == '\'')
                {
                    k += 1;
                }
                // A qualified name (`Sound.Tidal.Context.fast`): its last part.
                let mut text = src[at..end_of(k)].to_string();
                while chars.get(k).is_some_and(|c| c.1 == '.')
                    && text.starts_with(|c: char| c.is_uppercase())
                    && chars.get(k + 1).is_some_and(|c| c.1.is_alphabetic())
                {
                    k += 1;
                    let from = k;
                    while chars
                        .get(k)
                        .is_some_and(|c| c.1.is_alphanumeric() || c.1 == '_' || c.1 == '\'')
                    {
                        k += 1;
                    }
                    text = src[end_of(from)..end_of(k)].to_string();
                }
                Tok::Ident(text)
            }
            c if symbol(c) => {
                while chars.get(k).is_some_and(|c| symbol(c.1)) {
                    k += 1;
                }
                Tok::Op(src[at..end_of(k)].to_string())
            }
            other => {
                return Err(LexError {
                    at,
                    message: format!("unexpected '{other}'"),
                });
            }
        };
        out.push(Token {
            tok,
            at,
            end: end_of(k),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<Tok> {
        lex(s).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn operators_are_maximal_runs() {
        assert_eq!(
            toks(r#"d1 $ s "bd*2" |+| n 1 # gain 1.2"#),
            vec![
                Tok::Ident("d1".into()),
                Tok::Op("$".into()),
                Tok::Ident("s".into()),
                Tok::Str("bd*2".into()),
                Tok::Op("|+|".into()),
                Tok::Ident("n".into()),
                Tok::Num(1.0),
                Tok::Op("#".into()),
                Tok::Ident("gain".into()),
                Tok::Num(1.2),
            ]
        );
        assert_eq!(
            toks("(0.25 ~>) every' chunk'"),
            vec![
                Tok::Open,
                Tok::Num(0.25),
                Tok::Op("~>".into()),
                Tok::Close,
                Tok::Ident("every'".into()),
                Tok::Ident("chunk'".into()),
            ]
        );
    }

    #[test]
    fn comments_keep_offsets() {
        let src = "d1 $ s \"bd -- not a comment\" -- a comment\n{- block\n -} hush";
        let clean = blank_comments(src);
        assert_eq!(clean.len(), src.len());
        assert!(clean.contains("not a comment"));
        assert!(!clean.contains("a comment\n"));
        assert!(!clean.contains("block"));
        assert_eq!(clean.find("hush"), src.find("hush"));
    }

    #[test]
    fn an_open_string_is_an_error_where_it_starts() {
        let e = lex("d1 $ s \"bd sn").unwrap_err();
        assert_eq!(e.at, 7);
    }
}
