//! Titles in a restrained, contemporary style: clean uppercase type in
//! near-white, the brand red only as an accent, and motion that comes from
//! the cell grid itself (decoding glyphs, masked reveals, light).
use super::*;

const WHITE: [u8; 3] = [242, 240, 236];
const SOFT: [u8; 3] = [178, 176, 182];
const DIM: [u8; 3] = [64, 64, 72];
const RED: [u8; 3] = [240, 36, 59];

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t) as u8)
}

fn ease_out(p: f32) -> f32 {
    let p = p.clamp(0.0, 1.0);
    1.0 - (1.0 - p).powi(3)
}

/// The title in capitals (the art is centred as every act's is).
fn upper<'a>(c: &Ctx<'a>, title: &'a str) -> Ctx<'a> {
    Ctx { title, ..*c }
}

/// The title in its own measured box (about 62 % of the width, a third of
/// the height), centred: large enough for an artist line under it, never
/// edge to edge.
fn title_box(c: &Ctx) -> (Bits, isize, isize) {
    let b = art(c, c.title, (c.cols as f32 * 0.62) as usize, (c.rows as f32 * 0.32) as usize);
    if !b.on.iter().any(|x| *x) {
        return title_art(c);
    }
    let x0 = (c.cols as isize - b.w as isize) / 2;
    let y0 = (c.rows as isize - b.h as isize) / 2 - b.h as isize / 3;
    (b, x0, y0)
}

/// Which letter (run of lit columns) each column belongs to.
fn letter_of(b: &Bits) -> Vec<usize> {
    let mut of = vec![0; b.w];
    for (k, (a, z)) in b.letters().into_iter().enumerate() {
        for v in of.iter_mut().take(z).skip(a) {
            *v = k;
        }
    }
    of
}

/// The artist under the title, centred: (art, x0, y), or None.
fn artist_line(c: &Ctx, top: isize, below: isize) -> Option<(Bits, isize, isize)> {
    let a = c.artist.trim().to_uppercase();
    if a.is_empty() {
        return None;
    }
    // About half the title's height, at least five rows (fewer can't be
    // read as type), always smaller than the title.
    let title_h = (below - top).max(0) as usize;
    let cols = (c.cols as f32 * 0.7) as usize;
    let want = (title_h * 55 / 100).max(5);
    let fits = |b: &Bits| b.on.iter().any(|x| *x) && b.h < title_h;
    let mut b = art(c, &a, cols, want);
    // A small title (7 rows): five rows is too small for the face and the
    // block letters are taller than the title. One row under it fits.
    if !fits(&b) && title_h > want + 1 {
        b = art(c, &a, cols, title_h - 1);
    }
    if !fits(&b) {
        return None;
    }
    let gap = (b.h as isize / 2).max(2);
    let y = if below + gap + b.h as isize <= c.rows as isize {
        below + gap
    } else if top - gap - b.h as isize >= 0 {
        top - gap - b.h as isize
    } else {
        return None;
    };
    let x0 = (c.cols as isize - b.w as isize) / 2;
    Some((b, x0, y))
}

const NOISE: &[char] = &['#', '%', '&', '*', '+', '=', '/', '\\', '<', '>', '0', '1', ':', '·'];

/// Decode: glyph noise in red resolves letter by letter into clean type.
pub(super) fn act_decode(g: &mut Grid, c: &Ctx) {
    let up = c.title.to_uppercase();
    let cu = upper(c, &up);
    let (b, x0, y0) = title_box(&cu);
    let of = letter_of(&b);
    let six = (c.beat * 4.0).floor() as u64;
    for y in 0..b.h {
        for x in 0..b.w {
            if !b.on[y * b.w + x] {
                continue;
            }
            let id = (y * b.w + x) as u64;
            let at = 0.4 + of[x] as f64 * 0.22 + hash(id ^ 0x5eed) as f64 * 0.9;
            let (px, py) = (x0 + x as isize, y0 + y as isize);
            if c.age < at {
                // Not resolved yet: sparse noise, warmer as its moment nears.
                if hash(id ^ six.wrapping_mul(31)) < 0.55 {
                    let ch = NOISE[(hash(id.wrapping_add(six)) * NOISE.len() as f32) as usize % NOISE.len()];
                    let near = (1.0 - (at - c.age) as f32 / 1.2).clamp(0.0, 1.0);
                    put(g, px, py, ch, mix(DIM, RED, near));
                }
            } else {
                let fresh = ((c.age - at) as f32 / 0.5).clamp(0.0, 1.0);
                // A rare cell flickers back to noise on a hard kick.
                let glitch = c.kick() > 0.85 && hash(id ^ (c.music_beat.floor() as u64) << 7) < 0.02;
                if glitch {
                    put(g, px, py, NOISE[id as usize % NOISE.len()], RED);
                } else {
                    put(g, px, py, b.shape[y * b.w + x], mix(RED, WHITE, fresh));
                }
            }
        }
    }
    if let Some((a, ax, ay)) = artist_line(&cu, y0, y0 + b.h as isize) {
        let shown = ((c.age - 2.5) / 2.0).clamp(0.0, 1.0) as f32;
        let upto = (a.w as f32 * shown) as usize;
        for y in 0..a.h {
            for x in 0..upto.min(a.w) {
                if a.on[y * a.w + x] {
                    let edge = upto.saturating_sub(x) < 3 && shown < 1.0;
                    put(g, ax + x as isize, ay + y as isize, a.shape[y * a.w + x], if edge { RED } else { SOFT });
                }
            }
        }
    }
}

/// Rise: each letter rises in turn from behind a baseline; a red rule
/// draws out from the centre; the artist fades in under it.
pub(super) fn act_rise(g: &mut Grid, c: &Ctx) {
    let up = c.title.to_uppercase();
    let cu = upper(c, &up);
    let (b, x0, y0) = title_box(&cu);
    let of = letter_of(&b);
    let base = y0 + b.h as isize - 1;
    for y in 0..b.h {
        for x in 0..b.w {
            if !b.on[y * b.w + x] {
                continue;
            }
            let p = ((c.age - 0.2 - of[x] as f64 * 0.18) / 1.6) as f32;
            if p <= 0.0 {
                continue;
            }
            let dy = ((1.0 - ease_out(p)) * b.h as f32).round() as isize;
            let py = y0 + y as isize + dy;
            // The mask: nothing shows below the baseline.
            if py <= base {
                put(g, x0 + x as isize, py, b.shape[y * b.w + x], mix(SOFT, WHITE, p * 1.5));
            }
        }
    }
    // The rule, one row under the title, drawn out from the centre.
    let rule = ease_out(((c.age - 0.6) / 2.0) as f32);
    let half = (b.w as f32 * 0.5 * rule) as isize;
    let mid = x0 + b.w as isize / 2;
    for x in (mid - half)..(mid + half) {
        put(g, x, base + 2, '▔', RED);
    }
    if let Some((a, ax, ay)) = artist_line(&cu, y0, base + 3) {
        let f = ((c.age - 2.4) / 1.5).clamp(0.0, 1.0) as f32;
        if f > 0.0 {
            for y in 0..a.h {
                for x in 0..a.w {
                    if a.on[y * a.w + x] {
                        put(g, ax + x as isize, ay + y as isize, a.shape[y * a.w + x], mix([0, 0, 0], SOFT, f));
                    }
                }
            }
        }
    }
}

/// Sheen: a calm grey title; once a bar a band of white light sweeps across
/// it with a red edge trailing; the artist in red.
pub(super) fn act_sheen(g: &mut Grid, c: &Ctx) {
    let up = c.title.to_uppercase();
    let cu = upper(c, &up);
    let (b, x0, y0) = title_box(&cu);
    let fade = ((c.age / 1.5) as f32).clamp(0.0, 1.0);
    let w = b.w as f32 + b.h as f32 * 1.2;
    let pos = (c.beat / 4.0).rem_euclid(1.0) as f32 * (w + 40.0) - 20.0;
    let width = (b.w as f32 * 0.06).max(3.0);
    for y in 0..b.h {
        for x in 0..b.w {
            if !b.on[y * b.w + x] {
                continue;
            }
            let d = x as f32 + (b.h - y) as f32 * 1.2 - pos;
            let light = (-(d / width).powi(2)).exp();
            let edge = (-((d + width * 2.2) / (width * 0.8)).powi(2)).exp();
            let col = mix(mix([120, 120, 128], RED, edge * 0.85), WHITE, light);
            put(g, x0 + x as isize, y0 + y as isize, b.shape[y * b.w + x], mix([0, 0, 0], col, fade));
        }
    }
    if let Some((a, ax, ay)) = artist_line(&cu, y0, y0 + b.h as isize) {
        let f = ((c.age - 1.0) / 1.5).clamp(0.0, 1.0) as f32;
        for y in 0..a.h {
            for x in 0..a.w {
                if a.on[y * a.w + x] {
                    put(g, ax + x as isize, ay + y as isize, a.shape[y * a.w + x], mix([0, 0, 0], RED, f));
                }
            }
        }
    }
}

/// Slice: the title in three bands, each wiped in from alternating sides
/// behind a red leading edge.
pub(super) fn act_slice(g: &mut Grid, c: &Ctx) {
    let up = c.title.to_uppercase();
    let cu = upper(c, &up);
    let (b, x0, y0) = title_box(&cu);
    let bands = 3usize;
    for y in 0..b.h {
        let band = (y * bands / b.h.max(1)).min(bands - 1);
        let p = ease_out(((c.age - 0.2 - band as f64 * 0.45) / 1.8) as f32);
        let reach = (p * (b.w as f32 + 2.0)) as isize;
        let from_left = band.is_multiple_of(2);
        for x in 0..b.w {
            let k = if from_left { x as isize } else { b.w as isize - 1 - x as isize };
            if k > reach || !b.on[y * b.w + x] {
                continue;
            }
            let lead = p < 1.0 && reach - k < 2;
            put(g, x0 + x as isize, y0 + y as isize, b.shape[y * b.w + x], if lead { RED } else { WHITE });
        }
    }
    if let Some((a, ax, ay)) = artist_line(&cu, y0, y0 + b.h as isize) {
        let p = ease_out(((c.age - 1.8) / 1.6) as f32);
        let reach = (p * a.w as f32) as usize;
        for y in 0..a.h {
            for x in 0..reach.min(a.w) {
                if a.on[y * a.w + x] {
                    put(g, ax + x as isize, ay + y as isize, a.shape[y * a.w + x], SOFT);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_small_title_keeps_its_artist() {
        let fonts = crate::fonts::Fonts::load();
        let pal = crate::pack::PALETTES[0];
        let face = (0..fonts.len()).find(|&f| fonts.name(f) == "narrow").map_or(1, |f| f as u8 + 1);
        for size in 0..3u8 {
            let c = Ctx { cols: 240, rows: 68, title: "IT'S THAT TIME", artist: "Marlon Hoffstadt, FISHER", size, place: 0, font: face, fonts: Some(&fonts), age: 8.0, ..super::super::tests::ctx(12.0, &pal, &[], &[]) };
            let (b, _, y0) = title_box(&c);
            let a = artist_line(&c, y0, y0 + b.h as isize);
            eprintln!("size {size}: title {}x{} artist {:?}", b.w, b.h, a.as_ref().map(|(a, x, y)| (a.w, a.h, *x, *y)));
            assert!(a.is_some(), "size {size} lost its artist");
        }
    }
}
