// Adapted from avillagran/omarchy-ttfx-layer, MIT, c969b6c172108c5f6ab937d99db67c17ddd28196.
// See LICENSE and README.md in this directory.
use super::{Sound, raster::Raster};
use std::f32::consts::{PI, TAU};
type Color = (u8, u8, u8);
fn scale(c: Color, k: f32) -> Color {
    (
        (c.0 as f32 * k).clamp(0., 255.) as u8,
        (c.1 as f32 * k).clamp(0., 255.) as u8,
        (c.2 as f32 * k).clamp(0., 255.) as u8,
    )
}
fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0., 1.);
    (
        (a.0 as f32 + (b.0 as f32 - a.0 as f32) * t) as u8,
        (a.1 as f32 + (b.1 as f32 - a.1 as f32) * t) as u8,
        (a.2 as f32 + (b.2 as f32 - a.2 as f32) * t) as u8,
    )
}
struct Rng(u32);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
        (self.0 >> 8) as f32 / 16777216.
    }
}
fn away(cursor: Option<(f32, f32)>, x: f32, y: f32, radius: f32) -> (f32, f32, f32) {
    let Some((cx, cy)) = cursor else {
        return (0., 0., 0.);
    };
    let (dx, dy) = (x - cx, y - cy);
    let d = (dx * dx + dy * dy).sqrt();
    if d >= radius {
        return (0., 0., 0.);
    }
    let k = 1. - d / radius;
    (dx / d.max(0.001), dy / d.max(0.001), k * k)
}
struct Jelly {
    x: f32,
    y: f32,
    size: f32,
    phase: f32,
    hue: f32,
    drift: f32,
    /// How startled it is by the cursor, 0 to 1.
    fright: f32,
    /// Sideways drag on the tentacles while it flees.
    trail: f32,
}
struct Plankton {
    x: f32,
    y: f32,
    home: (f32, f32),
}
pub(super) struct Jellyfish {
    jellies: Vec<Jelly>,
    plankton: Vec<Plankton>,
    time: f32,
    beat_push: f32,
    rng: Rng,
}
impl Jellyfish {
    pub(super) fn new(seed: u32) -> Self {
        let mut rng = Rng(seed);
        let jellies = (0..7)
            .map(|i| Jelly {
                x: rng.next(),
                y: rng.next(),
                size: 0.55 + rng.next() * 0.8,
                phase: rng.next() * TAU,
                hue: i as f32 / 7.,
                drift: rng.next() - 0.5,
                fright: 0.,
                trail: 0.,
            })
            .collect();
        let plankton = (0..140)
            .map(|_| {
                let (x, y) = (rng.next(), rng.next());
                Plankton { x, y, home: (x, y) }
            })
            .collect();
        Self {
            jellies,
            plankton,
            time: 0.,
            beat_push: 0.,
            rng,
        }
    }
    pub(super) fn step(&mut self, dt: f32, sound: &Sound, cursor: Option<(f32, f32)>) {
        self.time += dt;
        self.beat_push *= (-dt / 0.22).exp();
        if sound.beat {self.beat_push=1.0;}
        let cursor = cursor.map(|(x, y)| ((x + 1.) * 0.5, (y + 1.) * 0.5));
        for j in &mut self.jellies {
            // Near the cursor it pulses hard, lights up and swims away.
            let (ax, ay, k) = away(cursor, j.x, j.y, 0.32);
            j.x += ax * k * dt * 0.3;
            j.y += ay * k * dt * 0.3;
            j.fright += ((k * 3.).min(1.) - j.fright) * (1. - (-dt * 5.).exp());
            j.trail += (-ax * k * 2.5 - j.trail) * (1. - (-dt * 3.).exp());
            // Bell contraction on the bass drives a push upward.
            j.phase += dt * (0.9 + sound.bass * 1.5 + j.fright * 7.);
            let push = (j.phase.sin().max(0.)).powi(2);
            j.y -= dt * (0.012 + push * 0.05 + self.beat_push * 0.08) / j.size;
            j.x += dt * j.drift * 0.02 + (self.time * 0.3 + j.phase).sin() * dt * 0.01;
            if j.y < -0.25 {
                j.y = 1.2;
                j.x = self.rng.next();
            }
            j.x = j.x.rem_euclid(1.);
        }
        let (tx, ty) = cursor.unwrap_or((-9., -9.));
        for (i, p) in self.plankton.iter_mut().enumerate() {
            let k = i as f32;
            // Each mote wanders around its own home and only leans toward the
            // cursor when it is close, so the cloud never collapses or drifts.
            let (dx, dy) = (tx - p.x, ty - p.y);
            let near = (1. - (dx * dx + dy * dy).sqrt() / 0.22).max(0.);
            let wander_x = (self.time * 0.4 + k).sin() * 0.012;
            let wander_y = (self.time * 0.3 + k * 1.3).cos() * 0.012 - 0.003;
            p.x += (wander_x + (p.home.0 - p.x) * 0.15 + dx * near * 0.6) * dt;
            p.y += (wander_y + (p.home.1 - p.y) * 0.15 + dy * near * 0.6) * dt;
            p.home.1 = (p.home.1 - dt * 0.004).rem_euclid(1.);
            p.x = p.x.rem_euclid(1.);
            p.y = p.y.rem_euclid(1.);
        }
    }
    pub(super) fn draw(
        &self,
        r: &mut Raster,
        cols: usize,
        rows: usize,
        aspect: f32,
        sound: &Sound,
    ) {
        // Caustic light bands on the sea floor, slow and soft.
        for x in 0..cols {
            for band in 0..3 {
                let yy = rows as f32 * (0.82 + band as f32 * 0.06)
                    + (x as f32 * 0.09 + self.time * (0.5 + band as f32 * 0.2) + band as f32).sin()
                        * 1.6
                    + (x as f32 * 0.21 - self.time * 0.8).sin() * 0.8;
                let g = 0.25
                    + 0.25 * (x as f32 * 0.17 + self.time * 1.1).sin().abs()
                    + sound.treble * 0.3;
                r.dot(x as i32, yy as i32, 40., '~', scale((60, 150, 190), g));
            }
        }
        for (i, p) in self.plankton.iter().enumerate() {
            let t = 0.4 + 0.6 * (self.time * 2. + i as f32).sin().abs();
            r.dot(
                (p.x * cols as f32) as i32,
                (p.y * rows as f32) as i32,
                30.,
                if t > 0.9 { '°' } else { '·' },
                scale((150, 220, 230), 0.3 + t * 0.4 + sound.treble * 0.3),
            );
        }
        let mut order: Vec<&Jelly> = self.jellies.iter().collect();
        order.sort_by(|a, b| a.size.partial_cmp(&b.size).unwrap());
        for j in order {
            let cx = j.x * cols as f32;
            let cy = j.y * rows as f32;
            let contract = 1. - (j.phase.sin().max(0.) * 0.6 + self.beat_push * (0.8+j.hue*0.2)).min(1.0) * 0.25;
            let rw = rows as f32 * 0.09 * j.size * aspect * contract;
            let rh = rows as f32 * 0.07 * j.size / contract.sqrt();
            let tint = mix(
                (190, 120, 255),
                (120, 230, 255),
                (j.hue * 2.).rem_euclid(1.),
            );
            let tint = mix(tint, (255, 180, 220), j.hue * 0.5);
            let depth = 10. - j.size;
            let bright = 0.45 + j.size * 0.35 + sound.bass * 0.35 + j.fright * 0.6;
            // Bell: an upper half ellipse of rim glyphs with a lighter inner glow.
            for i in 0..=48 {
                let a = PI + i as f32 / 48. * PI;
                let (x, y) = (cx + a.cos() * rw, cy + a.sin() * rh);
                let ch = if a.cos().abs() > 0.85 {
                    '('
                } else if a.sin() < -0.9 {
                    '~'
                } else {
                    '^'
                };
                let ch = if a.cos() > 0.85 { ')' } else { ch };
                r.dot(
                    x.round() as i32,
                    y.round() as i32,
                    depth,
                    ch,
                    scale(tint, bright),
                );
                if i % 4 == 0 {
                    let (ix, iy) = (cx + a.cos() * rw * 0.55, cy + a.sin() * rh * 0.55);
                    r.dot(
                        ix.round() as i32,
                        iy.round() as i32,
                        depth + 0.1,
                        '·',
                        scale(tint, bright * 0.5),
                    );
                }
            }
            for i in 0..=(rw * 2.) as i32 {
                let x = cx - rw + i as f32;
                r.dot(
                    x.round() as i32,
                    cy.round() as i32,
                    depth,
                    '~',
                    scale(tint, bright * 0.8),
                );
            }
            // Tentacles: sine ribbons trailing below, lagging behind the pulse.
            let count = 4 + (j.size * 3.) as usize;
            for t in 0..count {
                let ox = -rw * 0.8 + rw * 1.6 * t as f32 / (count - 1).max(1) as f32;
                let len = rh * (3. + (t % 3) as f32) * (0.8 + j.size * 0.3);
                let steps = len as i32;
                for s in 0..steps {
                    let f = s as f32 / len;
                    let sway =
                        (self.time * 1.4 - f * 5. + j.phase + t as f32).sin() * rw * 0.25 * f;
                    let x = cx + ox * (1. + f * 0.3) + sway + j.trail * rw * 1.2 * f * f;
                    let y = cy + s as f32;
                    let ch = if s % 5 == 0 {
                        '°'
                    } else if f > 0.75 {
                        '.'
                    } else {
                        ':'
                    };
                    r.dot(
                        x.round() as i32,
                        y as i32,
                        depth + 0.2,
                        ch,
                        scale(tint, bright * (0.8 - f * 0.6)),
                    );
                }
            }
        }
    }
}
