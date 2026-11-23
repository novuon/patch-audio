// Adapted from avillagran/omarchy-ttfx-layer, MIT, c969b6c172108c5f6ab937d99db67c17ddd28196.
// See LICENSE and README.md in this directory.
use super::raster::Raster;
type Color = (u8, u8, u8);
fn glyph(c: char) -> [u8; 5] {
    match c.to_ascii_uppercase() {
        '0' => [7, 5, 5, 5, 7],
        '1' => [2, 6, 2, 2, 7],
        '2' => [7, 1, 7, 4, 7],
        '3' => [7, 1, 3, 1, 7],
        '4' => [5, 5, 7, 1, 1],
        '5' => [7, 4, 7, 1, 7],
        '6' => [7, 4, 7, 5, 7],
        '7' => [7, 1, 1, 2, 2],
        '8' => [7, 5, 7, 5, 7],
        '9' => [7, 5, 7, 1, 7],
        'A' => [2, 5, 7, 5, 5],
        'B' => [6, 5, 6, 5, 6],
        'C' => [3, 4, 4, 4, 3],
        'D' => [6, 5, 5, 5, 6],
        'E' => [7, 4, 6, 4, 7],
        'F' => [7, 4, 6, 4, 4],
        'G' => [3, 4, 5, 5, 3],
        'H' => [5, 5, 7, 5, 5],
        'I' => [7, 2, 2, 2, 7],
        'J' => [1, 1, 1, 5, 2],
        'K' => [5, 5, 6, 5, 5],
        'L' => [4, 4, 4, 4, 7],
        'M' => [5, 7, 7, 5, 5],
        'N' => [6, 5, 5, 5, 5],
        'O' => [2, 5, 5, 5, 2],
        'P' => [6, 5, 6, 4, 4],
        'Q' => [2, 5, 5, 6, 3],
        'R' => [6, 5, 6, 5, 5],
        'S' => [3, 4, 2, 1, 6],
        'T' => [7, 2, 2, 2, 2],
        'U' => [5, 5, 5, 5, 7],
        'V' => [5, 5, 5, 5, 2],
        'W' => [5, 5, 7, 7, 5],
        'X' => [5, 5, 2, 5, 5],
        'Y' => [5, 5, 2, 2, 2],
        'Z' => [7, 1, 2, 4, 7],
        '%' => [5, 1, 2, 4, 5],
        '.' | '·' => [0, 0, 0, 0, 2],
        '/' => [1, 1, 2, 4, 4],
        ':' => [0, 2, 0, 2, 0],
        '-' => [0, 0, 7, 0, 0],
        '+' => [0, 2, 7, 2, 0],
        '▲' => [0, 2, 7, 7, 0],
        '▼' => [0, 7, 7, 2, 0],
        _ => [0; 5],
    }
}

/// Columns and rows of `s` at `k` cells per font pixel; 0 is plain text.
pub(crate) fn type_size(s: &str, k: usize) -> (usize, usize) {
    let n = s.chars().count();
    if k == 0 {
        return (n, 1);
    }
    ((n * 4).saturating_sub(1) * k, (5 * k).div_ceil(2))
}

/// `s` in the pixel font at `k` cells per font pixel (k >= 1), in half
/// blocks, with its top left corner on a cell of a `cols` by `rows` raster.
pub(crate) fn pixel_text(
    r: &mut Raster,
    (cols, rows): (usize, usize),
    (x0, y0, z): (i32, i32, f32),
    s: &str,
    k: usize,
    c: Color,
) {
    let (_, h) = type_size(s, k);
    for (i, ch) in s.chars().enumerate() {
        let g = glyph(ch);
        if g == [0; 5] {
            continue;
        }
        let gx = x0 + (i * 4 * k) as i32;
        let bit = |half: usize, px: usize| {
            let r = half / k;
            r < 5 && (g[r] >> (2 - px / k)) & 1 == 1
        };
        for row in 0..h {
            let y = y0 + row as i32;
            if y < 0 || y >= rows as i32 {
                continue;
            }
            for px in 0..3 * k {
                let x = gx + px as i32;
                if x < 0 || x >= cols as i32 {
                    continue;
                }
                let ch = match (bit(row * 2, px), bit(row * 2 + 1, px)) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    _ => continue,
                };
                r.dot(x, y, z, ch, c);
            }
        }
    }
}
