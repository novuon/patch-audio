// Adapted from avillagran/omarchy-ttfx-layer, MIT, c969b6c172108c5f6ab937d99db67c17ddd28196.
// See LICENSE and README.md in this directory.
#![allow(dead_code)] // Keep the upstream camera/raster helpers together.
use super::{Sound, journey::variant, raster::Raster};
use std::f32::consts::{PI, TAU};
type Color = (u8, u8, u8);
type V = [f32; 3];

// ---------------------------------------------------------------- vectors
fn add(a: V, b: V) -> V {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: V, b: V) -> V {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn mul(a: V, k: f32) -> V {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn dot(a: V, b: V) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V, b: V) -> V {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn len(a: V) -> f32 {
    dot(a, a).sqrt()
}
fn norm(a: V) -> V {
    let l = len(a);
    if l > 1e-9 {
        mul(a, 1. / l)
    } else {
        [0., 1., 0.]
    }
}
/// Rotation about the vertical axis.
fn yaw(p: V, a: f32) -> V {
    let (s, c) = a.sin_cos();
    [p[0] * c + p[2] * s, p[1], -p[0] * s + p[2] * c]
}
/// Rotation about the x axis.
fn tilt(p: V, a: f32) -> V {
    let (s, c) = a.sin_cos();
    [p[0], p[1] * c - p[2] * s, p[1] * s + p[2] * c]
}
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0., 1.);
    t * t * (3. - 2. * t)
}

// ----------------------------------------------------------------- colour
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
/// Piecewise-linear colour ramp over ascending stops.
fn grad(stops: &[(f32, Color)], t: f32) -> Color {
    if t <= stops[0].0 {
        return stops[0].1;
    }
    for w in stops.windows(2) {
        if t <= w[1].0 {
            return mix(w[0].1, w[1].1, (t - w[0].0) / (w[1].0 - w[0].0));
        }
    }
    stops[stops.len() - 1].1
}
/// Glyph density for a light level in [0, 1]. No ',' or ';': the preview
/// wire format reserves them.
fn ramp(l: f32) -> char {
    const R: [char; 10] = ['.', '·', ':', '-', '=', '+', '*', '#', '%', '@'];
    R[((l.clamp(0., 0.999)) * R.len() as f32) as usize]
}
/// Shade blocks for solid ground: smooth relief instead of letters.
fn block(l: f32) -> char {
    match l {
        l if l < 0.1 => '·',
        l if l < 0.2 => ':',
        l if l < 0.34 => '░',
        l if l < 0.55 => '▒',
        l if l < 0.8 => '▓',
        _ => '█',
    }
}
/// A point glyph that grows as it nears or brightens.
fn speck(l: f32) -> char {
    match l {
        l if l < 0.3 => '.',
        l if l < 0.55 => '·',
        l if l < 0.8 => '+',
        l if l < 1.05 => '*',
        _ => '✦',
    }
}

// ------------------------------------------------------------------ noise
fn hash3(x: i32, y: i32, z: i32) -> f32 {
    variant(
        (x as u32).wrapping_mul(73856093)
            ^ (y as u32).wrapping_mul(19349663)
            ^ (z as u32).wrapping_mul(83492791),
    )
}
/// `floor` without libm: on baseline x86-64 `f32::floor` is a function call,
/// and the rasterizer and the noise run it tens of thousands of times a frame.
fn flo(x: f32) -> f32 {
    let i = x as i32 as f32;
    if i > x { i - 1. } else { i }
}
/// `ceil` without libm, as `flo`.
fn cei(x: f32) -> f32 {
    let i = x as i32 as f32;
    if i < x { i + 1. } else { i }
}
fn s3(t: f32) -> f32 {
    t * t * (3. - 2. * t)
}
/// Trilinear value noise in [0, 1).
fn noise3(p: V) -> f32 {
    let (xi, yi, zi) = (flo(p[0]), flo(p[1]), flo(p[2]));
    let (fx, fy, fz) = (s3(p[0] - xi), s3(p[1] - yi), s3(p[2] - zi));
    let (x, y, z) = (xi as i32, yi as i32, zi as i32);
    let l = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let plane = |z: i32| {
        l(
            l(hash3(x, y, z), hash3(x + 1, y, z), fx),
            l(hash3(x, y + 1, z), hash3(x + 1, y + 1, z), fx),
            fy,
        )
    };
    l(plane(z), plane(z + 1), fz)
}
/// Value noise in [0, 1) whose lattice repeats every `period` cells in z.
fn noise2p(x: f32, z: f32, period: i32) -> f32 {
    let (xi, zi) = (flo(x), flo(z));
    let (fx, fz) = (s3(x - xi), s3(z - zi));
    let (x, z0) = (xi as i32, (zi as i32).rem_euclid(period));
    let z1 = (z0 + 1) % period;
    let h = |i: i32, j: i32| hash3(i, j, 7);
    let a = h(x, z0) + (h(x + 1, z0) - h(x, z0)) * fx;
    let b = h(x, z1) + (h(x + 1, z1) - h(x, z1)) * fx;
    a + (b - a) * fz
}

struct Rng(u32);
impl Rng {
    fn new() -> Self {
        Self(0)
    }
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
        (self.0 >> 8) as f32 / 16777216.
    }
    fn signed(&mut self) -> f32 {
        self.next() * 2. - 1.
    }
    fn unit(&mut self) -> V {
        let z = self.signed();
        let a = self.next() * TAU;
        let r = (1. - z * z).max(0.).sqrt();
        [r * a.cos(), z, r * a.sin()]
    }
}

/// Rising edges of a smoothed level: a beat, not a loud passage.
#[derive(Default)]
struct Pulse {
    slow: f32,
    cool: f32,
}
impl Pulse {
    fn hit(&mut self, v: f32, dt: f32) -> Option<f32> {
        self.cool -= dt;
        let rise = v - self.slow;
        self.slow += (v - self.slow) * (1. - (-dt * 2.5).exp());
        (rise > 0.1 && v > 0.18 && self.cool <= 0.).then(|| {
            self.cool = 0.2;
            (rise * 2. + v * 0.4).min(1.5)
        })
    }
}

// ----------------------------------------------------------------- camera
const NEAR: f32 = 0.1;
#[derive(Clone, Copy)]
struct Cam {
    pos: V,
    right: V,
    up: V,
    fwd: V,
    /// Focal length in rows.
    f: f32,
    cx: f32,
    cy: f32,
    /// Cell height over cell width.
    aspect: f32,
}
impl Cam {
    fn look(pos: V, target: V, fov: f32, cols: usize, rows: usize, aspect: f32) -> Self {
        let fwd = norm(sub(target, pos));
        let right = norm(cross([0., 1., 0.], fwd));
        let up = cross(fwd, right);
        // Wide grids keep the vertical field; narrow ones keep the width.
        let wide = cols as f32 / (rows as f32 * aspect).max(1.);
        let f = rows as f32 * 0.5 / (fov * 0.5).tan() * (wide / 1.6).min(1.);
        Self {
            pos,
            right,
            up,
            fwd,
            f: f.max(1.),
            cx: cols as f32 * 0.5,
            cy: rows as f32 * 0.5,
            aspect,
        }
    }
    fn view(&self, p: V) -> V {
        let d = sub(p, self.pos);
        [dot(d, self.right), dot(d, self.up), dot(d, self.fwd)]
    }
    fn screen(&self, v: V) -> Option<(f32, f32, f32)> {
        (v[2] > NEAR).then(|| {
            (
                self.cx + v[0] / v[2] * self.f * self.aspect,
                self.cy - v[1] / v[2] * self.f,
                v[2],
            )
        })
    }
    fn project(&self, p: V) -> Option<(f32, f32, f32)> {
        self.screen(self.view(p))
    }
    /// Screen position of a direction at infinity.
    fn sky(&self, d: V) -> Option<(f32, f32)> {
        let v = [dot(d, self.right), dot(d, self.up), dot(d, self.fwd)];
        (v[2] > 0.02).then(|| {
            (
                self.cx + v[0] / v[2] * self.f * self.aspect,
                self.cy - v[1] / v[2] * self.f,
            )
        })
    }
    fn ray(&self, sx: f32, sy: f32) -> V {
        norm(add(
            self.fwd,
            add(
                mul(self.right, (sx - self.cx) / (self.f * self.aspect)),
                mul(self.up, (self.cy - sy) / self.f),
            ),
        ))
    }
}
/// A camera orbiting `target` at `dist`, `az` round and `el` above.
fn orbit(target: V, az: f32, el: f32, dist: f32) -> V {
    add(
        target,
        [
            dist * el.cos() * az.sin(),
            dist * el.sin(),
            -dist * el.cos() * az.cos(),
        ],
    )
}

/// A projected vertex: screen position, 1/depth and three attributes that are
/// interpolated across the triangle.
#[derive(Clone, Copy)]
struct Vx {
    x: f32,
    y: f32,
    w: f32,
    a: [f32; 3],
}

struct Canvas<'a> {
    r: &'a mut Raster,
    cols: usize,
    rows: usize,
    cam: Cam,
}
impl Canvas<'_> {
    fn dot(&mut self, x: f32, y: f32, z: f32, ch: char, c: Color) {
        if x >= 0. && y >= 0. && x < self.cols as f32 && y < self.rows as f32 {
            self.r.dot(x as i32, y as i32, z, ch, c);
        }
    }
    fn point(&mut self, p: V, ch: char, c: Color) -> Option<(f32, f32, f32)> {
        let s = self.cam.project(p)?;
        self.dot(s.0, s.1, s.2, ch, c);
        Some(s)
    }
    /// A segment with one depth: keep segments short where depth matters.
    fn line(&mut self, a: V, b: V, ch: Option<char>, c: Color) {
        let (mut va, mut vb) = (self.cam.view(a), self.cam.view(b));
        if va[2] < NEAR && vb[2] < NEAR {
            return;
        }
        // Clip to the near plane.
        if va[2] < NEAR || vb[2] < NEAR {
            let t = (NEAR - va[2]) / (vb[2] - va[2]);
            let cut = add(va, mul(sub(vb, va), t));
            if va[2] < NEAR {
                va = cut;
            } else {
                vb = cut;
            }
            let push = |v: &mut V| v[2] = v[2].max(NEAR * 1.01);
            push(&mut va);
            push(&mut vb);
        }
        let (Some(sa), Some(sb)) = (self.cam.screen(va), self.cam.screen(vb)) else {
            return;
        };
        self.r.line_with_glyph(
            (sa.0 - 0.5, sa.1 - 0.5),
            (sb.0 - 0.5, sb.1 - 0.5),
            (sa.2 + sb.2) * 0.5,
            c,
            ch.is_none(),
            ch,
        );
    }
    fn vx(&self, p: V, a: [f32; 3]) -> Option<Vx> {
        let (x, y, z) = self.cam.project(p)?;
        Some(Vx { x, y, w: 1. / z, a })
    }
    /// Fills a triangle at cell centres. `paint` gets the interpolated
    /// attributes and the depth and returns the cell, or nothing to skip it.
    fn tri(
        &mut self,
        p: Vx,
        q: Vx,
        s: Vx,
        paint: &mut impl FnMut([f32; 3], f32) -> Option<(char, Color)>,
    ) {
        let area = (q.x - p.x) * (s.y - p.y) - (q.y - p.y) * (s.x - p.x);
        if !area.is_finite() || area.abs() < 1e-5 {
            return;
        }
        let inv = 1. / area;
        let lo_x = p.x.min(q.x).min(s.x).max(0.);
        let hi_x = p.x.max(q.x).max(s.x).min(self.cols as f32);
        let lo_y = p.y.min(q.y).min(s.y).max(0.);
        let hi_y = p.y.max(q.y).max(s.y).min(self.rows as f32);
        if lo_x >= hi_x || lo_y >= hi_y {
            return;
        }
        let (x0, x1) = (
            cei((lo_x - 0.5).max(0.)) as usize,
            (flo(hi_x - 0.5) as usize).min(self.cols - 1),
        );
        let (y0, y1) = (
            cei((lo_y - 0.5).max(0.)) as usize,
            (flo(hi_y - 0.5) as usize).min(self.rows - 1),
        );
        // Each weight is linear along a row, and so is everything blended
        // from them: per row, solve for the span where all three weights
        // are non-negative and set up depth and attributes as a + b·x, so a
        // cell costs a few multiply-adds and one division. A cell of slack
        // each side of the span leaves the exact test to decide.
        let (b0, b1) = ((q.y - s.y) * inv, (s.y - p.y) * inv);
        let b2 = -b0 - b1;
        // 1/depth and the three attributes, blended by three weights.
        let attrs = |v: &Vx| [v.w, v.a[0], v.a[1], v.a[2]];
        let (ap, aq, as_) = (attrs(&p), attrs(&q), attrs(&s));
        let blend = |k0: f32, k1: f32, k2: f32| -> [f32; 4] {
            std::array::from_fn(|i| k0 * ap[i] + k1 * aq[i] + k2 * as_[i])
        };
        let slope = blend(b0, b1, b2);
        for y in y0..=y1 {
            let py = y as f32 + 0.5;
            let a0 = (q.x * (s.y - py) - (q.y - py) * s.x) * inv;
            let a1 = (s.x * (p.y - py) - (s.y - py) * p.x) * inv;
            let a2 = 1. - a0 - a1;
            let (mut lo, mut hi) = (x0 as f32 + 0.5, x1 as f32 + 0.5);
            for (a, b) in [(a0, b0), (a1, b1), (a2, b2)] {
                if b.abs() < 1e-12 {
                    if a < -1e-4 {
                        hi = f32::MIN;
                    }
                } else if b > 0. {
                    lo = lo.max((-1e-4 - a) / b);
                } else {
                    hi = hi.min((-1e-4 - a) / b);
                }
            }
            if lo.partial_cmp(&hi).is_none_or(|o| o.is_gt()) {
                continue;
            }
            let xa = (flo(lo - 0.5) as i64 - 1).max(x0 as i64) as usize;
            let xb = (cei(hi - 0.5) as i64 + 1).min(x1 as i64);
            if xb < xa as i64 {
                continue;
            }
            let base = blend(a0, a1, a2);
            for x in xa..=xb as usize {
                let px = x as f32 + 0.5;
                let (w0, w1) = (a0 + b0 * px, a1 + b1 * px);
                if w0 < -1e-4 || w1 < -1e-4 || 1. - w0 - w1 < -1e-4 {
                    continue;
                }
                let w = base[0] + slope[0] * px;
                if w <= 0. {
                    continue;
                }
                let z = 1. / w;
                if self.r.hidden(x, y, z) {
                    continue;
                }
                let at = [
                    base[1] + slope[1] * px,
                    base[2] + slope[2] * px,
                    base[3] + slope[3] * px,
                ];
                if let Some((ch, c)) = paint(at, z) {
                    self.r.dot(x as i32, y as i32, z, ch, c);
                }
            }
        }
    }
    /// Ray-traces a sphere over its screen bounds. `paint` gets the world hit
    /// point, the outward normal and the depth.
    fn sphere(
        &mut self,
        c: V,
        rad: f32,
        paint: &mut impl FnMut(V, V, f32) -> Option<(char, Color)>,
    ) {
        let v = self.cam.view(c);
        if v[2] <= rad + NEAR {
            return;
        }
        let Some((sx, sy, _)) = self.cam.screen(v) else {
            return;
        };
        // Off-axis spheres project to ellipses stretched outward: pad for it.
        let off = ((v[0] * v[0] + v[1] * v[1]).sqrt() / v[2]).min(2.);
        let sr = rad / (v[2] - rad).max(NEAR) * self.cam.f * (1.1 + off * 0.5) + 1.;
        let x0 = (sx - sr * self.cam.aspect).max(0.) as usize;
        let x1 = ((sx + sr * self.cam.aspect).max(0.) as usize).min(self.cols.saturating_sub(1));
        let y0 = (sy - sr).max(0.) as usize;
        let y1 = ((sy + sr).max(0.) as usize).min(self.rows.saturating_sub(1));
        if sx + sr * self.cam.aspect < 0. || sy + sr < 0. || x0 > x1 || y0 > y1 {
            return;
        }
        let oc = sub(self.cam.pos, c);
        let cc = dot(oc, oc) - rad * rad;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let d = self.cam.ray(x as f32 + 0.5, y as f32 + 0.5);
                let b = dot(oc, d);
                let h = b * b - cc;
                if h < 0. {
                    continue;
                }
                let t = -b - h.sqrt();
                if t < NEAR {
                    continue;
                }
                let hit = add(self.cam.pos, mul(d, t));
                let n = mul(sub(hit, c), 1. / rad);
                let depth = t * dot(d, self.cam.fwd);
                if let Some((ch, col)) = paint(hit, n, depth) {
                    self.r.dot(x as i32, y as i32, depth, ch, col);
                }
            }
        }
    }
    fn text(&mut self, x: i32, y: i32, z: f32, s: &str, c: Color) {
        for (i, ch) in s.chars().enumerate() {
            let ch = match ch {
                ',' => '·',
                ';' => ':',
                c => c,
            };
            if ch != ' ' {
                self.r.dot(x + i as i32, y, z, ch, c);
            }
        }
    }
    /// Text centred on a world point, in front of everything near it.
    fn label(&mut self, p: V, rise: f32, s: &str, c: Color) {
        if self.cols < 90 {
            return;
        }
        if let Some((x, y, z)) = self.cam.project(p) {
            let n = s.chars().count() as f32;
            self.text((x - n * 0.5) as i32, (y - rise) as i32, z - 0.5, s, c);
        }
    }
}

// ------------------------------------------------------------------- sky
struct Star {
    dir: V,
    mag: f32,
    phase: f32,
}
fn starfield(n: usize, rng: &mut Rng) -> Vec<Star> {
    (0..n)
        .map(|_| Star {
            dir: rng.unit(),
            mag: rng.next().powi(3),
            phase: rng.next() * TAU,
        })
        .collect()
}
fn draw_sky(cv: &mut Canvas, stars: &[Star], time: f32, treble: f32) {
    for s in stars {
        if let Some((x, y)) = cv.cam.sky(s.dir) {
            let tw = 0.65 + 0.35 * (time * (1.5 + s.mag * 3.) + s.phase).sin();
            let l = (0.25 + s.mag * 0.9) * tw + treble * s.mag * 0.6;
            cv.dot(x, y, 1e5, speck(l), scale((190, 205, 255), 0.35 + l * 0.7));
        }
    }
}
/// Radial streamers round a disc on screen, as round a star.
fn corona(
    cv: &mut Canvas,
    (cx, cy): (f32, f32),
    radius: f32,
    depth: f32,
    time: f32,
    color: Color,
    reach: f32,
    light: f32,
) {
    let aspect = cv.cam.aspect;
    let n = ((TAU * radius * aspect * 1.2) as usize + 24).min(3000);
    let steps = (radius * reach).ceil().max(2.) as usize * 2;
    for i in 0..n {
        let a = i as f32 / n as f32 * TAU;
        let streak = noise3([a.cos() * 2.5 + 9., a.sin() * 2.5, time * 0.3]) * 0.7
            + noise3([a.cos() * 7., a.sin() * 7., time * 0.6]) * 0.3;
        let len = reach * (0.25 + streak * streak * 1.6);
        for k in 0..steps {
            let f = k as f32 / steps as f32;
            if f > len / reach {
                break;
            }
            let r = radius * (1.03 + f * reach);
            let l = light * (1. - f * reach / len).max(0.) * (0.5 + streak);
            if l < 0.06 {
                continue;
            }
            cv.dot(
                cx + a.cos() * r * aspect,
                cy + a.sin() * r,
                depth,
                if f < 0.08 { ramp(l) } else { speck(l * 0.8) },
                scale(color, 0.25 + l * 0.8),
            );
        }
    }
}
/// A ragged glow round a disc on screen: corona, photon ring.
fn halo(
    cv: &mut Canvas,
    (cx, cy): (f32, f32),
    radius: f32,
    depth: f32,
    time: f32,
    color: Color,
    layers: &[(f32, f32)],
    gate: f32,
) {
    let aspect = cv.cam.aspect;
    let n = ((TAU * radius * aspect * 1.4) as usize + 24).min(4000);
    for (li, &(reach, light)) in layers.iter().enumerate() {
        for i in 0..n {
            let a = i as f32 / n as f32 * TAU;
            let streak = noise3([
                a.cos() * 3. + 9.,
                a.sin() * 3.,
                time * 0.35 + li as f32 * 1.7,
            ]);
            if streak < gate {
                continue;
            }
            let r = radius * (reach + (streak - gate) * 0.25 * li as f32);
            let l = light * (0.6 + streak * 0.6);
            cv.dot(
                cx + a.cos() * r * aspect,
                cy + a.sin() * r,
                depth,
                ramp(l),
                scale(color, l),
            );
        }
    }
}

// --------------------------------------------------------------- machine
/// A free particle.
struct Mote {
    p: V,
    v: V,
    life: f32,
}

// ======================================================== event horizon
/// A hot spot riding the disk: a beat, or a star that fell in.
struct Spot {
    r: f32,
    a: f32,
    heat: f32,
}
/// Rock or star dust thrown in where the cursor points; `size` 0 is dust.
struct Rock {
    p: V,
    v: V,
    size: f32,
}
pub(crate) struct Horizon {
    spots: Vec<Spot>,
    debris: Vec<Rock>,
    /// The world point under the cursor, found by the last draw.
    aim: std::cell::Cell<Option<V>>,
    spawn: f32,
    /// A flash of the photon ring each time the hole swallows something.
    gulp: f32,
    jets: Vec<Mote>,
    stream: Vec<Mote>,
    stars: Vec<Star>,
    time: f32,
    drift: f32,
    meal: f32,
    jet_budget: f32,
    glow: f32,
    rng: Rng,
}
const GM: f32 = 9.;
/// Schwarzschild radius; the shadow is 2.6 of them across the axis.
const RS: f32 = 0.62;
const B_CRIT: f32 = 2.6 * RS;
const INNER: f32 = 1.9;
const OUTER: f32 = 7.4;
fn disk_color(t: f32) -> Color {
    grad(
        &[
            (0.0, (90, 20, 30)),
            (0.25, (200, 60, 30)),
            (0.45, (255, 140, 50)),
            (0.65, (255, 210, 130)),
            (0.85, (255, 245, 225)),
            (1.0, (200, 225, 255)),
        ],
        t,
    )
}
fn kepler(r: f32) -> f32 {
    (GM / (r * r * r)).sqrt() * 0.55
}
impl Horizon {
    pub(super) fn new(seed: u32) -> Self {
        let mut rng = Rng(seed);
        let stars = starfield(1100, &mut rng);
        Self {
            spots: vec![],
            debris: vec![],
            aim: std::cell::Cell::new(None),
            spawn: 0.,
            gulp: 0.,
            jets: vec![],
            stream: vec![],
            stars,
            time: rng.next() * 100.,
            drift: rng.next() * TAU,
            meal: 6.,
            jet_budget: 0.,
            glow: 0.,
            rng,
        }
    }
    pub(super) fn step(&mut self, dt: f32, sound: &Sound, cursor: Option<(f32, f32)>) {
        // The disk texture shears with time; wrapping it now and then keeps
        // f32 precision, at the cost of one jump every few hours.
        self.time = (self.time + dt) % 20000.;
        self.drift += dt * 0.05;
        self.glow += (sound.bass - self.glow) * (1. - (-dt * 6.).exp());
        let feed = 0.05 + sound.bass * 0.35;
        for s in &mut self.spots {
            s.a = (s.a + kepler(s.r) * dt).rem_euclid(TAU);
            s.r -= dt * feed * (INNER * 1.5 / s.r);
            s.heat *= (-dt * 0.45).exp();
        }
        self.spots.retain(|s| s.r > INNER && s.heat > 0.05);
        // A beat ignites a hot spot that winds inward with the flow.
        let beat = sound.beat.then_some((sound.bass * 1.2).max(0.25));
        if let Some(k) = beat
            && self.spots.len() < 10
        {
            self.spots.push(Spot {
                r: INNER + 0.6 + self.rng.next() * 3.5,
                a: self.rng.next() * TAU,
                heat: k * 1.4,
            });
        }
        self.feed_debris(dt, sound, cursor.and(self.aim.get()), beat);
        // Treble drives the polar jets.
        self.jet_budget += dt * (30. + sound.treble * 650.);
        while self.jet_budget >= 1. {
            self.jet_budget -= 1.;
            if self.jets.len() >= 2600 {
                continue;
            }
            let side = if self.rng.next() < 0.5 { -1. } else { 1. };
            let (sx, sz) = (self.rng.signed() * 0.18, self.rng.signed() * 0.18);
            self.jets.push(Mote {
                // Born outside the shadow, so no beam runs across the hole.
                p: [sx * 0.6, side * B_CRIT * 1.15, sz * 0.6],
                v: [
                    sx,
                    side * (6. + self.rng.next() * 6. + sound.treble * 4.),
                    sz,
                ],
                life: 1.,
            });
        }
        let swirl = dt * 1.8;
        for m in &mut self.jets {
            m.p = yaw(add(m.p, mul(m.v, dt)), swirl);
            m.life -= dt * 0.55;
        }
        self.jets.retain(|m| m.life > 0.);
        // Now and then a star wanders too close and is torn into a stream.
        self.meal -= dt * (1. + sound.bass * 2.);
        if self.meal <= 0. && self.stream.is_empty() {
            self.meal = 16. + self.rng.next() * 14.;
            let phi = self.rng.next() * TAU;
            let start = [phi.cos() * 11., self.rng.signed() * 1.2, phi.sin() * 11.];
            let along = [-phi.sin(), 0., phi.cos()];
            let speed = (GM / 11.).sqrt() * 0.42;
            for _ in 0..700 {
                let off = mul(self.rng.unit(), 0.55 * self.rng.next().cbrt());
                self.stream.push(Mote {
                    p: add(start, off),
                    v: mul(along, speed * (1. + self.rng.signed() * 0.14)),
                    life: 1.,
                });
            }
        }
        let h = dt / 3.;
        let drag = (-h * 0.12).exp();
        let mut fell = None;
        for m in &mut self.stream {
            for _ in 0..3 {
                let d = len(m.p).max(0.3);
                m.v = mul(add(m.v, mul(m.p, -GM / (d * d * d) * h)), drag);
                m.p = add(m.p, mul(m.v, h));
            }
            m.life -= dt * 0.025;
            let d = len(m.p);
            if d < INNER + 0.5 {
                fell = Some((d.max(INNER + 0.1), m.p[2].atan2(m.p[0]).rem_euclid(TAU)));
                m.life = 0.;
            } else if d > 18. {
                m.life = 0.;
            }
        }
        self.stream.retain(|m| m.life > 0.);
        // What falls in lights the disk where it lands.
        if let Some((r, a)) = fell {
            self.land(r, a, 0.02);
        }
    }
    /// Rocks and dust appear round the cursor, steadily while it is there and
    /// in a burst on each beat, then spiral in and are swallowed. Without a
    /// cursor a beat still throws a little dust in from the dark.
    fn feed_debris(&mut self, dt: f32, sound: &Sound, aim: Option<V>, beat: Option<f32>) {
        self.gulp *= (-dt * 3.).exp();
        let from = match aim {
            Some(a) => {
                self.spawn += dt * (16. + sound.bass * 90. + sound.treble * 40.)
                    + beat.map_or(0., |k| 30. + k * 60.);
                Some(a)
            }
            None => beat.map(|k| {
                self.spawn += 12. + k * 30.;
                let phi = self.rng.next() * TAU;
                [phi.cos() * 10.5, self.rng.signed() * 2.5, phi.sin() * 10.5]
            }),
        };
        if let Some(at) = from {
            while self.spawn >= 1. {
                self.spawn -= 1.;
                if self.debris.len() >= 1600 {
                    continue;
                }
                let size = if self.rng.next() < 0.14 {
                    0.4 + self.rng.next() * 0.6
                } else {
                    0.
                };
                let p = add(
                    at,
                    mul(
                        self.rng.unit(),
                        0.25 + self.rng.next() * (0.5 + sound.bass * 0.6),
                    ),
                );
                let r = len(p).max(1.);
                // Mostly round the hole, a little inward: a slow spiral in.
                let along = norm(cross([0., 1., 0.], p));
                let v = add(
                    mul(along, (GM / r).sqrt() * (0.5 + self.rng.next() * 0.25)),
                    add(mul(p, -0.06), mul(self.rng.unit(), 0.25)),
                );
                self.debris.push(Rock { p, v, size });
            }
        }
        self.spawn = self.spawn.min(4.);
        let h = dt / 3.;
        let drag = (-h * 0.3).exp();
        let mut eaten = vec![];
        self.debris.retain_mut(|m| {
            for _ in 0..3 {
                let d = len(m.p).max(0.3);
                m.v = mul(add(m.v, mul(m.p, -GM / (d * d * d) * h)), drag);
                m.p = add(m.p, mul(m.v, h));
            }
            let d = len(m.p);
            let flat = (m.p[0] * m.p[0] + m.p[2] * m.p[2]).sqrt();
            if d < B_CRIT * 1.05 || (flat < INNER + 0.3 && m.p[1].abs() < 0.4) {
                eaten.push((
                    flat.max(INNER + 0.1),
                    m.p[2].atan2(m.p[0]).rem_euclid(TAU),
                    m.size,
                ));
                return false;
            }
            d < 22.
        });
        for (r, a, size) in eaten {
            self.gulp = (self.gulp + 0.03 + size * 0.35).min(1.5);
            self.land(r, a, 0.01 + size * 0.3);
        }
    }
    fn land(&mut self, r: f32, a: f32, heat: f32) {
        {
            let full = self.spots.len() >= 14;
            match self.spots.iter_mut().find(|s| {
                ((s.a - a + PI).rem_euclid(TAU) - PI).abs() < 0.4 && (s.r - r).abs() < 0.6
            }) {
                Some(s) => s.heat = (s.heat + heat).min(2.),
                None if !full => self.spots.push(Spot {
                    r,
                    a,
                    heat: 0.5 + heat,
                }),
                None => {}
            }
        }
    }
    fn draw(&self, cv: &mut Canvas, sound: &Sound, gaze: (f32, f32)) {
        // Nearly edge on, so the far side of the disk rises over the shadow;
        // the cursor only adds a touch of parallax, so the camera neither
        // swings round with every move nor falls flat into the plane.
        let az = self.drift + gaze.0 * 0.3;
        let el = (0.14 - gaze.1 * 0.08).clamp(0.07, 0.24);
        let dist = 13.5 - self.glow * 0.8;
        cv.cam = Cam::look(
            orbit([0., 0., 0.], az, el, dist),
            [0., 0., 0.],
            1.0,
            cv.cols,
            cv.rows,
            cv.cam.aspect,
        );
        let cam = cv.cam;
        // Under the cursor, on the plane through the hole that faces us.
        let (ux, uy) = (
            (gaze.0 + 1.) * 0.5 * cv.cols as f32,
            (gaze.1 + 1.) * 0.5 * cv.rows as f32,
        );
        let ray = cam.ray(ux, uy);
        let t = -dot(cam.pos, cam.fwd) / dot(ray, cam.fwd).max(0.05);
        let p = add(cam.pos, mul(ray, t));
        self.aim.set(Some(mul(p, (15. / len(p).max(1e-3)).min(1.))));
        let Some((hx, hy, hd)) = cam.project([0., 0., 0.]) else {
            return;
        };
        let aspect = cam.aspect;
        let shadow = B_CRIT * cam.f / hd;
        let einstein = shadow * 1.2;
        // Point lens for what is not ray traced: stars, jets, the stream.
        let lens = |sx: f32, sy: f32, behind: f32| -> ((f32, f32), Option<((f32, f32), f32)>) {
            if behind <= 0. {
                return ((sx, sy), None);
            }
            let e = einstein * (behind / (behind + 2.)).sqrt();
            let (dx, dy) = ((sx - hx) / aspect, sy - hy);
            let d = (dx * dx + dy * dy).sqrt();
            let (ux, uy) = if d > 1e-3 {
                (dx / d, dy / d)
            } else {
                (0., -1.)
            };
            let root = (d * d + 4. * e * e).sqrt();
            let d1 = 0.5 * (d + root);
            let d2 = 0.5 * (d - root);
            let weak = (e * e / (d1 * d1).max(1e-3)).min(1.) * 0.7;
            (
                (hx + ux * d1 * aspect, hy + uy * d1),
                (d2.abs() > shadow).then_some(((hx + ux * d2 * aspect, hy + uy * d2), weak)),
            )
        };
        for s in &self.stars {
            let Some((x, y)) = cam.sky(s.dir) else {
                continue;
            };
            let ((x, y), _) = lens(x, y, 1e3);
            let tw = 0.65 + 0.35 * (self.time * (1.5 + s.mag * 3.) + s.phase).sin();
            let l = (0.25 + s.mag * 0.9) * tw + sound.treble * s.mag * 0.6;
            cv.dot(x, y, 1e5, speck(l), scale((190, 205, 255), 0.35 + l * 0.7));
        }
        // The disk, ray traced per cell: each ray runs straight to its
        // closest approach, then leaves bent toward the hole, so the far side
        // of the disk rises over the shadow and its underside shows beneath.
        let mut x0 = hx - shadow * aspect * 2.5;
        let mut x1 = hx + shadow * aspect * 2.5;
        let mut y0 = hy - shadow * 2.5;
        let mut y1 = hy + shadow * 2.5;
        for i in 0..24 {
            let a = i as f32 / 24. * TAU;
            if let Some((x, y, _)) = cam.project([OUTER * a.cos(), 0., OUTER * a.sin()]) {
                x0 = x0.min(x - 1.);
                x1 = x1.max(x + 1.);
                y0 = y0.min(y - 1.);
                y1 = y1.max(y + 1.);
            } else {
                (x0, x1, y0, y1) = (0., cv.cols as f32, 0., cv.rows as f32);
            }
        }
        let (x0, x1) = (x0.max(0.) as usize, (x1.max(0.) as usize).min(cv.cols - 1));
        let (y0, y1) = (y0.max(0.) as usize, (y1.max(0.) as usize).min(cv.rows - 1));
        let c = cam.pos;
        let time = self.time;
        let glow = 0.8 + self.glow * 0.6 + self.gulp * 0.25;
        let bend_tail = PI - 2. * RS / B_CRIT;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let d = cam.ray(x as f32 + 0.5, y as f32 + 0.5);
                let tca = -dot(c, d);
                let mut hit = None;
                if d[1].abs() > 1e-5 {
                    let t = -c[1] / d[1];
                    if t > 0. && (tca <= 0. || t <= tca) {
                        let h = add(c, mul(d, t));
                        let r = (h[0] * h[0] + h[2] * h[2]).sqrt();
                        if (INNER..OUTER).contains(&r) {
                            hit = Some((h, d, t, r));
                        }
                    }
                }
                let mut ring = 0.;
                if hit.is_none() && tca > 0. {
                    let p = add(c, mul(d, tca));
                    let b = len(p);
                    if b < B_CRIT {
                        cv.r.dot(x as i32, y as i32, tca * dot(d, cam.fwd), ' ', (0, 0, 0));
                        continue;
                    }
                    let alpha = 2. * RS / b + bend_tail * (B_CRIT / b).powi(10);
                    let d2 = norm(add(mul(d, alpha.cos()), mul(p, -alpha.sin() / b)));
                    if d2[1].abs() > 1e-5 {
                        let s = -p[1] / d2[1];
                        if s > 0. {
                            let h = add(p, mul(d2, s));
                            let r = (h[0] * h[0] + h[2] * h[2]).sqrt();
                            if (INNER..OUTER).contains(&r) {
                                hit = Some((h, d2, tca + s, r));
                            }
                        }
                    }
                    ring = (1. - (b - B_CRIT) / (B_CRIT * 0.09)).max(0.);
                }
                if let Some((h, dir, path, r)) = hit {
                    // atan2 is within ±π: one add puts it in [0, τ) without fmod.
                    let a = h[2].atan2(h[0]);
                    let a = if a < 0. { a + TAU } else { a };
                    let temp = (INNER / r).powf(0.9);
                    // Beaming: the side coming toward us burns brighter. The
                    // angle's cosine and sine are the hit point over its radius.
                    let approach = -(dir[2] * (h[0] / r) - dir[0] * (h[2] / r));
                    let doppler = (1. + 0.45 * approach * temp.sqrt()).powi(3);
                    let u = (a - kepler(r) * time) / TAU * 18.;
                    let streak = noise2p(r * 2.4, u, 18);
                    let fine = noise2p(r * 7.5, u * 3., 54);
                    let tex = 0.35 + 0.8 * streak * streak + 0.35 * fine;
                    let mut spot = 0.;
                    for s in &self.spots {
                        let da = (a - s.a + PI).rem_euclid(TAU) - PI;
                        spot +=
                            s.heat * (-((r - s.r).powi(2) / 0.2 + (da * r).powi(2) / 0.6)).exp();
                    }
                    let edge = smoothstep(INNER, INNER + 0.2, r)
                        * (1. - smoothstep(OUTER - 2.2, OUTER, r));
                    let l = ((0.18 + temp * 0.95) * tex * doppler * glow * edge + spot).min(1.8);
                    if l < 0.04 {
                        continue;
                    }
                    let color = disk_color((temp * 0.9 + spot * 0.3 + approach * 0.08).min(1.));
                    cv.r.dot(
                        x as i32,
                        y as i32,
                        path * dot(d, cam.fwd),
                        ramp(l * 0.72),
                        scale(color, 0.3 + l * 0.75),
                    );
                } else if ring > 0. {
                    let flicker = 0.75
                        + 0.25 * noise3([x as f32 * 0.3, y as f32 * 0.3, time * 2.])
                        + sound.treble * 0.3
                        + self.gulp * 0.6;
                    let l = ring * flicker;
                    cv.r.dot(
                        x as i32,
                        y as i32,
                        hd,
                        ramp(l * 0.8),
                        scale((255, 215, 170), 0.3 + l * 0.8),
                    );
                }
            }
        }
        let put = |cv: &mut Canvas, p: V, l: f32, color: Color, ch: char| {
            let v = cam.view(p);
            let Some((sx, sy, z)) = cam.screen(v) else {
                return;
            };
            let (primary, secondary) = lens(sx, sy, z - hd);
            cv.dot(primary.0, primary.1, z, ch, color);
            if let Some((p2, k)) = secondary
                && l * k > 0.08
            {
                cv.dot(p2.0, p2.1, z, ramp(l * k), scale(color, k * 1.4));
            }
        };
        for m in &self.jets {
            let l = m.life * (0.55 + sound.treble * 0.7);
            let color = scale(mix((150, 120, 255), (140, 220, 255), m.life), 0.4 + l * 0.8);
            put(cv, m.p, l, color, if m.life > 0.8 { '|' } else { speck(l) });
        }
        for m in &self.stream {
            let l = (0.6 + (len(m.v) * 0.25).min(0.6)) * m.life.min(0.3) / 0.3;
            put(cv, m.p, l, scale((255, 225, 170), l), speck(l));
        }
        // Debris heats as it nears the hole; rocks stretch out along their
        // fall, torn by the tide, before they vanish into it.
        for m in &self.debris {
            let d = len(m.p);
            let heat = (1. - (d - B_CRIT) / 7.).clamp(0., 1.);
            if m.size == 0. {
                let l = 0.45 + heat * 0.6 + sound.treble * 0.25;
                let color = mix((160, 190, 255), (255, 200, 120), heat);
                put(cv, m.p, l, scale(color, 0.4 + l * 0.6), speck(l));
            } else {
                let color = mix(
                    (150, 135, 125),
                    disk_color(0.55 + heat * 0.4),
                    heat.powf(0.7),
                );
                let l = 0.6 + heat * 0.5;
                let back = mul(norm(m.v), -1.);
                let tail = ((4.5 - d) / 2.5).clamp(0., 1.) * 4.;
                for k in (1..=tail as usize).rev() {
                    let q = add(m.p, mul(back, k as f32 * 0.16 * (1. + m.size)));
                    put(
                        cv,
                        q,
                        l * 0.7,
                        scale(color, 0.8 - k as f32 * 0.12),
                        if k < 2 { '•' } else { '·' },
                    );
                }
                put(
                    cv,
                    m.p,
                    l,
                    scale(color, 0.5 + l * 0.5),
                    if m.size > 0.75 { '●' } else { '•' },
                );
            }
        }
    }
}

impl Horizon {
    pub(super) fn paint(
        &self,
        r: &mut Raster,
        cols: usize,
        rows: usize,
        aspect: f32,
        sound: &Sound,
        gaze: (f32, f32),
    ) {
        let cam = Cam::look([0., 0., -10.], [0.; 3], 1., cols, rows, aspect);
        self.draw(&mut Canvas { r, cols, rows, cam }, sound, gaze);
    }
}
