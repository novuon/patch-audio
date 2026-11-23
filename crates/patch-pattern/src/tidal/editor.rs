//! What the Tidal Unit's editor knows about the code being written: the
//! colour of every character, the word at the cursor and what it does, how
//! to complete it, and the editing a code editor does for you (indenting a
//! new line, closing brackets and quotes, commenting lines out). Pure
//! functions on text and byte offsets, so the window stays a thin view and
//! all of it is tested here.
//!
//! [`SourceMap`] finds a channel's code, as the engine keeps it, in the
//! editor's text, so the words an event came from (`Event::locs`) light up
//! where they are written.

use super::eval::{CONTROLS, WORDS};
use super::help;
use super::lex::blank_comments;

/// What a character is, for its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Class {
    /// Names the code defines (`let` names and arguments), brackets.
    Plain = 0,
    Comment = 1,
    /// Words inside a string: the mini-notation's sounds and numbers.
    Str = 2,
    Number = 3,
    /// A word of Tidal's vocabulary (`fast`, `every`, `jux`).
    Word = 4,
    /// A control (`s`, `n`, `gain`, `room`).
    Control = 5,
    /// What starts a statement (`d1`, `hush`, `setcps`, `let`).
    Head = 6,
    /// Operators, and the mini-notation's own symbols (`*`, `<`, `~`).
    Op = 7,
    /// A name Patch's Tidal doesn't know: it would fail when run.
    Unknown = 8,
}

pub const CLASSES: usize = 9;

/// Words that start a statement.
const HEADS: &[&str] = &[
    "hush", "setcps", "setbpm", "let", "mute", "unmute", "solo", "unsolo", "unmuteAll", "unsoloAll",
    "p", "once", "do", "xfade", "xfadeIn", "jumpIn", "jumpIn'", "clutch", "clutchIn",
    "anticipate", "interpolate",
];

fn is_head(w: &str) -> bool {
    HEADS.contains(&w)
        || w.strip_prefix('d').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn is_word(w: &str) -> bool {
    WORDS.iter().any(|x| x.0 == w)
}

fn is_control(w: &str) -> bool {
    CONTROLS.contains(&w)
}

fn ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '\''
}

fn symbol(c: char) -> bool {
    matches!(
        c,
        '!' | '#' | '$' | '%' | '&' | '*' | '+' | '.' | '/' | '<' | '=' | '>' | '?' | '@' | '\\' | '^' | '|'
            | '-' | '~' | ':'
    )
}

/// A mini-notation word's characters (as the mini-notation parser reads them).
fn mini_word(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '.' | '#' | ':' | '-' | '\'')
}

/// The names the code itself defines: `let name args = …` and `\x ->`.
pub fn defined_names(code: &str) -> Vec<String> {
    let clean = blank_comments(code);
    let mut out: Vec<String> = Vec::new();
    for line in clean.lines() {
        let t = line.trim_start();
        let t = t.strip_prefix("do ").map(str::trim_start).unwrap_or(t);
        if let Some(rest) = t.strip_prefix("let ")
            && let Some(eq) = rest.find('=')
        {
            for w in rest[..eq].split_whitespace() {
                if w.starts_with(ident_start) && !out.iter().any(|o| o == w) {
                    out.push(w.to_string());
                }
            }
        }
        let mut s = line;
        while let Some(i) = s.find('\\') {
            let after = &s[i + 1..];
            let end = after.find("->").unwrap_or(after.len());
            for w in after[..end].split_whitespace() {
                if w.starts_with(ident_start) && !out.iter().any(|o| o == w) {
                    out.push(w.to_string());
                }
            }
            s = &after[end..];
        }
    }
    out
}

/// The class of every character of `code`, in order.
pub fn classify(code: &str) -> Vec<Class> {
    let names = defined_names(code);
    let chars: Vec<char> = code.chars().collect();
    let mut out = vec![Class::Plain; chars.len()];
    let mut k = 0;
    while k < chars.len() {
        let c = chars[k];
        // Comments: `--` to the end of the line (not inside a longer
        // operator), `{- … -}`.
        if c == '-' && chars.get(k + 1) == Some(&'-') {
            let mut j = k;
            while j < chars.len() && chars[j] == '-' {
                j += 1;
            }
            let longer = j < chars.len() && symbol(chars[j]);
            let after = k > 0 && symbol(chars[k - 1]);
            if !longer && !after {
                while k < chars.len() && chars[k] != '\n' {
                    out[k] = Class::Comment;
                    k += 1;
                }
                continue;
            }
        }
        if c == '{' && chars.get(k + 1) == Some(&'-') {
            let mut depth = 0;
            while k < chars.len() {
                if chars[k] == '{' && chars.get(k + 1) == Some(&'-') {
                    depth += 1;
                    out[k] = Class::Comment;
                    out[k + 1] = Class::Comment;
                    k += 2;
                } else if chars[k] == '-' && chars.get(k + 1) == Some(&'}') {
                    depth -= 1;
                    out[k] = Class::Comment;
                    out[k + 1] = Class::Comment;
                    k += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    out[k] = Class::Comment;
                    k += 1;
                }
            }
            continue;
        }
        if c == '"' {
            out[k] = Class::Op;
            let line_start = chars[..k].iter().rposition(|c| *c == '\n').map_or(0, |i| i + 1);
            let before: String = chars[line_start..k].iter().collect();
            let sounds = sound_string(&before);
            k += 1;
            let mut depth = 0;
            while k < chars.len() && chars[k] != '"' && chars[k] != '\n' {
                if !mini_word(chars[k]) {
                    depth += match chars[k] {
                        '(' => 1,
                        ')' if depth > 0 => -1,
                        _ => 0,
                    };
                    out[k] = Class::Op;
                    k += 1;
                    continue;
                }
                // A word: in `s "…"`, a sound Patch has (or not).
                let start = k;
                while k < chars.len() && mini_word(chars[k]) {
                    k += 1;
                }
                let w: String = chars[start..k].iter().collect();
                let name = w.split(':').next().unwrap_or("");
                let unknown = sounds
                    && depth == 0
                    && name.starts_with(|c: char| c.is_alphabetic())
                    && !known_sound(name);
                for o in &mut out[start..k] {
                    *o = if unknown { Class::Unknown } else { Class::Str };
                }
            }
            if k < chars.len() && chars[k] == '"' {
                out[k] = Class::Op;
                k += 1;
            }
            continue;
        }
        if c.is_ascii_digit() {
            while k < chars.len()
                && (chars[k].is_ascii_digit()
                    || (chars[k] == '.' && chars.get(k + 1).is_some_and(char::is_ascii_digit))
                    || (matches!(chars[k], 'e' | 'E') && chars.get(k + 1).is_some_and(char::is_ascii_digit)))
            {
                out[k] = Class::Number;
                k += 1;
            }
            continue;
        }
        if ident_start(c) {
            let start = k;
            while k < chars.len() && ident_char(chars[k]) {
                k += 1;
            }
            let w: String = chars[start..k].iter().collect();
            let class = if is_head(&w) {
                Class::Head
            } else if names.contains(&w) {
                Class::Plain
            } else if is_control(&w) {
                Class::Control
            } else if is_word(&w) {
                Class::Word
            } else {
                Class::Unknown
            };
            for o in &mut out[start..k] {
                *o = class;
            }
            continue;
        }
        if c == '`' {
            let start = k;
            k += 1;
            while k < chars.len() && chars[k] != '`' && chars[k] != '\n' {
                k += 1;
            }
            k = (k + 1).min(chars.len());
            for o in &mut out[start..k] {
                *o = Class::Word;
            }
            continue;
        }
        if symbol(c) {
            out[k] = Class::Op;
        }
        k += 1;
    }
    out
}

/// The code as one text per class: each keeps its own characters and puts
/// a space in place of every other (line breaks stay), so in a monospaced
/// face the texts lie exactly over each other and over the code.
pub fn layers(code: &str) -> [String; CLASSES] {
    let classes = classify(code);
    let mut out: [String; CLASSES] = Default::default();
    for o in out.iter_mut() {
        o.reserve(code.len());
    }
    for (c, class) in code.chars().zip(classes) {
        for (i, o) in out.iter_mut().enumerate() {
            if c == '\n' {
                o.push('\n');
            } else if i == class as usize && !c.is_whitespace() {
                o.push(c);
            } else {
                o.push(' ');
            }
        }
    }
    out
}

/// The word around (or just before) the cursor: its byte range.
pub fn word_at(code: &str, cursor: usize) -> Option<(usize, usize)> {
    let cursor = code.floor_char_boundary(cursor.min(code.len()));
    let before = code[..cursor].char_indices().rev().take_while(|(_, c)| ident_char(*c)).last();
    let start = before.map_or(cursor, |(i, _)| i);
    let end = cursor + code[cursor..].chars().take_while(|c| ident_char(*c)).map(char::len_utf8).sum::<usize>();
    if start == end || !code[start..].starts_with(ident_start) {
        return None;
    }
    // Inside a string or comment: not a word of the language.
    let line_start = code[..start].rfind('\n').map_or(0, |i| i + 1);
    if code[line_start..start].matches('"').count() % 2 == 1
        || blank_comments(&code[line_start..end])[start - line_start..].trim().is_empty()
    {
        return None;
    }
    Some((start, end))
}

/// Whether a string opened after `before` (the line up to its quote) holds
/// sounds: `s "…"`, `# sound "…"`, `(s "…")`.
fn sound_string(before: &str) -> bool {
    let b = before.trim_end().trim_end_matches('(').trim_end();
    let start = b.len() - b.chars().rev().take_while(|c| ident_char(*c)).map(char::len_utf8).sum::<usize>();
    matches!(&b[start..], "s" | "sound")
}

fn sound_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.')
}

/// Whether Patch has a sound of that name: a sample folder, or built in.
pub fn known_sound(name: &str) -> bool {
    crate::instrument::sound(name, 0.0).is_some() || crate::samples::index().get(name).is_some()
}

/// The sound name around (or just before) the cursor inside `s "…"`: its
/// byte range (the name only, not a `:n` after it).
pub fn sound_at(code: &str, cursor: usize) -> Option<(usize, usize)> {
    let cursor = code.floor_char_boundary(cursor.min(code.len()));
    let line_start = code[..cursor].rfind('\n').map_or(0, |i| i + 1);
    let line = &code[line_start..cursor];
    // Inside a string (not a comment) on this line.
    let mut in_str = false;
    for (i, c) in line.char_indices() {
        if c == '"' {
            in_str = !in_str;
        } else if !in_str && line[i..].starts_with("--") {
            return None;
        }
    }
    if !in_str {
        return None;
    }
    let quote = line_start + line.rfind('"')?;
    if !sound_string(&code[line_start..quote]) {
        return None;
    }
    let start = code[..cursor]
        .char_indices()
        .rev()
        .take_while(|(i, c)| *i > quote && sound_char(*c))
        .last()
        .map_or(cursor, |(i, _)| i);
    let end = cursor + code[cursor..].chars().take_while(|c| sound_char(*c)).map(char::len_utf8).sum::<usize>();
    if start == end || !code[start..].starts_with(char::is_alphabetic) {
        return None;
    }
    Some((start, end))
}

/// Sounds starting with what is typed in `s "…"`: sample folders first,
/// then Patch's own.
fn sound_completions(code: &str, cursor: usize) -> Vec<String> {
    let Some((start, end)) = sound_at(code, cursor) else { return vec![] };
    if end != cursor.min(code.len()) {
        return vec![];
    }
    let prefix = &code[start..end];
    let ix = crate::samples::index();
    let (drums, synths) = crate::instrument::names();
    let mut found: Vec<(u8, usize, String)> = ix
        .names()
        .map(|(n, _)| (0, n.to_string()))
        .chain(drums.into_iter().chain(synths).map(|n| (1, n.to_string())))
        .filter(|(_, n)| n.starts_with(prefix) && n.len() > prefix.len())
        .map(|(r, n)| (r, n.len(), n))
        .collect();
    found.sort();
    found.dedup_by(|a, b| a.2 == b.2);
    found.into_iter().map(|f| f.2).take(8).collect()
}

/// What a sound is: a sample folder (and its files), or Patch's own.
pub fn sound_hint(name: &str) -> Option<String> {
    let ix = crate::samples::index();
    if let Some(f) = ix.get(name) {
        let n = f.files.len();
        let which = if n == 1 { "1 file".to_string() } else { format!("{n} files, {name}:0 … {name}:{}", n - 1) };
        return Some(format!("{name}  —  sample folder: {which}"));
    }
    if let Some(&(_, voice, _)) = crate::instrument::DRUMS.iter().find(|d| d.0 == name) {
        const VOICE: [&str; 8] = ["kick", "snare", "closed hat", "open hat", "clap", "tom", "rim", "shaker"];
        return Some(format!("{name}  —  Patch's built-in {}", VOICE[voice as usize % 8]));
    }
    if crate::instrument::KITS.contains(&name) {
        return Some(format!("{name}  —  Patch's drum kit: {name}:0 kick … {name}:7 shaker"));
    }
    if crate::instrument::sound(name, 0.0).is_some() {
        return Some(format!("{name}  —  Patch's synth: n or note sets the pitch"));
    }
    let own = crate::samples::own_root();
    Some(format!("Patch has no sound '{name}': put audio files in {}/{name}/", own.display()))
}

/// Every word Patch's Tidal knows, and the code's own names, with a rank:
/// the code's names first, then functions, controls, statement words.
fn vocabulary(code: &str) -> Vec<(u8, String)> {
    let mut v: Vec<(u8, String)> = defined_names(code).into_iter().map(|n| (0, n)).collect();
    v.extend(WORDS.iter().filter(|w| !w.0.starts_with('_')).map(|w| (1, w.0.to_string())));
    v.extend(CONTROLS.iter().map(|c| (2, c.to_string())));
    v.extend(HEADS.iter().map(|h| (3, h.to_string())));
    let mut seen = std::collections::HashSet::new();
    v.retain(|(_, w)| seen.insert(w.clone()));
    v
}

/// Completions of the word being typed (`fa` → `fast`, `fastGap`, …):
/// exact-case prefix matches first, shortest first.
pub fn completions(code: &str, cursor: usize) -> Vec<String> {
    let Some((start, end)) = word_at(code, cursor) else { return sound_completions(code, cursor) };
    if end != cursor.min(code.len()) {
        return vec![];
    }
    let prefix = &code[start..end];
    let lower = prefix.to_ascii_lowercase();
    let mut found: Vec<(u8, u8, usize, String)> = vocabulary(code)
        .into_iter()
        .filter(|(_, w)| w != prefix && w.len() > prefix.len())
        .filter_map(|(rank, w)| {
            if w.starts_with(prefix) {
                Some((0, rank, w.len(), w))
            } else if w.to_ascii_lowercase().starts_with(&lower) {
                Some((1, rank, w.len(), w))
            } else {
                None
            }
        })
        .collect();
    found.sort();
    found.into_iter().map(|f| f.3).take(8).collect()
}

/// What the word does, from the reference: `usage — what it does`.
pub fn hint(word: &str) -> Option<String> {
    let entry = help::REFERENCE.iter().find(|r| {
        r.1 == word || r.1.split([' ', '/']).any(|w| w == word && !w.is_empty())
    });
    if let Some((_, _, usage, doc, _)) = entry {
        return Some(format!("{usage}  —  {doc}"));
    }
    if let Some((w, arity)) = WORDS.iter().find(|w| w.0 == word) {
        return Some(match arity {
            0 => format!("{w}: a pattern"),
            1 => format!("{w}: takes 1 argument"),
            n => format!("{w}: takes {n} arguments"),
        });
    }
    if is_control(word) {
        return Some(format!("# {word} …: a control"));
    }
    if is_head(word) {
        return Some(format!("{word}: starts a statement"));
    }
    None
}

/// An edit: the new code and where the cursor goes.
pub type Edit = (String, usize);

fn sel(code: &str, anchor: usize, cursor: usize) -> (usize, usize) {
    let fix = |i: usize| code.floor_char_boundary(i.min(code.len()));
    let (a, c) = (fix(anchor), fix(cursor));
    (a.min(c), a.max(c))
}

fn replace(code: &str, a: usize, b: usize, with: &str, cursor_in: usize) -> Edit {
    let mut out = String::with_capacity(code.len() + with.len());
    out.push_str(&code[..a]);
    out.push_str(with);
    out.push_str(&code[b..]);
    (out, a + cursor_in)
}

/// Enter: a new line indented like this one (one step more after a line
/// ending in `$`, `(`, `[` or `#`: the statement goes on).
pub fn newline(code: &str, anchor: usize, cursor: usize) -> Edit {
    let (a, b) = sel(code, anchor, cursor);
    let line_start = code[..a].rfind('\n').map_or(0, |i| i + 1);
    let line = &code[line_start..a];
    let mut indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
    let t = blank_comments(line);
    if t.trim_end().ends_with(['$', '(', '[', '#', '.', ',']) {
        indent.push_str("  ");
    }
    let with = format!("\n{indent}");
    replace(code, a, b, &with, with.len())
}

/// Tab: complete the word being typed when one completion stands out (the
/// only one, or what all of them start with); else indent by two spaces.
pub fn tab(code: &str, anchor: usize, cursor: usize) -> Edit {
    let (a, b) = sel(code, anchor, cursor);
    if a == b
        && let Some((start, end)) = word_at(code, a).or_else(|| sound_at(code, a))
        && end == a
    {
        let found = completions(code, a);
        let prefix = &code[start..end];
        if let Some(first) = found.first() {
            // The longest start every completion shares (case as found).
            let common = found.iter().skip(1).fold(first.clone(), |acc, w| {
                acc.chars().zip(w.chars()).take_while(|(x, y)| x == y).map(|x| x.0).collect()
            });
            let pick = if found.len() == 1 || common.len() <= prefix.len() { first.clone() } else { common };
            return replace(code, start, end, &pick, pick.len());
        }
    }
    if a != b {
        // A selection: indent its lines.
        let first = code[..a].rfind('\n').map_or(0, |i| i + 1);
        let block = &code[first..b];
        let indented: String = block.split('\n').map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n");
        let (s, _) = replace(code, first, b, &indented, 0);
        return (s, first + indented.len());
    }
    replace(code, a, b, "  ", 2)
}

/// Ctrl+/: comment the lines of the selection (or the cursor's line) out,
/// or back in when they all are. The cursor stays on its character.
pub fn toggle_comment(code: &str, anchor: usize, cursor: usize) -> Edit {
    let (a, b) = sel(code, anchor, cursor);
    let cursor = code.floor_char_boundary(cursor.min(code.len()));
    let first = code[..a].rfind('\n').map_or(0, |i| i + 1);
    let last = code[b..].find('\n').map_or(code.len(), |i| b + i);
    let lines: Vec<&str> = code[first..last].split('\n').collect();
    let filled = || lines.iter().filter(|l| !l.trim().is_empty());
    let all = filled().count() > 0 && filled().all(|l| l.trim_start().starts_with("--"));
    let indent = filled().map(|l| l.len() - l.trim_start().len()).min().unwrap_or(0);
    let mut out = String::with_capacity(code.len() + 3 * lines.len());
    out.push_str(&code[..first]);
    let mut new_cursor = cursor;
    let mut at = first;
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        // (where in the line text changes, bytes removed, text added)
        let (pos, removed, added) = if l.trim().is_empty() {
            (0, 0, "")
        } else if all {
            let lead = l.len() - l.trim_start().len();
            let n = if l[lead + 2..].starts_with(' ') { 3 } else { 2 };
            (lead, n, "")
        } else {
            (indent.min(l.len()), 0, "-- ")
        };
        out.push_str(&l[..pos]);
        out.push_str(added);
        out.push_str(&l[pos + removed..]);
        if cursor >= at && cursor <= at + l.len() {
            let col = cursor - at;
            let col = if col >= pos + removed { col - removed + added.len() } else { col.min(pos) };
            new_cursor = out.len() - (l.len() - pos - removed) - added.len() - pos + col;
        } else if cursor > at + l.len() {
            new_cursor = cursor;
        }
        at += l.len() + 1;
    }
    let delta = out.len() as isize - last as isize;
    out.push_str(&code[last..]);
    if cursor > last {
        new_cursor = (cursor as isize + delta) as usize;
    }
    let c = out.floor_char_boundary(new_cursor.min(out.len()));
    (out, c)
}

/// A typed opening bracket or quote, closed for you (only before a space,
/// the end of the line or a closer, so typing into a word stays as typed);
/// a selection is wrapped. A typed closer the cursor sits on is stepped
/// over. `None`: type it as usual.
pub fn pair(code: &str, anchor: usize, cursor: usize, typed: char) -> Option<Edit> {
    let (a, b) = sel(code, anchor, cursor);
    let close = match typed {
        '(' => ')',
        '[' => ']',
        '"' => '"',
        ')' | ']' => {
            return (a == b && code[a..].starts_with(typed)).then(|| (code.to_string(), a + 1));
        }
        _ => return None,
    };
    if typed == '"' && a == b && code[a..].starts_with('"') {
        // Step over the closing quote of a string just typed.
        let line_start = code[..a].rfind('\n').map_or(0, |i| i + 1);
        if code[line_start..a].matches('"').count() % 2 == 1 {
            return Some((code.to_string(), a + 1));
        }
    }
    if a != b {
        let with = format!("{typed}{}{close}", &code[a..b]);
        let n = with.len();
        return Some(replace(code, a, b, &with, n));
    }
    let next = code[a..].chars().next();
    let free = next.is_none_or(|c| c.is_whitespace() || matches!(c, ')' | ']' | ','));
    if !free {
        return None;
    }
    if typed == '"' {
        // Not inside a string already (that quote closes it).
        let line_start = code[..a].rfind('\n').map_or(0, |i| i + 1);
        if code[line_start..a].matches('"').count() % 2 == 1 {
            return None;
        }
        // Not right after a word character (`can"t`).
        if code[..a].chars().next_back().is_some_and(|c| c.is_alphanumeric()) {
            return None;
        }
    }
    Some(replace(code, a, b, &format!("{typed}{close}"), 1))
}

/// Backspace between an empty pair (`(|)`, `"|"`): both go.
pub fn backspace(code: &str, anchor: usize, cursor: usize) -> Option<Edit> {
    let (a, b) = sel(code, anchor, cursor);
    if a != b || a == 0 {
        return None;
    }
    let before = code[..a].chars().next_back()?;
    let after = code[a..].chars().next()?;
    matches!((before, after), ('(', ')') | ('[', ']') | ('"', '"'))
        .then(|| replace(code, a - 1, a + 1, "", 0))
}

/// Where code the engine keeps (a channel's expression, a `let`'s) is
/// written in the editor: for every byte of the kept code, the editor's
/// byte, so an event's words (`Event::locs`) light up where they are.
/// Whitespace and comments may differ (the engine re-indents and drops
/// comments); every other character must match, else it isn't found: code
/// edited since it ran isn't what plays.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceMap {
    to: Vec<u32>,
}

const NONE: u32 = u32::MAX;

impl SourceMap {
    /// The occurrence of `kept` in `code` at or after byte `near` (the
    /// channel's line), else the last before it.
    /// A channel in a transition (`_xfade … $ new`) is found by its new
    /// code: what was typed.
    pub fn find(kept: &str, code: &str, near: usize) -> Option<SourceMap> {
        let skip = super::transition(kept).map_or(0, |t| t.new_at);
        let k: Vec<(usize, char)> =
            kept.char_indices().filter(|(i, c)| *i >= skip && !c.is_whitespace()).collect();
        if k.is_empty() {
            return None;
        }
        let clean = blank_comments(code);
        let e: Vec<(usize, char)> = clean.char_indices().filter(|(_, c)| !c.is_whitespace()).collect();
        let mut before = None;
        let mut after = None;
        for i in 0..(e.len() + 1).saturating_sub(k.len()) {
            if e[i..i + k.len()].iter().zip(&k).all(|(x, y)| x.1 == y.1) {
                if e[i].0 >= near {
                    after = Some(i);
                    break;
                }
                before = Some(i);
            }
        }
        let start = after.or(before)?;
        let mut to = vec![NONE; kept.len() + 1];
        for (n, (kb, kc)) in k.iter().enumerate() {
            let eb = e[start + n].0;
            for j in 0..kc.len_utf8() {
                to[kb + j] = (eb + j) as u32;
            }
        }
        Some(SourceMap { to })
    }

    /// The editor's byte range for the kept code's `from..to`.
    pub fn range(&self, from: usize, to: usize) -> Option<(usize, usize)> {
        let to = to.min(self.to.len().saturating_sub(1));
        let slice = self.to.get(from..to)?;
        let first = slice.iter().find(|x| **x != NONE)?;
        let last = slice.iter().rev().find(|x| **x != NONE)?;
        Some((*first as usize, *last as usize + 1))
    }
}

/// Byte ranges of `code` as (line, column, length) in characters, cut at
/// line ends: what the editor draws its marks with.
pub fn cells(code: &str, ranges: &[(usize, usize)]) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    for &(a, b) in ranges {
        let a = code.floor_char_boundary(a.min(code.len()));
        let b = code.floor_char_boundary(b.min(code.len()));
        if b <= a {
            continue;
        }
        let line = code[..a].matches('\n').count();
        let line_start = code[..a].rfind('\n').map_or(0, |i| i + 1);
        let col = code[line_start..a].chars().count();
        let seg = &code[a..b];
        let len = seg.split('\n').next().unwrap_or("").chars().count();
        if len > 0 {
            out.push((line, col, len));
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes_of(code: &str) -> String {
        classify(code)
            .into_iter()
            .map(|c| match c {
                Class::Plain => '.',
                Class::Comment => 'c',
                Class::Str => 's',
                Class::Number => 'n',
                Class::Word => 'w',
                Class::Control => 'k',
                Class::Head => 'h',
                Class::Op => 'o',
                Class::Unknown => '?',
            })
            .collect()
    }

    #[test]
    fn every_character_has_a_class() {
        assert_eq!(classes_of(r#"d1 $ fast 2 $ s "bd*2" # gian 1 -- hi"#),
                   "hh.o.wwww.n.o.k.ossoso.o.????.n.ccccc");
        assert_eq!(classes_of("let kick = s \"bd\"\nd1 $ kick"), "hhh......o.k.osso.hh.o.....");
        assert_eq!(classes_of("n (\"0 .. 7\" + 12)"), "k..osossoso.o.nn.");
        // Layers keep each character in exactly one text.
        let code = "d1 $ s \"bd\" # gain 1.2\n-- note";
        let l = layers(code);
        for i in 0..code.chars().count() {
            let shown = l.iter().filter(|t| !t.chars().nth(i).unwrap().is_whitespace()).count();
            let c = code.chars().nth(i).unwrap();
            assert_eq!(shown, usize::from(!c.is_whitespace()), "{c:?} at {i}");
        }
        assert!(l.iter().all(|t| t.chars().count() == code.chars().count()));
    }

    #[test]
    fn the_word_at_the_cursor_its_hint_and_completions() {
        let code = "d1 $ every 4 (fast 2) $ s \"bd\"";
        assert_eq!(word_at(code, 8).map(|(a, b)| &code[a..b]), Some("every"));
        assert_eq!(word_at(code, 10).map(|(a, b)| &code[a..b]), Some("every"));
        assert_eq!(word_at(code, 28), None, "inside a string");
        assert!(hint("every").unwrap().contains("every n cycles"));
        assert!(hint("room").unwrap().contains("reverb"));
        assert!(hint("stutWith").is_some());
        let c = completions("d1 $ fa", 7);
        assert_eq!(c.first().map(String::as_str), Some("fast"));
        assert!(c.contains(&"fastGap".to_string()));
        assert_eq!(completions("d1 $ s \"fa", 10), Vec::<String>::new());
        // A let's name completes too.
        assert_eq!(completions("let wobble = sine\nd1 $ s \"x\" # pan wob", 38), ["wobble"]);
    }

    #[test]
    fn sounds_in_strings_complete_hint_and_show_when_unknown() {
        let dir = crate::samples::test_root().join("zzbreaks");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.wav"), b"RIFF").unwrap();
        std::fs::write(dir.join("b.wav"), b"RIFF").unwrap();
        crate::samples::refresh();
        let code = r#"d1 $ s "bd zzq:3 zzbreaks:1 ~ superpiano(3,8)" # n "zzq 0""#;
        let classes = classify(code);
        let at = |w: &str| classes[code.find(w).unwrap()];
        assert_eq!(at("bd"), Class::Str);
        assert_eq!(at("zzq"), Class::Unknown, "no such sound");
        assert_eq!(at("zzbreaks"), Class::Str, "a sample folder");
        assert_eq!(at("superpiano"), Class::Str);
        assert_eq!(classes[code.find("3,8").unwrap()], Class::Str);
        assert_eq!(classes[code.rfind("zzq").unwrap()], Class::Str, "not a sound string");
        // The sound at the cursor, its hint and completions.
        let code = r#"d1 $ sound "bd zzbr"#;
        assert_eq!(sound_at(code, code.len()).map(|(a, b)| &code[a..b]), Some("zzbr"));
        assert_eq!(sound_at(r#"d1 $ n "zz"#, 10), None);
        assert_eq!(completions(code, code.len()), ["zzbreaks"]);
        assert_eq!(tab(code, code.len(), code.len()).0, r#"d1 $ sound "bd zzbreaks"#);
        assert_eq!(completions(r#"d1 $ s "superpi"#, 15), ["superpiano"]);
        assert_eq!(completions(r#"d1 $ s "superp"#, 14), ["superpad", "superpwm", "superpiano"]);
        assert!(sound_hint("zzbreaks").unwrap().contains("2 files, zzbreaks:0 … zzbreaks:1"));
        assert!(sound_hint("bd").unwrap().contains("built-in kick"));
        assert!(sound_hint("zzq").unwrap().contains("no sound 'zzq'"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tab_completes_or_indents() {
        assert_eq!(tab("d1 $ jux re", 11, 11), ("d1 $ jux rev".into(), 12));
        // Several: what they share first.
        let (s, c) = tab("d1 $ someCy", 11, 11);
        assert_eq!(&s[..c], "d1 $ someCycles");
        assert_eq!(tab("d1 $ s \"bd\"", 11, 11), ("d1 $ s \"bd\"  ".into(), 13));
        assert_eq!(tab("a\nb", 0, 3).0, "  a\n  b");
    }

    #[test]
    fn newline_keeps_the_indent_and_goes_on_after_an_open_line() {
        assert_eq!(newline("d1 $ s \"bd\"", 11, 11), ("d1 $ s \"bd\"\n".into(), 12));
        assert_eq!(newline("d1 $ s \"bd\" #", 13, 13).0, "d1 $ s \"bd\" #\n  ");
        assert_eq!(newline("  # gain 1", 10, 10).0, "  # gain 1\n  ");
        assert_eq!(newline("d1 $", 4, 4).0, "d1 $\n  ");
    }

    #[test]
    fn comments_toggle_by_line() {
        let (s, _) = toggle_comment("d1 $ s \"bd\"\n  # gain 1\n\nd2 $ x", 3, 15);
        assert_eq!(s, "-- d1 $ s \"bd\"\n--   # gain 1\n\nd2 $ x");
        let (s, _) = toggle_comment(&s, 0, 20);
        assert_eq!(s, "d1 $ s \"bd\"\n  # gain 1\n\nd2 $ x");
        let (s, c) = toggle_comment("  fast 2", 4, 4);
        assert_eq!(s, "  -- fast 2");
        assert!(c <= s.len());
    }

    #[test]
    fn brackets_and_quotes_close_and_step_over() {
        assert_eq!(pair("d1 $ ", 5, 5, '('), Some(("d1 $ ()".into(), 6)));
        assert_eq!(pair("d1 $ s ", 7, 7, '"'), Some(("d1 $ s \"\"".into(), 8)));
        assert_eq!(pair("d1 $ s \"bd\"", 10, 10, '"'), Some(("d1 $ s \"bd\"".into(), 11)), "step over");
        assert_eq!(pair("(fast 2)", 7, 7, ')'), Some(("(fast 2)".into(), 8)));
        assert_eq!(pair("fast", 0, 0, '('), None, "before a word: as typed");
        assert_eq!(pair("a fast b", 2, 6, '('), Some(("a (fast) b".into(), 8)), "wraps");
        assert_eq!(backspace("()", 1, 1), Some((String::new(), 0)));
        assert_eq!(backspace("(x)", 1, 1), None);
    }

    #[test]
    fn kept_code_is_found_in_the_editor() {
        let code = "-- intro\nd1 $ s \"bd*2\"\n  # gain 0.9 -- loud\n\nd2 $ s \"bd*2\"";
        let kept = "s \"bd*2\"\n  # gain 0.9";
        let m = SourceMap::find(kept, code, 0).unwrap();
        let (a, b) = m.range(3, 5).unwrap();
        assert_eq!(&code[a..b], "bd");
        assert_eq!(a, code.find("bd").unwrap());
        // Near d2's line: the second copy.
        let m = SourceMap::find("s \"bd*2\"", code, code.find("d2").unwrap()).unwrap();
        let (a, _) = m.range(3, 5).unwrap();
        assert_eq!(a, code.rfind("bd").unwrap());
        // Edited since it ran: not found.
        assert!(SourceMap::find("s \"sn*2\"", code, 0).is_none());
        assert_eq!(cells(code, &[(a, a + 2)]), [(4, 8, 2)]);
    }
}
