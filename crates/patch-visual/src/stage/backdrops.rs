//! The backdrops, one function each: what fills the stage behind a title.

use super::*;

/// Broad spectral ribbons: a slow atmosphere with depth, rather
/// than a static quiet scene. Coordinates cover any stage aspect.
pub(super) fn backdrop_aurora(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let t = c.t as f32;
    for y in 0..rows {
        for x in 0..cols {
            let u = x as f32 / cols.max(1) as f32;
            let v = y as f32 / rows.max(1) as f32;
            let mut light = 0.0;
            for n in 0..3 {
                let n = n as f32;
                let center =
                    0.25 + n * 0.23 + (u * (4.0 + n) + t * 0.22 + n * 2.1).sin() * 0.12;
                let width = 0.035 + band(c, n * 0.25) * 0.075;
                let distance = (v - center) / width;
                light += (-distance * distance).exp()
                    * (0.45 + 0.2 * (u * 12.0 - t * 0.3).sin());
            }
            lit(
                g,
                c,
                x,
                y,
                light,
                (u * 0.6 + v * 0.4 + t * 0.02).rem_euclid(1.0),
            );
        }
    }
}

/// Twisted depth planes open and close over an eight-bar movement.
pub(super) fn backdrop_vortex(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let kick = c.kick();
    let t = c.t as f32;
    let movement = (c.age as f32 / 32.0 * std::f32::consts::TAU).sin();
    for y in 0..rows {
        for x in 0..cols {
            let (dx, dy, r) = polar(c, x, y);
            let angle = dy.atan2(dx);
            let depth = 1.0 / (r + 0.18);
            let twist = angle * 3.0 + depth * (2.2 + movement * 0.6) - t * 1.2;
            let ribs = twist.cos().abs().powi(12);
            let planes = (depth * 4.0 - t * 1.4).sin().abs().powi(16);
            let light =
                (ribs * 0.6 + planes * 0.45) * r.min(0.35) * 2.8 * (0.65 + 0.35 * kick);
            lit(
                g,
                c,
                x,
                y,
                light,
                (angle / std::f32::consts::TAU + r * 0.8 + t * 0.035).rem_euclid(1.0),
            );
        }
    }
}

/// Rotating faceted interference, unfolded by bass and closed by
/// the phrase. No additional per-frame allocations or GPU shader.
pub(super) fn backdrop_prism(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let kick = c.kick();
    let t = c.t as f32;
    let turn = t * 0.16;
    let (sn, cs) = turn.sin_cos();
    let phrase = (c.age as f32 / 16.0 * std::f32::consts::PI).sin();
    for y in 0..rows {
        for x in 0..cols {
            let (dx, dy, r) = polar(c, x, y);
            let scale = (cols as f32 * 0.5).max(1.0);
            let u = (dx * cs - dy * sn) / scale;
            let v = (dx * sn + dy * cs) / scale;
            let facets = (u.abs() + v.abs()) * (7.0 + phrase * 2.0 + c.bass * 2.0);
            let edge = (facets - t * 0.6).sin().abs().powi(18);
            let cross = ((u - v) * 9.0 + t * 0.5).sin().abs().powi(20);
            lit(
                g,
                c,
                x,
                y,
                edge * 0.65 + cross * 0.25 + kick * edge * 0.2,
                (r * 0.8 + facets * 0.08 + t * 0.025).rem_euclid(1.0),
            );
        }
    }
}

pub(super) fn backdrop_rings(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let into = c.into();
    for y in 0..rows {
        for x in 0..cols {
            let (_, _, r) = polar(c, x, y);
            let mut i = 0.0f32;
            for (age, k) in [(into, 1.0), (into + 1.0, 0.45)] {
                let d = (r - age * 0.9) / (0.03 + 0.05 * age);
                i += (-d * d).exp() * (1.0 - age / 2.0).max(0.0) * k * (0.6 + 0.6 * c.bass);
            }
            lit(g, c, x, y, i, 1.0 - r);
        }
    }
}

pub(super) fn backdrop_tunnel(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let kick = c.kick();
    let beat = c.beat as f32;
    let t = c.t as f32;
    for y in 0..rows {
        for x in 0..cols {
            let (dx, dy, r) = polar(c, x, y);
            let a = dy.atan2(dx) / std::f32::consts::PI;
            let u = 0.35 / r.max(0.02) + beat;
            let v = a * 6.0 + t * 0.25;
            let check = ((u.floor() as i32 + v.floor() as i32) & 1) as f32;
            let i = (0.25 + 0.55 * check) * r.min(1.0).powf(0.7) * (0.6 + 0.4 * kick);
            lit(g, c, x, y, i, check * 0.5 + r * 0.5);
        }
    }
}

pub(super) fn backdrop_warp(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let kick = c.kick();
    let e = c.energy;
    let beat = c.beat as f32;
    let t = c.t as f32;
    let travel = t * (0.25 + 0.9 * e) + beat * 0.15;
    for y in 0..rows {
        for x in 0..cols {
            let (dx, dy, r) = polar(c, x, y);
            let ray = ((dy.atan2(dx) / std::f32::consts::TAU + 0.5) * 180.0) as u64;
            let at = (hash(ray) + travel * (0.5 + hash(ray + 7777))).fract();
            let len = 0.02 + 0.08 * e + 0.05 * kick;
            let d = at - r;
            if (0.0..len).contains(&d) && hash(ray + 31) > 0.35 {
                lit(g, c, x, y, (1.0 - d / len) * r.sqrt(), r);
            }
        }
    }
}

pub(super) fn backdrop_skyline(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let kick = c.kick();
    let n = c.history.len().max(1);
    let cx = cols as f32 / 2.0;
    for x in 0..cols {
        let off = ((x as f32 - cx).abs() / cx.max(1.0) * (n - 1) as f32) as usize;
        let level = c
            .history
            .get(n - 1 - off.min(n - 1))
            .copied()
            .unwrap_or(0.0);
        let h = (level.powf(1.5) * rows as f32 * 0.55 * (0.8 + 0.3 * kick)) as usize;
        for k in 0..h.min(rows) {
            lit(
                g,
                c,
                x,
                rows - 1 - k,
                0.45 + 0.4 * (k as f32 / rows as f32),
                k as f32 / rows as f32 * 1.6,
            );
        }
    }
}

pub(super) fn backdrop_plasma(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let kick = c.kick();
    let t = c.t as f32;
    for y in 0..rows {
        for x in 0..cols {
            let (u, v) = (x as f32 / cols as f32 * 6.0, y as f32 / rows as f32 * 3.0);
            let tt = t * 0.35;
            let p = (u + tt).sin()
                + (v * 1.3 - tt * 0.7).sin()
                + ((u + v) * 0.8 + tt * 0.5).sin()
                + ((u * u + v * v).sqrt() * 1.5 - tt).sin();
            let i = ((p / 4.0) * 0.5 + 0.5).powi(2) * (0.7 + 0.3 * kick);
            lit(g, c, x, y, i, p * 0.25 + 0.5);
        }
    }
}

pub(super) fn backdrop_spectrum(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let kick = c.kick();
    // The spectrum mirrored from the middle, up and down.
    let cx = cols as f32 / 2.0;
    for x in 0..cols {
        let f = ((x as f32 + 0.5 - cx).abs() / cx).min(1.0);
        let v = band(c, f);
        let h = (v * rows as f32 * 0.5 * (0.85 + 0.3 * kick)) as usize;
        for k in 0..h.min(rows / 2) {
            let i = 0.35 + 0.5 * k as f32 / (rows as f32 / 2.0);
            lit(g, c, x, rows / 2 + k, i, f);
            lit(g, c, x, rows / 2 - 1 - k.min(rows / 2 - 1), i, f);
        }
    }
}

pub(super) fn backdrop_horizon(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let kick = c.kick();
    let beat = c.beat as f32;
    // A floor grid rushing in under a sun that pulses.
    let hz = rows as f32 * 0.55;
    for y in 0..rows {
        for x in 0..cols {
            let (dx, _, _) = polar(c, x, y);
            let yf = y as f32 + 0.5;
            if yf > hz {
                let depth = (yf - hz) / (rows as f32 - hz);
                let z = 1.0 / depth.max(0.02);
                let line =
                    ((z * 0.6 - beat * 1.0).fract() < 0.12 * depth.max(0.3)) as i32 as f32;
                let col = (((dx * depth * 0.18).fract()).abs() < 0.1 * depth.max(0.3))
                    as i32 as f32;
                lit(
                    g,
                    c,
                    x,
                    y,
                    (line.max(col)) * (0.3 + 0.6 * depth) * (0.7 + 0.3 * kick),
                    0.2 + depth * 0.6,
                );
            } else {
                let (sx, sy) = (
                    dx / (cols as f32 * 0.16),
                    (yf - hz * 0.62) / (rows as f32 * 0.2),
                );
                let d = (sx * sx + sy * sy).sqrt();
                if d < 1.0 + 0.15 * kick && !(sy > 0.0 && ((sy * 6.0).fract() < 0.3)) {
                    lit(g, c, x, y, 0.55 + 0.35 * (1.0 - d), 0.9 - sy * 0.3);
                }
            }
        }
    }
}

pub(super) fn backdrop_lasers(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let kick = c.kick();
    let t = c.t as f32;
    // Beams from the bottom centre, sweeping; they flash on the kick.
    let n = 5 + (c.seed % 4) as usize;
    let (cx, by) = (cols as f32 / 2.0, rows as f32);
    for y in 0..rows {
        for x in 0..cols {
            let (dx, dy) = (x as f32 + 0.5 - cx, (by - y as f32 - 0.5) * 2.0);
            let a = dx.atan2(dy);
            let mut i = 0.0f32;
            for k in 0..n {
                let base = (k as f32 / (n - 1) as f32 - 0.5) * 1.8;
                let sweep = (t * 0.7 + k as f32).sin() * 0.35;
                let d = (a - base - sweep).abs();
                i = i.max((1.0 - d / 0.025).max(0.0));
            }
            lit(
                g,
                c,
                x,
                y,
                i * (0.45 + 0.55 * kick),
                (dy / (by * 2.0)).min(1.0),
            );
        }
    }
}

pub(super) fn backdrop_checker(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let kick = c.kick();
    let beat = c.beat as f32;
    let size = 2.0 + 6.0 * (1.0 - kick * 0.5);
    for y in 0..rows {
        for x in 0..cols {
            let (dx, dy, r) = polar(c, x, y);
            let (u, v) = (
                (dx / size + beat * 0.5).floor() as i32,
                (dy / size).floor() as i32,
            );
            if (u + v) & 1 == 0 {
                lit(g, c, x, y, 0.3 + 0.3 * kick * (1.0 - r), r);
            }
        }
    }
}

pub(super) fn backdrop_spiral(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let kick = c.kick();
    let beat = c.beat as f32;
    let t = c.t as f32;
    let arms = 2.0 + (c.seed % 3) as f32;
    for y in 0..rows {
        for x in 0..cols {
            let (dx, dy, r) = polar(c, x, y);
            let a = dy.atan2(dx);
            let s = (a * arms + r * 14.0 - t * 2.0 - beat * 0.5).sin();
            lit(
                g,
                c,
                x,
                y,
                ((s + 1.0) * 0.5).powi(3) * (0.4 + 0.6 * r) * (0.7 + 0.3 * kick),
                r,
            );
        }
    }
}

pub(super) fn backdrop_glyphfall(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let e = c.energy;
    let t = c.t as f32;
    // Columns of falling glyphs, speed with the energy.
    let glyphs: Vec<char> = "01<>/\\|=+*#%&$@".chars().collect();
    for x in 0..cols {
        let speed = 4.0 + 10.0 * hash(x as u64) + 12.0 * e;
        let head =
            (t * speed + hash(x as u64 + 99) * rows as f32 * 3.0) % (rows as f32 * 1.6);
        for y in 0..rows {
            let d = head - y as f32;
            if (0.0..rows as f32 * 0.4).contains(&d) {
                let i = (1.0 - d / (rows as f32 * 0.4)).powi(2) * 0.75;
                if i > 0.05 {
                    let ch = glyphs[(hash(x as u64 * 131 + y as u64 + (t * 8.0) as u64)
                        * glyphs.len() as f32)
                        as usize
                        % glyphs.len()];
                    let col = scale(c.pal.at(i), 0.3 + 0.6 * i);
                    g.cells[y * cols + x] = Cell {
                        ch,
                        fg: Some(col),
                        bg: None,
                    };
                }
            }
        }
    }
}

pub(super) fn backdrop_ripple(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let kick = c.kick();
    let beat = c.beat as f32;
    let t = c.t as f32;
    // Two drops rippling, a new ring each beat.
    let src = [
        (0.3 + 0.1 * (t * 0.3).sin(), 0.4),
        (0.7, 0.6 + 0.1 * (t * 0.4).cos()),
    ];
    for y in 0..rows {
        for x in 0..cols {
            let mut s = 0.0;
            for (sx, sy) in src {
                let (dx, dy) = (
                    (x as f32 / cols as f32 - sx) * cols as f32,
                    (y as f32 / rows as f32 - sy) * rows as f32 * 2.0,
                );
                let d = (dx * dx + dy * dy).sqrt();
                s += (d * 0.35 - beat * std::f32::consts::TAU * 0.5).sin()
                    / (1.0 + d * 0.05);
            }
            lit(
                g,
                c,
                x,
                y,
                (s * 0.35 + 0.35).max(0.0) * (0.7 + 0.3 * kick),
                s * 0.25 + 0.5,
            );
        }
    }
}

pub(super) fn backdrop_chase(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    // Bars lighting in turn, one per 16th, across the width.
    let n = 16;
    let at = c.sixteenth().rem_euclid(n as i64) as usize;
    for x in 0..cols {
        let k = x * n / cols.max(1);
        let age = (at + n - k) % n;
        let i = if age == 0 {
            0.85
        } else {
            0.5 * (1.0 - age as f32 / 5.0).max(0.0)
        };
        if i > 0.0 && x % (cols / n).max(1) != 0 {
            for y in 0..rows {
                if (y as f32 / rows as f32 - 0.5).abs()
                    < 0.18 + 0.3 * band(c, k as f32 / n as f32)
                {
                    lit(g, c, x, y, i, k as f32 / n as f32);
                }
            }
        }
    }
}

pub(super) fn backdrop_fire(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let kick = c.kick();
    let t = c.t as f32;
    // Heat rising from the bottom, as high as the bass.
    for y in 0..rows {
        for x in 0..cols {
            let up = 1.0 - y as f32 / rows as f32;
            let n = ((x as f32 * 0.35 + t * 1.3).sin()
                + (x as f32 * 0.13 - t * 2.1 + up * 5.0).sin()
                + (up * 9.0 - t * 4.0).sin())
                / 3.0;
            let flame = (1.0 - up / (0.3 + 0.55 * c.bass + 0.15 * kick)) + n * 0.35;
            lit(g, c, x, y, flame.clamp(0.0, 1.0) * 0.9, 1.0 - flame);
        }
    }
}

pub(super) fn backdrop_burst(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let beat = c.beat as f32;
    // Spokes flashing on every 16th, turning with the bar.
    let spokes = 12.0 + (c.seed % 3) as f32 * 6.0;
    let s16 = (c.beat * 4.0).fract() as f32;
    let flash = (1.0 - s16).powi(2) * (0.4 + 0.6 * c.high);
    for y in 0..rows {
        for x in 0..cols {
            let (dx, dy, r) = polar(c, x, y);
            let a = (dy.atan2(dx) / std::f32::consts::TAU + (beat / 16.0)).fract();
            let d = ((a * spokes).fract() - 0.5).abs();
            if d < 0.1 {
                lit(g, c, x, y, (0.3 + 0.6 * flash) * r.sqrt(), r);
            }
        }
    }
}

pub(super) fn backdrop_scope(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let kick = c.kick();
    let t = c.t as f32;
    // A Lissajous figure tracing itself, its ratio turning with the bar.
    let (cx, cy) = (cols as f32 / 2.0, rows as f32 / 2.0);
    let (fa, fb) = (3.0 + (c.seed % 3) as f32, 2.0 + ((c.seed / 3) % 3) as f32);
    let steps = 1600;
    for k in 0..steps {
        let p = k as f32 / steps as f32 * std::f32::consts::TAU;
        let x = cx + (fa * p + t * 0.4).sin() * cx * (0.7 + 0.2 * kick);
        let y = cy + (fb * p).sin() * cy * (0.7 + 0.2 * c.bass);
        lit(
            g,
            c,
            x as usize,
            y as usize,
            0.55 + 0.3 * kick,
            k as f32 / steps as f32,
        );
    }
}

pub(super) fn backdrop_dots(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let into = c.into();
    // A grid of dots breathing with the kick from the centre out.
    for y in (0..rows).step_by(2) {
        for x in (0..cols).step_by(4) {
            let (_, _, r) = polar(c, x, y);
            let wave = (1.0 - ((r - into * 0.8).abs() * 5.0)).max(0.0);
            lit(g, c, x, y, 0.2 + 0.6 * wave * (0.5 + c.bass), r);
        }
    }
}

pub(super) fn backdrop_stars(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let t = c.t as f32;
    for y in 0..rows {
        for x in 0..cols {
            let h = hash((y * cols + x) as u64);
            if h > 0.985 {
                let tw = ((t * (1.0 + h * 3.0) + h * 40.0).sin() * 0.5 + 0.5)
                    * (0.5 + 0.5 * c.high);
                lit(g, c, x, y, 0.3 + 0.6 * tw, h);
            }
        }
    }
}

pub(super) fn backdrop_lines(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let kick = c.kick();
    let beat = c.beat as f32;
    // Horizontal sine lines, their amplitude the bass, their speed the tempo.
    let n = 5;
    for k in 0..n {
        for x in 0..cols {
            let ph =
                x as f32 / cols as f32 * std::f32::consts::TAU * (1.0 + k as f32 * 0.5);
            let amp = rows as f32 * (0.08 + 0.2 * c.bass) * (0.6 + 0.4 * kick);
            let y = rows as f32 * (k as f32 + 1.0) / (n + 1) as f32
                + (ph + beat * std::f32::consts::PI * 0.5 + k as f32).sin() * amp;
            if y >= 0.0 {
                lit(
                    g,
                    c,
                    x,
                    y as usize,
                    0.5 + 0.3 * kick,
                    k as f32 / n as f32,
                );
            }
        }
    }
}

pub(super) fn backdrop_grid(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let kick = c.kick();
    let beat = c.beat as f32;
    // A wireframe grid bent by the bass, its lines moving with the beat.
    for y in 0..rows {
        for x in 0..cols {
            let (dx, dy, r) = polar(c, x, y);
            let bend = 1.0 + c.bass * 0.8 * (1.0 - r) + kick * 0.3;
            let (u, v) = (dx * bend / 6.0 + beat * 0.25, dy * bend / 6.0);
            let line = (u.fract().abs() < 0.12) || (v.rem_euclid(1.0) < 0.12);
            if line {
                lit(g, c, x, y, 0.35 + 0.35 * (1.0 - r) + 0.2 * kick, r);
            }
        }
    }
}

pub(super) fn backdrop_orbits(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let kick = c.kick();
    let e = c.energy;
    let t = c.t as f32;
    // Sparks orbiting the centre, faster with the energy, each a
    // short trail; rings of them at different radii.
    let n = 60;
    for k in 0..n {
        let h = hash(k as u64 ^ c.seed);
        let radius = 0.15 + 0.8 * h;
        let speed = (0.3 + 1.2 * hash(k as u64 + 5))
            * (0.5 + e)
            * if k % 2 == 0 { 1.0 } else { -1.0 };
        for tail in 0..8 {
            let a = t * speed - tail as f32 * 0.04 * speed.signum() + h * 40.0;
            let rr = radius * (1.0 + 0.1 * kick);
            let (x, y) = (
                cols as f32 / 2.0 + a.cos() * rr * cols as f32 / 2.0,
                rows as f32 / 2.0 + a.sin() * rr * rows as f32 / 2.0,
            );
            if x >= 0.0 && y >= 0.0 {
                lit(
                    g,
                    c,
                    x as usize,
                    y as usize,
                    (0.8 - tail as f32 * 0.09).max(0.0),
                    h,
                );
            }
        }
    }
}

pub(super) fn backdrop_kaleido(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let kick = c.kick();
    let t = c.t as f32;
    // Interference mirrored in four: a kaleidoscope turning slowly.
    let tt = t * 0.3;
    for y in 0..rows {
        for x in 0..cols {
            let (dx, dy, r) = polar(c, x, y);
            let (u, v) = (dx.abs() / cols as f32 * 8.0, dy.abs() / rows as f32 * 4.0);
            let a = (v.atan2(u) * 4.0 + tt).sin();
            let p = (u * 1.3 + tt).sin() * (v * 1.7 - tt * 0.8).cos() + a * 0.6;
            let i = ((p * 0.5 + 0.5).powi(2) * (0.6 + 0.4 * kick)) * (1.0 - r * 0.4);
            lit(g, c, x, y, i, (p * 0.3 + 0.5 + r * 0.2).fract());
        }
    }
}

pub(super) fn backdrop_radial(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let kick = c.kick();
    // The spectrum as bars around a ring, pulsing out on the kick.
    let n = 48;
    let (cx, cy) = (cols as f32 / 2.0, rows as f32 / 2.0);
    let base = 0.22 + 0.04 * kick;
    for k in 0..n {
        let a = k as f32 / n as f32 * std::f32::consts::TAU;
        let f = (k as f32 / n as f32 * 2.0 - 1.0).abs();
        let len = base + band(c, f) * 0.45;
        let steps = 40;
        for st in 0..steps {
            let r = base + (len - base) * st as f32 / steps as f32;
            let (x, y) = (cx + a.cos() * r * cx, cy + a.sin() * r * cy);
            if x >= 0.0 && y >= 0.0 {
                lit(
                    g,
                    c,
                    x as usize,
                    y as usize,
                    0.4 + 0.5 * st as f32 / steps as f32,
                    f,
                );
            }
        }
    }
}

pub(super) fn backdrop_rainfall(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let e = c.energy;
    let t = c.t as f32;
    // Slanted rain streaks, heavier with the energy.
    for x in 0..cols {
        for k in 0..3 {
            let seed = (x * 3 + k) as u64;
            if hash(seed + 11) > 0.35 + 0.4 * (1.0 - e) {
                continue;
            }
            let speed = 10.0 + 14.0 * hash(seed);
            let len = 3.0 + 5.0 * hash(seed + 3);
            let head = (t * speed + hash(seed + 7) * rows as f32 * 4.0)
                % (rows as f32 + len * 2.0)
                - len;
            for d in 0..len as usize {
                let y = head - d as f32;
                let xx = x as f32 + (y * 0.35);
                if y >= 0.0 && xx >= 0.0 {
                    lit(
                        g,
                        c,
                        xx as usize % cols,
                        y as usize,
                        0.5 * (1.0 - d as f32 / len),
                        0.3 + 0.4 * hash(seed),
                    );
                }
            }
        }
    }
}

pub(super) fn backdrop_wavefront(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let t = c.t as f32;
    // The level over time as a waveform crossing the screen,
    // mirrored, the newest at the right.
    let n = c.history.len().max(2);
    let mid = rows as f32 / 2.0;
    for x in 0..cols {
        let i = (x * (n - 1) / cols.max(1)).min(n - 1);
        let v = c.history.get(i).copied().unwrap_or(0.0);
        let amp = (v * mid * 0.9).max(0.5);
        let wob = ((x as f32 * 0.3 + t * 3.0).sin() * 0.5 + 0.5) * amp;
        for y in [mid - wob, mid + wob] {
            if y >= 0.0 {
                lit(
                    g,
                    c,
                    x,
                    y as usize,
                    0.5 + 0.4 * (i as f32 / n as f32),
                    x as f32 / cols as f32,
                );
            }
        }
    }
}

pub(super) fn backdrop_hexes(g: &mut Grid, c: &Ctx) {
    let cols = c.cols;
    let rows = c.rows;
    let into = c.into();
    // A honeycomb whose cells light in a wave from the centre, a
    // ring per beat.
    let size = 5.0f32;
    for y in 0..rows {
        for x in 0..cols {
            let (fx, fy) = (x as f32 / size, y as f32 * 2.0 / size);
            let row = fy.floor();
            let shift = if row as i32 % 2 == 0 { 0.0 } else { 0.5 };
            let (cx_, cy_) = ((fx + shift).floor() - shift + 0.5, row + 0.5);
            let (ux, uy) = (fx - cx_, fy - cy_);
            let edge = ux.abs() > 0.4 || uy.abs() > 0.42;
            let (_, _, r) = polar(c, x, y);
            let wave =
                (1.0 - ((r * 3.0 - into * 3.0).rem_euclid(3.0) - 0.0).min(1.0)).max(0.0);
            if edge {
                lit(g, c, x, y, 0.3 + 0.5 * wave, r);
            }
        }
    }
}
