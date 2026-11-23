//! The title acts, one function each: how a title plays on the beat.

use super::*;

pub(super) fn act_shatter(g: &mut Grid, c: &Ctx) {
    let into = c.into();
    let (b, x0, y0) = title_art(c);
    // Apart on the kick, home by the next beat along the song's
    // curve (a spring overshoots, a sine glides).
    let out = 1.0 - c.eased((into * 1.6).min(1.0));
    // (At 3 + 18 × bass cells the title was gone at the peak.)
    let reach = (1.0 + 2.5 * c.bass) * out;
    for y in 0..b.h {
        for x in 0..b.w {
            if b.on[y * b.w + x] {
                let h = hash((y * 997 + x) as u64 ^ c.seed);
                let a = h * std::f32::consts::TAU;
                let (dx, dy) = (
                    a.cos() * reach * 2.0 * (0.5 + h),
                    a.sin() * reach * (0.5 + h),
                );
                let ch = if reach < 0.8 {
                    b.shape[y * b.w + x]
                } else if reach < 3.0 {
                    '▓'
                } else {
                    '▒'
                };
                put(
                    g,
                    x0 + x as isize + dx as isize,
                    y0 + y as isize + dy as isize,
                    ch,
                    c.pal.at(x as f32 / b.w as f32),
                );
            }
        }
    }
    byline(g, c, y0, y0 + b.h as isize);
}

pub(super) fn act_wave(g: &mut Grid, c: &Ctx) {
    let kick = c.kick();
    let (b, x0, y0) = title_art(c);
    // Whole letters ride the wave (bending each column shredded
    // the letters, real type worst).
    let amp = (1.0 + 3.0 * c.bass + 1.5 * kick).min(b.h as f32 * 0.2);
    for &(a, z) in &b.letters() {
        let mid = (a + z) as f32 / 2.0;
        let dy = ((mid * 0.18 - c.beat as f32 * std::f32::consts::PI).sin() * amp) as isize;
        for x in a..z {
            for y in 0..b.h {
                if b.on[y * b.w + x] {
                    put(
                        g,
                        x0 + x as isize,
                        y0 + y as isize + dy,
                        b.shape[y * b.w + x],
                        c.pal
                            .at((x as f32 / b.w as f32 + c.beat as f32 * 0.1).fract()),
                    );
                }
            }
        }
    }
    byline(g, c, y0, y0 + b.h as isize + amp as isize);
}

pub(super) fn act_glitch(g: &mut Grid, c: &Ctx) {
    let (b, x0, y0) = title_art(c);
    let s16 = c.sixteenth();
    let glyphs: Vec<char> = "01#%&@$*+=/\\<>▓▒░".chars().collect();
    for y in 0..b.h {
        let slice = y / 2;
        let jump = hash((slice as u64 * 31 + s16 as u64 * 7919) ^ c.seed);
        // Now and then, not all the time: the title stays readable.
        let dx = if jump > 0.8 {
            ((jump - 0.8) * 20.0 * (0.4 + c.high)) as isize
                * (if slice % 2 == 0 { 1 } else { -1 })
        } else {
            0
        };
        for x in 0..b.w {
            if b.on[y * b.w + x] {
                let flick = hash((y * 131 + x) as u64 + s16 as u64 * 13);
                let ch = if flick < 0.03 + 0.07 * c.high {
                    glyphs[(flick * 1000.0) as usize % glyphs.len()]
                } else {
                    b.shape[y * b.w + x]
                };
                let col = if dx != 0 {
                    c.pal.c
                } else if slice % 3 == 0 {
                    c.pal.b
                } else {
                    c.pal.a
                };
                put(g, x0 + x as isize + dx, y0 + y as isize, ch, col);
            }
        }
    }
    byline(g, c, y0, y0 + b.h as isize);
}

pub(super) fn act_type(g: &mut Grid, c: &Ctx) {
    let (b, x0, y0) = title_art(c);
    // Revealed column by column: a letter a 16th, or spread over a
    // long scene (a handover) so the last lands at its end.
    // Whole letters as drawn, a 16th each (or spread over a long
    // scene, a handover, so the last lands at its end).
    let runs = b.letters();
    let letters = runs.len().max(1) as f64;
    let rate = if c.span > 16.0 {
        (letters / (c.span * 0.8).max(1.0)).clamp(0.0, 4.0)
    } else {
        4.0
    };
    let count = (c.age * rate) as usize;
    let shown = if count >= runs.len() {
        b.w
    } else {
        runs.get(count).map_or(0, |r| r.0)
    };
    for y in 0..b.h {
        for x in 0..b.w.min(shown) {
            if b.on[y * b.w + x] {
                put(
                    g,
                    x0 + x as isize,
                    y0 + y as isize,
                    b.shape[y * b.w + x],
                    c.pal.at(x as f32 / b.w as f32),
                );
            }
        }
    }
    if shown < b.w || (c.beat * 2.0).floor() as i64 % 2 == 0 {
        let cx = (x0 + shown.min(b.w) as isize + 1).min(c.cols as isize - 1);
        for y in 0..b.h {
            put(g, cx, y0 + y as isize, '█', c.pal.c);
        }
    }
    if shown >= b.w {
        byline(g, c, y0, y0 + b.h as isize);
    }
}

pub(super) fn act_neon(g: &mut Grid, c: &Ctx) {
    let kick = c.kick();
    let (b, x0, y0) = title_art(c);
    let s16 = c.sixteenth();
    // The tube flickers now and then; a kick makes it blaze.
    let flicker = hash(s16 as u64 ^ c.seed) < 0.12;
    for y in 0..b.h as isize {
        for x in 0..b.w as isize {
            if b.at(x, y) {
                let edge =
                    !(b.at(x - 1, y) && b.at(x + 1, y) && b.at(x, y - 1) && b.at(x, y + 1));
                if edge {
                    let col = if flicker {
                        scale(c.pal.a, 0.35)
                    } else {
                        mix(c.pal.a, c.pal.c, kick)
                    };
                    put(g, x0 + x, y0 + y, '▓', col);
                } else {
                    put(g, x0 + x, y0 + y, '·', scale(c.pal.b, 0.5));
                }
            }
        }
    }
    byline(g, c, y0, y0 + b.h as isize);
}

pub(super) fn act_marquee(g: &mut Grid, c: &Ctx) {
    let art = crate::banner::huge(
        &format!("{}  ·  {}  ·  ", c.title, c.artist),
        usize::MAX / 4,
        c.rows.saturating_sub(4),
    );
    let b = bits(&art);
    let letter = (b.w as f32
        / (c.title.chars().count() + c.artist.chars().count() + 10) as f32)
        .max(1.0);
    let off = ((c.beat as f32 * letter) as usize) % b.w.max(1);
    let y0 = (c.rows as isize - b.h as isize) / 2;
    for y in 0..b.h {
        for x in 0..c.cols {
            let sx = (x + off) % b.w.max(1);
            if b.on[y * b.w + sx] {
                put(
                    g,
                    x as isize,
                    y0 + y as isize,
                    b.shape[y * b.w + sx],
                    c.pal.at(sx as f32 / b.w as f32),
                );
            }
        }
    }
}

pub(super) fn act_bounce(g: &mut Grid, c: &Ctx) {
    let into = c.into();
    let (b, x0, y0) = title_art(c);
    // Each letter (as drawn) jumps in turn, a small hop: the word
    // stays readable.
    let letters = b.letters();
    let n = letters.len().max(1);
    let hop = (2.0 + 2.0 * c.bass).min(b.h as f32 * 0.35);
    for (i, &(a, z)) in letters.iter().enumerate() {
        let ph = (into - i as f32 * 0.06).rem_euclid(1.0);
        let up = ((1.0 - c.eased((ph * 2.0).min(1.0))).clamp(0.0, 1.2) * hop) as isize;
        for x in a..z {
            for y in 0..b.h {
                if b.on[y * b.w + x] {
                    put(
                        g,
                        x0 + x as isize,
                        y0 + y as isize - up,
                        b.shape[y * b.w + x],
                        c.pal.at(i as f32 / n as f32),
                    );
                }
            }
        }
    }
    byline(g, c, y0, y0 + b.h as isize);
}

pub(super) fn act_mirror(g: &mut Grid, c: &Ctx) {
    let (b, x0, _) = title_art(c);
    // The title and its reflection, both on the canvas (a title
    // too tall for two plays without one).
    let room = 2 * b.h < c.rows;
    let y0 = if room {
        (c.rows as isize - (2 * b.h + 1) as isize) / 2
    } else {
        (c.rows as isize - b.h as isize) / 2
    };
    for y in 0..b.h {
        for x in 0..b.w {
            if b.on[y * b.w + x] {
                put(
                    g,
                    x0 + x as isize,
                    y0 + y as isize,
                    b.shape[y * b.w + x],
                    c.pal.at(x as f32 / b.w as f32),
                );
                if !room {
                    continue;
                }
                // The reflection: upside down, rippling, dim.
                let ry = y0 + (2 * b.h - y) as isize;
                let ripple = (1.0 + 2.0 * c.bass).min(x0.max(0) as f32);
                let dx = ((ry as f32 * 0.8 - c.beat as f32 * 3.0).sin() * ripple) as isize;
                put(g, x0 + x as isize + dx, ry, '▒', scale(c.pal.b, 0.45));
            }
        }
    }
}

pub(super) fn act_assemble(g: &mut Grid, c: &Ctx) {
    let (b, x0, y0) = title_art(c);
    // Over most of the scene: each pixel falls from above in its
    // own time (timed to a handover's end, it lands on it).
    let p = (c.age / (c.span * 0.8).max(4.0)).clamp(0.0, 1.0) as f32;
    for y in 0..b.h {
        for x in 0..b.w {
            if b.on[y * b.w + x] {
                let delay = hash((y * 991 + x) as u64 ^ c.seed) * 0.6;
                let k = ((p - delay) / 0.4).clamp(0.0, 1.0);
                let fall = ((1.0 - c.eased(k)) * (y0 + y as isize + 4) as f32) as isize;
                if k > 0.0 {
                    put(
                        g,
                        x0 + x as isize,
                        y0 + y as isize - fall,
                        if k < 1.0 { '▓' } else { b.shape[y * b.w + x] },
                        c.pal.at(x as f32 / b.w as f32),
                    );
                }
            }
        }
    }
    if p >= 1.0 {
        byline(g, c, y0, y0 + b.h as isize);
    }
}

pub(super) fn act_scan(g: &mut Grid, c: &Ctx) {
    let (b, x0, y0) = title_art(c);
    // A line down the canvas each bar; what it passed glows, fading.
    let bar = (c.beat / 4.0).rem_euclid(1.0) as f32;
    let line = bar * c.rows as f32;
    for y in 0..b.h {
        let ry = (y0 + y as isize) as f32;
        let since = (line - ry) / c.rows as f32;
        let glow = if since >= 0.0 {
            (1.0 - since * 1.5).max(0.25)
        } else {
            0.25
        };
        for x in 0..b.w {
            if b.on[y * b.w + x] {
                put(
                    g,
                    x0 + x as isize,
                    y0 + y as isize,
                    b.shape[y * b.w + x],
                    scale(c.pal.at(x as f32 / b.w as f32), glow),
                );
            }
        }
    }
    for x in 0..c.cols {
        put(g, x as isize, line as isize, '▔', c.pal.c);
    }
    byline(g, c, y0, y0 + b.h as isize);
}

pub(super) fn act_stack(g: &mut Grid, c: &Ctx) {
    let kick = c.kick();
    let (b, x0, y0) = title_art(c);
    // Each row slides in from its side, one a 16th, then holds.
    let s16 = (c.age * 4.0) as f32;
    for y in 0..b.h {
        // All rows in within three beats, however tall the title.
        let arrive = y as f32 * (10.0 / b.h.max(1) as f32).min(0.5);
        let k = ((s16 - arrive) / 2.0).clamp(0.0, 1.0);
        let ease = c.eased(k);
        let from = if y % 2 == 0 {
            -(c.cols as f32)
        } else {
            c.cols as f32
        };
        // Settled, the rows twitch apart on each kick.
        let twitch = kick * (1.0 + 2.0 * c.bass) * if y % 2 == 0 { 1.0 } else { -1.0 };
        // Settled rows twitch only as far as the canvas allows.
        let room = ((c.cols as isize - b.w as isize) / 2).max(0) as f32;
        let dx = (from * (1.0 - ease) + twitch.clamp(-room, room)) as isize;
        for x in 0..b.w {
            if b.on[y * b.w + x] {
                put(
                    g,
                    x0 + x as isize + dx,
                    y0 + y as isize,
                    b.shape[y * b.w + x],
                    c.pal.at(y as f32 / b.h as f32),
                );
            }
        }
    }
    byline(g, c, y0, y0 + b.h as isize);
}

pub(super) fn act_pixelate(g: &mut Grid, c: &Ctx) {
    let (b, x0, y0) = title_art(c);
    // Blocks of 8, 4, 2, then the letters, a step each bar (or
    // over the scene), a flash on each step.
    let steps = [8usize, 4, 2, 1];
    let k = (c.age.max(0.0) as usize).min(3);
    // Sharp, then a flash of blocks on each bar's one.
    let k = if k == 3 && c.beat.rem_euclid(4.0) < 0.2 {
        2
    } else {
        k
    };
    let sz = steps[k];
    for by in (0..b.h).step_by(sz) {
        for bx in (0..b.w).step_by(sz * 2) {
            let mut n = 0;
            for y in by..(by + sz).min(b.h) {
                for x in bx..(bx + sz * 2).min(b.w) {
                    n += b.on[y * b.w + x] as usize;
                }
            }
            if n * 3 >= sz * sz * 2 {
                for y in by..(by + sz).min(b.h) {
                    for x in bx..(bx + sz * 2).min(b.w) {
                        put(
                            g,
                            x0 + x as isize,
                            y0 + y as isize,
                            if sz > 1 { '▓' } else { b.shape[y * b.w + x] },
                            c.pal.at(bx as f32 / b.w as f32),
                        );
                    }
                }
            }
        }
    }
    byline(g, c, y0, y0 + b.h as isize);
}

pub(super) fn act_drip(g: &mut Grid, c: &Ctx) {
    let (b, x0, y0) = title_art(c);
    for x in 0..b.w {
        let mut bottom = None;
        for y in 0..b.h {
            if b.on[y * b.w + x] {
                put(
                    g,
                    x0 + x as isize,
                    y0 + y as isize,
                    b.shape[y * b.w + x],
                    c.pal.at(x as f32 / b.w as f32),
                );
                bottom = Some(y);
            }
        }
        // Now and then a drop runs down from the column's bottom.
        if let Some(yb) = bottom {
            let h = hash(x as u64 ^ c.seed);
            if h < 0.35 {
                let speed = 3.0 + 6.0 * hash(x as u64 + 17);
                let len = ((c.t as f32 * speed + h * 50.0) % (c.rows as f32)) as isize;
                for d in 1..len {
                    put(
                        g,
                        x0 + x as isize,
                        y0 + yb as isize + d,
                        if d == len - 1 { '▼' } else { '│' },
                        scale(c.pal.b, 0.7),
                    );
                }
            }
        }
    }
}

pub(super) fn act_echo(g: &mut Grid, c: &Ctx) {
    let into = c.into();
    let (b, x0, y0) = title_art(c);
    // Three ghosts drifting out behind, the newest from this beat.
    // (Faint and close: the title in front stays readable.)
    for ghost in (1..=2).rev() {
        let age = into + ghost as f32 - 1.0;
        let off = (age * (1.0 + 1.5 * c.bass)) as isize + 1;
        // Only as far as there is room right and below.
        let off = off
            .min(c.cols as isize - (x0 + b.w as isize))
            .min(2 * (c.rows as isize - (y0 + b.h as isize)))
            .max(0);
        let fade = (1.0 - age / 2.0).max(0.0) * 0.25;
        for y in 0..b.h {
            for x in 0..b.w {
                if b.on[y * b.w + x] {
                    put(
                        g,
                        x0 + x as isize + off,
                        y0 + y as isize + off / 2,
                        '░',
                        scale(c.pal.b, fade),
                    );
                }
            }
        }
    }
    for y in 0..b.h {
        for x in 0..b.w {
            if b.on[y * b.w + x] {
                put(
                    g,
                    x0 + x as isize,
                    y0 + y as isize,
                    b.shape[y * b.w + x],
                    c.pal.at(x as f32 / b.w as f32),
                );
            }
        }
    }
    byline(g, c, y0, y0 + b.h as isize + 4);
}

pub(super) fn act_split(g: &mut Grid, c: &Ctx) {
    let into = c.into();
    let (b, x0, y0) = title_art(c);
    let apart = ((1.0 - c.eased((into * 1.5).min(1.0))) * (2.0 + 4.0 * c.bass)) as isize;
    let half = b.h / 2;
    for y in 0..b.h {
        let dy = if y < half { -apart } else { apart };
        for x in 0..b.w {
            if b.on[y * b.w + x] {
                let col = if y < half { c.pal.a } else { c.pal.b };
                put(
                    g,
                    x0 + x as isize + if y < half { apart } else { -apart },
                    y0 + y as isize + dy,
                    b.shape[y * b.w + x],
                    col,
                );
            }
        }
    }
    // A bright seam where they parted.
    if apart > 0 {
        for x in 0..b.w {
            put(g, x0 + x as isize, y0 + half as isize, '─', c.pal.c);
        }
    }
    byline(g, c, y0, y0 + b.h as isize + apart);
}

pub(super) fn act_credits(g: &mut Grid, c: &Ctx) {
    // Title and artist rising from below, a row a beat; both
    // letter art, the artist a readable line under the title.
    let t = art(
        c,
        c.title,
        c.cols.saturating_sub(4),
        (c.rows as f32 * 0.4) as usize,
    );
    let a = c.artist.trim().to_uppercase();
    let min = line_rows(c);
    let who = (!a.is_empty())
        .then(|| artist_art(c, &a, (c.cols as f32 * 0.85) as usize, t.h))
        .flatten();
    let gap = (min / 2).max(1);
    let h = t.h + who.as_ref().map_or(0, |w| gap + w.h);
    let off = (c.beat as f32).rem_euclid((h + c.rows) as f32) as isize;
    let y0 = c.rows as isize - off;
    let tw = t.w.max(1) as f32;
    put_art(g, &t, y0, |x| c.pal.at(x as f32 / tw));
    if let Some(w) = &who {
        put_art(g, w, y0 + (t.h + gap) as isize, |_| c.pal.b);
    }
}

pub(super) fn act_flip(g: &mut Grid, c: &Ctx) {
    let into = c.into();
    let (b, x0, y0) = title_art(c);
    // Each letter turns edge-on around its middle on the beat,
    // one after the other.
    let letters = b.letters();
    let n = letters.len().max(1);
    for (i, &(a, z)) in letters.iter().enumerate() {
        let ph = (into - i as f32 * 0.04).rem_euclid(1.0);
        // A quick turn at the start of each beat, then it reads.
        let squash = if ph < 0.3 {
            (ph / 0.3 * std::f32::consts::PI).cos().abs().max(0.35)
        } else {
            1.0
        };
        let centre = (a + z) as f32 / 2.0;
        for x in a..z {
            // Where this column lands, squeezed towards the middle.
            let dx = centre + (x as f32 + 0.5 - centre) * squash;
            for y in 0..b.h {
                if b.on[y * b.w + x] {
                    let col = if squash < 0.5 {
                        c.pal.c
                    } else {
                        c.pal.at(i as f32 / n as f32)
                    };
                    put(
                        g,
                        x0 + dx as isize,
                        y0 + y as isize,
                        b.shape[y * b.w + x],
                        col,
                    );
                }
            }
        }
    }
    byline(g, c, y0, y0 + b.h as isize);
}

pub(super) fn act_spotlight(g: &mut Grid, c: &Ctx) {
    let kick = c.kick();
    let (b, x0, y0) = title_art(c);
    // A light sweeping across and back each bar, a flash on the kick.
    let bar = (c.beat / 4.0).rem_euclid(2.0) as f32;
    let sx = if bar < 1.0 { bar } else { 2.0 - bar } * b.w as f32;
    let reach = b.w as f32 * (0.18 + 0.1 * kick);
    for y in 0..b.h {
        for x in 0..b.w {
            if b.on[y * b.w + x] {
                let d = ((x as f32 - sx) / reach).powi(2)
                    + ((y as f32 - b.h as f32 / 2.0) / (b.h as f32)).powi(2);
                let light = (1.0 - d).max(0.5);
                put(
                    g,
                    x0 + x as isize,
                    y0 + y as isize,
                    b.shape[y * b.w + x],
                    scale(c.pal.at(x as f32 / b.w as f32), light),
                );
            }
        }
    }
    byline(g, c, y0, y0 + b.h as isize);
}

pub(super) fn act_tile(g: &mut Grid, c: &Ctx) {
    // Title · artist, small, over and over, each row running its way.
    // Readable lines (letter art), each a row of the pattern.
    let line = format!(
        "{}  ·  {}  ·  ",
        c.title.to_uppercase(),
        c.artist.to_uppercase()
    );
    let h = line_rows(c);
    let step = h + (h / 3).max(1);
    for (k, y) in (0..c.rows).step_by(step).enumerate() {
        let dir = if k % 2 == 0 { 1.0 } else { -1.0 };
        let off = (c.beat * 2.0 * dir) as isize + (k as isize * 37);
        let lift = if k as i64 % 4 == (c.beat.floor() as i64).rem_euclid(4) {
            1.0
        } else {
            0.55
        };
        scroll_line(g, c, &line, y as isize, h, off, |f| {
            scale(c.pal.at(f), lift)
        });
    }
}

pub(super) fn act_orbit(g: &mut Grid, c: &Ctx) {
    let into = c.into();
    // Each letter on a ring, the ring turning a quarter a bar,
    // breathing out on the kick; a second ring the other way.
    let letters: Vec<char> = c
        .title
        .to_uppercase()
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect();
    let n = letters.len().max(1);
    let (cx, cy) = (c.cols as f32 / 2.0, c.rows as f32 / 2.0);
    for (ring, dir, r0) in [(0, 1.0f32, 0.6f32), (1, -1.0, 0.35)] {
        let r = r0 * (1.0 + 0.12 * c.eased(1.0 - into) * c.bass);
        let turn = c.beat as f32 * 0.4 * dir;
        for (i, ch) in letters.iter().enumerate() {
            let a = i as f32 / n as f32 * std::f32::consts::TAU + turn;
            let (x, y) = (cx + a.cos() * r * cx, cy + a.sin() * r * cy);
            let col = if ring == 0 {
                c.pal.at(i as f32 / n as f32)
            } else {
                scale(c.pal.b, 0.6)
            };
            if ring == 0 {
                // The outer ring in block letters.
                let b = art(
                    c,
                    &ch.to_string(),
                    (c.cols / 16).max(12),
                    (c.rows / 8).max(7),
                );
                for yy in 0..b.h {
                    for xx in 0..b.w {
                        if b.on[yy * b.w + xx] {
                            put(
                                g,
                                x as isize + xx as isize - b.w as isize / 2,
                                y as isize + yy as isize - b.h as isize / 2,
                                b.shape[yy * b.w + xx],
                                col,
                            );
                        }
                    }
                }
            } else {
                // The inner ring smaller, still letters you can read.
                let b = art(
                    c,
                    &ch.to_string(),
                    (c.cols / 24).max(8),
                    (c.rows / 12).max(line_rows(c)),
                );
                for yy in 0..b.h {
                    for xx in 0..b.w {
                        if b.on[yy * b.w + xx] {
                            put(
                                g,
                                x as isize + xx as isize - b.w as isize / 2,
                                y as isize + yy as isize - b.h as isize / 2,
                                b.shape[yy * b.w + xx],
                                col,
                            );
                        }
                    }
                }
            }
        }
    }
    // The artist in the middle, when it fits inside the inner ring.
    let a = c.artist.trim().to_uppercase();
    if !a.is_empty() {
        let b = art(c, &a, (c.cols as f32 * 0.4) as usize, line_rows(c));
        if readable(&b, c) {
            put_art(g, &b, (c.rows as isize - b.h as isize) / 2, |_| {
                c.pal.c
            });
        }
    }
}

pub(super) fn act_vertical(g: &mut Grid, c: &Ctx) {
    let into = c.into();
    // The title's words as columns of block letters, falling in.
    let words: Vec<&str> = c.title.split_whitespace().collect();
    let n = words.len().max(1);
    let colw = c.cols / n;
    for (k, w) in words.iter().enumerate() {
        let letters: Vec<char> = w.chars().collect();
        // At least a block letter's height (smaller fell back to
        // plain text, which drew nothing); a long word runs off.
        let per = (c.rows / letters.len().max(1)).max(7);
        let drop = ((1.0
            - c.eased(((c.age as f32) - k as f32 * 0.5).clamp(0.0, 2.0) / 2.0))
            * c.rows as f32) as isize;
        // Settled, the columns bob in turn, one a beat.
        let turn = (c.beat.max(0.0).floor() as usize) % n == k;
        let drop = drop
            + if turn {
                ((1.0 - c.eased(into)) * (2.0 + 3.0 * c.bass)) as isize
            } else {
                0
            };
        for (i, ch) in letters.iter().enumerate() {
            let b = art(c, &ch.to_string(), colw.saturating_sub(2), per);
            let x0 = (k * colw + (colw.saturating_sub(b.w)) / 2) as isize;
            let y0 = (i * per) as isize - drop;
            for y in 0..b.h {
                for x in 0..b.w {
                    if b.on[y * b.w + x] {
                        put(
                            g,
                            x0 + x as isize,
                            y0 + y as isize,
                            b.shape[y * b.w + x],
                            c.pal.at(i as f32 / letters.len().max(1) as f32),
                        );
                    }
                }
            }
        }
    }
}

pub(super) fn act_scatter(g: &mut Grid, c: &Ctx) {
    // A word a beat at its own place and size; the last few stay,
    // fading, so the screen fills with them.
    let mut words: Vec<String> = c
        .title
        .split_whitespace()
        .map(|w| w.to_uppercase())
        .collect();
    let from_title = words.len();
    words.extend(c.artist.split_whitespace().map(|w| w.to_uppercase()));
    if !words.is_empty() {
        let now = c.beat.floor() as i64;
        // The word now and the one before (faint), in different
        // quarters of the screen: never on top of each other.
        for back in (0..2).rev() {
            let k = now - back;
            let w = &words[(k.rem_euclid(words.len() as i64)) as usize];
            let h = hash(k as u64 ^ c.seed);
            let size = ((c.rows as f32 / 2.0) * [0.55, 0.7, 0.85][(h * 3.0) as usize % 3])
                .round() as usize;
            // The artist's words a size down from the title's.
            let idx = k.rem_euclid(words.len() as i64) as usize;
            let size = if idx >= from_title {
                small_rows(c).max(4).min(size)
            } else {
                size
            };
            let b = art(c, w, c.cols / 2, size);
            let q = k.rem_euclid(4) as usize;
            let (qw, qh) = (c.cols / 2, c.rows / 2);
            let x0 = ((q % 2) * qw
                + (hash(k as u64 + 1) * qw.saturating_sub(b.w) as f32) as usize)
                as isize;
            let y0 = ((q / 2) * qh
                + (hash(k as u64 + 2) * qh.saturating_sub(b.h) as f32) as usize)
                as isize;
            let fade = if back == 0 { 1.0 } else { 0.3 };
            for y in 0..b.h {
                for x in 0..b.w {
                    if b.on[y * b.w + x] {
                        put(
                            g,
                            x0 + x as isize,
                            y0 + y as isize,
                            if back == 0 {
                                b.shape[y * b.w + x]
                            } else {
                                '▒'
                            },
                            scale(c.pal.at(h), fade),
                        );
                    }
                }
            }
        }
    }
}

pub(super) fn act_ticker(g: &mut Grid, c: &Ctx) {
    // Five lines, each at its own speed, the middle one big.
    let line = format!(
        "{}   {}   ",
        c.title.to_uppercase(),
        c.artist.to_uppercase()
    );
    for k in 0..5 {
        let y = (c.rows as f32 * (k as f32 + 1.0) / 6.0) as isize;
        if k == 2 {
            let art = crate::banner::huge(&line, usize::MAX / 4, (c.rows / 4).max(7));
            let b = bits(&art);
            let off = (c.beat as f32 * 3.0) as usize;
            for yy in 0..b.h {
                for x in 0..c.cols {
                    let sx = (x + off) % b.w.max(1);
                    if b.on[yy * b.w + sx] {
                        put(
                            g,
                            x as isize,
                            y - b.h as isize / 2 + yy as isize,
                            b.shape[yy * b.w + sx],
                            c.pal.at(sx as f32 / b.w as f32),
                        );
                    }
                }
            }
            continue;
        }
        let speed = [1.0, 2.5, 0.0, -1.5, -3.0][k];
        let off = (c.beat * speed * 2.0) as isize;
        let h = line_rows(c);
        scroll_line(g, c, &line, y - h as isize / 2, h, off, |_| {
            scale(c.pal.b, 0.75)
        });
    }
}

pub(super) fn act_letter(g: &mut Grid, c: &Ctx) {
    let into = c.into();
    // One letter a beat, as big as the screen holds, in turn.
    let letters: Vec<char> = c
        .title
        .to_uppercase()
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect();
    if !letters.is_empty() {
        let k = c.beat.max(0.0).floor() as usize % letters.len();
        let grow = 0.8 + 0.2 * c.eased((into * 2.0).min(1.0));
        let b = art(
            c,
            &letters[k].to_string(),
            c.cols.saturating_sub(2),
            ((c.rows as f32 - 2.0) * grow) as usize,
        );
        let (x0, y0) = (
            (c.cols as isize - b.w as isize) / 2,
            (c.rows as isize - b.h as isize) / 2,
        );
        let col = [c.pal.a, c.pal.b, c.pal.c][k % 3];
        for y in 0..b.h {
            for x in 0..b.w {
                if b.on[y * b.w + x] {
                    put(
                        g,
                        x0 + x as isize,
                        y0 + y as isize,
                        b.shape[y * b.w + x],
                        col,
                    );
                }
            }
        }
    }
}
