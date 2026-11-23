//! Big block letters for the text the effects play with: a track title in a
//! 5×7 font, each lit pixel a '█' (two columns wide when it fits: terminal
//! cells are about twice as tall as wide), the artist small below.

/// A track's name without what's in brackets or after a dash ("Insane
/// (Original Mix)" → "Insane", "Losing It - Extended" → "Losing It"): the
/// song, not its version.
pub fn clean_title(title: &str) -> String {
    // Strip a leading filename number (e.g. "10. Friend Within...")
    // before the bracket/dash stripping so titles from untagged files
    // don't show the track number.
    let title = title.trim_start();
    let title = if let Some(pos) = title.find(". ") {
        let before = &title[..pos];
        if before.chars().all(|c| c.is_ascii_digit()) {
            &title[pos + ". ".len()..]
        } else {
            title
        }
    } else {
        title
    };
    let mut out = String::new();
    let mut depth = 0i32;
    for c in title.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = (depth - 1).max(0),
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let out = out.split(" - ").next().unwrap_or("").to_string();
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.is_empty() {
        title.trim().to_string()
    } else {
        out
    }
}

const W: usize = 5;
pub const H: usize = 7;

/// A 5×7 glyph: seven rows, the five low bits of each (bit 4 is the left).
fn glyph(c: char) -> Option<[u8; H]> {
    let rows: [&str; H] = match c {
        'A' => [
            ".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#",
        ],
        'B' => [
            "####.", "#...#", "#...#", "####.", "#...#", "#...#", "####.",
        ],
        'C' => [
            ".###.", "#...#", "#....", "#....", "#....", "#...#", ".###.",
        ],
        'D' => [
            "####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####.",
        ],
        'E' => [
            "#####", "#....", "#....", "####.", "#....", "#....", "#####",
        ],
        'F' => [
            "#####", "#....", "#....", "####.", "#....", "#....", "#....",
        ],
        'G' => [
            ".###.", "#...#", "#....", "#.###", "#...#", "#...#", ".####",
        ],
        'H' => [
            "#...#", "#...#", "#...#", "#####", "#...#", "#...#", "#...#",
        ],
        'I' => [
            "#####", "..#..", "..#..", "..#..", "..#..", "..#..", "#####",
        ],
        'J' => [
            "..###", "...#.", "...#.", "...#.", "...#.", "#..#.", ".##..",
        ],
        'K' => [
            "#...#", "#..#.", "#.#..", "##...", "#.#..", "#..#.", "#...#",
        ],
        'L' => [
            "#....", "#....", "#....", "#....", "#....", "#....", "#####",
        ],
        'M' => [
            "#...#", "##.##", "#.#.#", "#.#.#", "#...#", "#...#", "#...#",
        ],
        'N' => [
            "#...#", "#...#", "##..#", "#.#.#", "#..##", "#...#", "#...#",
        ],
        'O' => [
            ".###.", "#...#", "#...#", "#...#", "#...#", "#...#", ".###.",
        ],
        'P' => [
            "####.", "#...#", "#...#", "####.", "#....", "#....", "#....",
        ],
        'Q' => [
            ".###.", "#...#", "#...#", "#...#", "#.#.#", "#..#.", ".##.#",
        ],
        'R' => [
            "####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#",
        ],
        'S' => [
            ".####", "#....", "#....", ".###.", "....#", "....#", "####.",
        ],
        'T' => [
            "#####", "..#..", "..#..", "..#..", "..#..", "..#..", "..#..",
        ],
        'U' => [
            "#...#", "#...#", "#...#", "#...#", "#...#", "#...#", ".###.",
        ],
        'V' => [
            "#...#", "#...#", "#...#", "#...#", "#...#", ".#.#.", "..#..",
        ],
        'W' => [
            "#...#", "#...#", "#...#", "#.#.#", "#.#.#", "#.#.#", ".#.#.",
        ],
        'X' => [
            "#...#", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "#...#",
        ],
        'Y' => [
            "#...#", "#...#", ".#.#.", "..#..", "..#..", "..#..", "..#..",
        ],
        'Z' => [
            "#####", "....#", "...#.", "..#..", ".#...", "#....", "#####",
        ],
        '0' => [
            ".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###.",
        ],
        '1' => [
            "..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###.",
        ],
        '2' => [
            ".###.", "#...#", "....#", "...#.", "..#..", ".#...", "#####",
        ],
        '3' => [
            "####.", "....#", "....#", ".###.", "....#", "....#", "####.",
        ],
        '4' => [
            "...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#.",
        ],
        '5' => [
            "#####", "#....", "####.", "....#", "....#", "#...#", ".###.",
        ],
        '6' => [
            ".###.", "#....", "#....", "####.", "#...#", "#...#", ".###.",
        ],
        '7' => [
            "#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#...",
        ],
        '8' => [
            ".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###.",
        ],
        '9' => [
            ".###.", "#...#", "#...#", ".####", "....#", "....#", ".###.",
        ],
        '!' => [
            "..#..", "..#..", "..#..", "..#..", "..#..", ".....", "..#..",
        ],
        '?' => [
            ".###.", "#...#", "....#", "...#.", "..#..", ".....", "..#..",
        ],
        '.' => [
            ".....", ".....", ".....", ".....", ".....", ".....", "..#..",
        ],
        ',' => [
            ".....", ".....", ".....", ".....", ".....", "..#..", ".#...",
        ],
        '\'' => [
            "..#..", "..#..", ".....", ".....", ".....", ".....", ".....",
        ],
        '-' => [
            ".....", ".....", ".....", ".###.", ".....", ".....", ".....",
        ],
        '+' => [
            ".....", "..#..", "..#..", "#####", "..#..", "..#..", ".....",
        ],
        ':' => [
            ".....", "..#..", ".....", ".....", ".....", "..#..", ".....",
        ],
        '/' => [
            "....#", "....#", "...#.", "..#..", ".#...", "#....", "#....",
        ],
        '&' => [
            ".##..", "#..#.", "#.#..", ".#...", "#.#.#", "#..#.", ".##.#",
        ],
        '(' => [
            "...#.", "..#..", ".#...", ".#...", ".#...", "..#..", "...#.",
        ],
        ')' => [
            ".#...", "..#..", "...#.", "...#.", "...#.", "..#..", ".#...",
        ],
        '#' => [
            ".#.#.", ".#.#.", "#####", ".#.#.", "#####", ".#.#.", ".#.#.",
        ],
        '$' => [
            "..#..", ".####", "#.#..", ".###.", "..#.#", "####.", "..#..",
        ],
        '%' => [
            "##..#", "##..#", "...#.", "..#..", ".#...", "#..##", "#..##",
        ],
        ' ' => [
            ".....", ".....", ".....", ".....", ".....", ".....", ".....",
        ],
        _ => return None,
    };
    let mut g = [0u8; H];
    for (r, s) in rows.iter().enumerate() {
        for (i, b) in s.bytes().enumerate() {
            if b == b'#' {
                g[r] |= 1 << (W - 1 - i);
            }
        }
    }
    Some(g)
}

/// Letters the font has: uppercase, accents dropped, the rest a space.
fn fold(c: char) -> char {
    let c = match c {
        'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' | 'À' | 'Á' | 'Â' | 'Ä' | 'Ã' | 'Å' => 'A',
        'è' | 'é' | 'ê' | 'ë' | 'È' | 'É' | 'Ê' | 'Ë' => 'E',
        'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => 'I',
        'ò' | 'ó' | 'ô' | 'ö' | 'õ' | 'ø' | 'Ò' | 'Ó' | 'Ô' | 'Ö' | 'Õ' | 'Ø' => 'O',
        'ù' | 'ú' | 'û' | 'ü' | 'Ù' | 'Ú' | 'Û' | 'Ü' => 'U',
        'ñ' | 'Ñ' => 'N',
        'ç' | 'Ç' => 'C',
        'ß' => 'S',
        '’' | '‘' | '`' => '\'',
        '–' | '—' | '_' => '-',
        c => c,
    };
    let u = c.to_ascii_uppercase();
    if glyph(u).is_some() { u } else { ' ' }
}

/// One line of block letters, `px` columns per pixel.
fn line(text: &str, px: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().map(fold).collect();
    (0..H)
        .map(|r| {
            let mut s = String::new();
            for (i, c) in chars.iter().enumerate() {
                let g = glyph(*c).unwrap_or([0; H]);
                for b in (0..W).rev() {
                    let on = g[r] & (1 << b) != 0;
                    for _ in 0..px {
                        s.push(if on { '█' } else { ' ' });
                    }
                }
                if i + 1 < chars.len() {
                    for _ in 0..px {
                        s.push(' ');
                    }
                }
            }
            s.trim_end().to_string()
        })
        .collect()
}

fn width(text: &str, px: usize) -> usize {
    let n = text.chars().count();
    if n == 0 { 0 } else { n * (W + 1) * px - px }
}

/// Split into at most `lines` lines of words that each fit `cols` at `px`.
fn wrap(text: &str, cols: usize, px: usize, lines: usize) -> Option<Vec<String>> {
    let mut out: Vec<String> = vec![];
    for word in text.split_whitespace() {
        match out.last_mut() {
            Some(l) if width(&format!("{l} {word}"), px) <= cols => {
                l.push(' ');
                l.push_str(word);
            }
            _ => out.push(word.to_string()),
        }
    }
    (out.len() <= lines && out.iter().all(|l| width(l, px) <= cols)).then_some(out)
}

/// `text` on one line as large as fits `cols` × `rows`: each pixel `s`
/// rows tall and `2s` columns wide, sized to the canvas (a short word fills the
/// window). Falls back to [`banner`] when even the smallest doesn't fit.
pub fn huge(text: &str, cols: usize, rows: usize) -> String {
    let text = text.trim();
    let largest = (rows / H).min(cols / width(text, 2).max(1)).min(64);
    for s in (1..=largest).rev() {
        if width(text, 2 * s) <= cols && H * s <= rows {
            let mut out = vec![];
            for l in line(text, 2 * s) {
                for _ in 0..s {
                    out.push(l.clone());
                }
            }
            return out.join("\n");
        }
    }
    banner(text, "", cols, rows)
}

/// Whether `title` fits `cols` × `rows` with square pixels (two columns
/// each), on one or two lines: else the canvas wants smaller cells.
pub fn fits_big(title: &str, cols: usize, rows: usize) -> bool {
    let title = title.trim();
    (1..=2).any(|n| n * H + (n - 1) <= rows && wrap(title, cols, 2, n).is_some())
}

/// The title in block letters (as large as fits `cols` × `rows`, over two
/// lines if needed) and `small` below it in plain letters, every line
/// centred. What ttfx animates.
pub fn banner(title: &str, small: &str, cols: usize, rows: usize) -> String {
    let title = title.trim();
    let small: String = small.trim().to_uppercase().chars().take(cols).collect();
    let room = rows.saturating_sub(if small.is_empty() { 0 } else { 2 });
    let mut out: Vec<String> = vec![];
    // Biggest first: two columns per pixel, one line; then two lines; then
    // one column per pixel; else the title in plain letters.
    let tries = [(2, 1), (2, 2), (1, 1), (1, 2)];
    let fitted = tries.iter().find_map(|&(px, n)| {
        if n * H + (n - 1) > room {
            return None;
        }
        wrap(title, cols, px, n).map(|ls| (px, ls))
    });
    match fitted {
        Some((px, ls)) => {
            for (i, l) in ls.iter().enumerate() {
                if i > 0 {
                    out.push(String::new());
                }
                out.extend(line(l, px));
            }
        }
        None => out.push(title.to_uppercase().chars().take(cols).collect()),
    }
    if !small.is_empty() {
        out.push(String::new());
        out.push(small);
    }
    let wide = out.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    out.iter()
        .map(|l| {
            let pad = (wide - l.chars().count()) / 2;
            format!("{}{}", " ".repeat(pad), l)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_when_it_fits_smaller_when_not() {
        let b = banner("Patch", "", 80, 20);
        let lines: Vec<&str> = b.lines().collect();
        assert_eq!(lines.len(), 7);
        // Five letters, 6 pixels each but the last gap, two columns a pixel.
        assert_eq!(lines.iter().map(|l| l.chars().count()).max(), Some(58));
        let b = banner("Tell Me Why Tonight Forever", "Supermode", 80, 20);
        assert!(b.lines().all(|l| l.chars().count() <= 80), "{b}");
        assert!(b.lines().last().unwrap().contains("SUPERMODE"));
    }

    #[test]
    fn versions_are_left_out() {
        assert_eq!(clean_title("Insane  (Original Mix)"), "Insane");
        assert_eq!(
            clean_title("Tell Me Why [Supermode Remix] (Extended)"),
            "Tell Me Why"
        );
        assert_eq!(clean_title("Losing It - Extended Mix"), "Losing It");
        assert_eq!(clean_title("(Intro)"), "(Intro)");
    }

    #[test]
    fn accents_and_unknown_characters() {
        let b = banner("Café ☕", "", 80, 10);
        assert_eq!(b.lines().count(), 7);
    }
}
