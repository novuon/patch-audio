// Adapted from avillagran/omarchy-ttfx-layer, MIT, c969b6c172108c5f6ab937d99db67c17ddd28196.
// See ../LICENSE and ../README.md for provenance and integration notes.
use crate::ambient::{Sound,raster::{DirectCell,Raster}};
const TRAIL_PIECE: f32 = 0.5;
const BIKES: u32 = 3;
/// Cruising speed of each bike, away from the camera. Being different, they
/// catch up with each other's trails.
const BIKE_SPEED: [f32; 3] = [3.8, 4.9, 3.0];
const BIKE_RUN: f32 = 12.;
const BIKE_ALIVE: f32 = 9.5;
const TRAIL_LENGTH: f32 = 14.;
/// Closer than this across the tunnel and a bike has hit a trail.
const TRAIL_TOUCH: f32 = 0.34;
/// A new bike cannot crash yet: it would blow up in the camera's face.
const BIKE_SAFE_AGE: f32 = 1.2;
const CRASH_TIME: f32 = 1.5;
const ROUTE_LANES: [f32; 4] = [-2.2, -1.1, 1.1, 2.2];
/// Distance a bike holds its lane or moves to the next one.
const ROUTE_LEG: f32 = 11.;
const ROUTE_HOLD: f32 = 0.6;
/// Where a bike is across the tunnel at `distance` along it. The route belongs
/// to the tunnel, not to the camera: a piece of trail never moves once laid,
/// and it only bends where the bike changed lane.
fn bike_route(seed: u32, distance: f32) -> f32 {
    let lane = |leg: i32| {
        let h = (leg as u32).wrapping_mul(2654435761) ^ seed.wrapping_mul(40503);
        ROUTE_LANES[(h.wrapping_mul(2654435761) >> 30) as usize]
    };
    let leg = (distance / ROUTE_LEG).floor();
    let t = ((distance / ROUTE_LEG - leg - ROUTE_HOLD) / (1. - ROUTE_HOLD)).clamp(0., 1.);
    let (from, to) = (lane(leg as i32), lane(leg as i32 + 1));
    from + (to - from) * t * t * (3. - 2. * t)
}
fn bike_color(bike: u32) -> (u8, u8, u8) {
    if bike == 1 {
        (255, 150, 25)
    } else {
        (65, 245, 255)
    }
}
#[derive(Clone, Copy)]
struct Rider {
    seed: u32,
    run: u32,
    age: f32,
    z: f32,
}
/// A derezzed bike: where along the tunnel it blew up and what it had laid.
#[derive(Clone, Copy)]
struct Crash {
    run: u32,
    seed: u32,
    at: f32,
    head: f32,
    tail: f32,
    x: f32,
}
pub(crate) struct Tunnel {
    crashes: [Option<Crash>; BIKES as usize],
    raster: std::cell::RefCell<Option<Raster>>,
    time: f32,
    travel: f32,
    gaze: (f32, f32),
    sound: Sound,
    sparks: Vec<(f32, f32, f32)>,
    spawn: f32,
    serial: u32,
}
impl Tunnel {
    pub(crate) fn new() -> Self {
        Self {
            crashes: [None; BIKES as usize],
            raster: Default::default(),
            time: 0.,
            travel: 0.,
            gaze: (0., 0.),
            sound: Sound::default(),
            sparks: vec![],
            spawn: 0.,
            serial: crate::ambient::upstream::seed::next(),
        }
    }
    pub(crate) fn step(&mut self, dt: f32, pointer: Option<(f32, f32)>, sound: Sound) {
        let dt = dt.clamp(0., 0.1);
        self.time = (self.time + dt) % 628.;
        self.sound.follow(sound, dt);
        self.travel = (self.travel + dt * 3.8) % 1000.;
        for p in &mut self.sparks {
            p.2 -= dt * 9.;
        }
        self.sparks.retain(|p| p.2 > 0.8);
        let drive = 1. - (-3. * (self.sound.bass + self.sound.treble)).exp();
        self.spawn += dt * drive * 7.;
        while self.spawn >= 1. {
            self.spawn -= 1.;
            if self.sparks.len() < 40 {
                let h = self.serial.wrapping_mul(747796405).wrapping_add(2891336453);
                self.sparks.push((
                    if h & 1 == 0 { -3.12 } else { 3.12 },
                    ((h >> 8) & 65535) as f32 / 65535. * 3.4 - 1.7,
                    64. + ((h >> 24) & 255) as f32 / 255. * 8.,
                ));
            }
            self.serial = self.serial.wrapping_add(1);
        }
        self.collide();
        let target = pointer
            .map(|(x, y)| ((x - 0.5) * 2., (y - 0.5) * 2.))
            .unwrap_or((0., 0.));
        let k = 1. - (-dt * 5.).exp();
        self.gaze.0 += (target.0.clamp(-1., 1.) - self.gaze.0) * k;
        self.gaze.1 += (target.1.clamp(-1., 1.) - self.gaze.1) * k;
    }
    fn project(&self, x: f32, y: f32, z: f32, cols: usize, rows: usize, aspect: f32) -> (f32, f32) {
        let bend_x = (z * 0.075 + self.time * 0.23).sin() * z * 0.12;
        let bend_y = (z * 0.052 + self.time * 0.17).cos() * z * 0.055;
        let roll = (z * 0.027 + self.time * 0.13).sin() * 0.24 + self.gaze.0 * 0.08;
        let (s, c) = roll.sin_cos();
        let xx = x * c - y * s;
        let yy = x * s + y * c;
        let f = rows as f32 * 0.88;
        (
            cols as f32 * 0.5 + (xx + bend_x - self.gaze.0 * 0.75) * f * aspect / z,
            rows as f32 * 0.5 + (yy + bend_y - self.gaze.1 * 0.45) * f / z,
        )
    }
    /// A bike on the track right now, if it is riding and has not crashed.
    fn rider(&self, bike: u32) -> Option<Rider> {
        let clock = self.time + bike as f32 * 3.6;
        let age = clock % BIKE_RUN;
        let run = (clock / BIKE_RUN) as u32;
        let crashed = self.crashes[bike as usize].is_some_and(|c| c.run == run);
        (age <= BIKE_ALIVE && !crashed).then(|| Rider {
            seed: run.wrapping_mul(3).wrapping_add(bike),
            run,
            age,
            z: 0.7 + age * BIKE_SPEED[bike as usize],
        })
    }
    /// The first thing a bike touches is another bike's trail: it derezzes.
    fn collide(&mut self) {
        for bike in 0..BIKES {
            let Some(r) = self.rider(bike) else { continue };
            if r.age < BIKE_SAFE_AGE {
                continue;
            }
            let at = self.travel + r.z;
            let x = bike_route(r.seed, at);
            let hit = (0..BIKES).filter(|&o| o != bike).filter_map(|o| self.rider(o)).any(|o| {
                let inside = r.z <= o.z && r.z >= (o.z - TRAIL_LENGTH).max(0.65);
                inside && (x - bike_route(o.seed, at)).abs() < TRAIL_TOUCH
            });
            if hit {
                self.crashes[bike as usize] = Some(Crash {
                    run: r.run,
                    seed: r.seed,
                    at: self.time,
                    head: at,
                    tail: self.travel + (r.z - TRAIL_LENGTH).max(0.65),
                    x,
                });
            }
        }
    }
    /// A bike's trail: its route between two depths, as screen points close
    /// enough to show every turn it took.
    #[allow(clippy::too_many_arguments)]
    fn trail(&self, seed: u32, y: f32, from: f32, to: f32, cols: usize, rows: usize, aspect: f32) -> Vec<(f32, f32)> {
        let pieces = ((to - from) / TRAIL_PIECE).ceil().max(1.) as usize;
        (0..=pieces)
            .map(|i| {
                let z = (from + i as f32 * TRAIL_PIECE).min(to);
                self.project(bike_route(seed, self.travel + z), y, z, cols, rows, aspect)
            })
            .collect()
    }
    pub(crate) fn draw(&self, cols: usize, rows: usize, aspect: f32, cells: &mut Vec<DirectCell>) {
        cells.clear();
        if cols == 0 || rows == 0 {
            return;
        }
        let mut slot = self.raster.borrow_mut();
        let mut raster = match slot.take() {
            Some(r) if r.fits(cols, rows) => r,
            _ => Raster::new(cols, rows),
        };
        let spacing = 1.8;
        let phase = self.travel % spacing;
        // More than 20 true perspective cross-sections, connected by longitudinal rails.
        for n in (0..24).rev() {
            let z = 0.65 + n as f32 * spacing - phase;
            if z < 0.65 {
                continue;
            }
            let next = z + spacing;
            let pulse = (1. - ((z - (self.time * 12.) % 42.).abs() / 3.).min(1.)).max(0.)
                * self.sound.treble;
            for side in 0..4 {
                let edge = |t: f32| match side {
                    0 => (-3.2 + 6.4 * t, -2.2),
                    1 => (3.2, -2.2 + 4.4 * t),
                    2 => (3.2 - 6.4 * t, 2.2),
                    _ => (-3.2, 2.2 - 4.4 * t),
                };
                let color = if side == 0 || side == 2 {
                    (30., 210., 255.)
                } else {
                    (185., 55., 255.)
                };
                let intensity = (0.18 + 0.82 / (1. + z * 0.065) + pulse * 0.65).min(1.4);
                let rgb = |gain: f32| {
                    (
                        (color.0 * gain).min(255.) as u8,
                        (color.1 * gain).min(255.) as u8,
                        (color.2 * gain).min(255.) as u8,
                    )
                };
                let (a, b) = (edge(0.), edge(1.));
                raster.line(
                    self.project(a.0, a.1, z, cols, rows, aspect),
                    self.project(b.0, b.1, z, cols, rows, aspect),
                    z,
                    rgb(intensity),
                    true,
                );
                for lane in 0..=4 {
                    let p = edge(lane as f32 / 4.);
                    let gain = if lane == 0 || lane == 4 {
                        intensity
                    } else {
                        intensity * 0.32
                    };
                    raster.line(
                        self.project(p.0, p.1, z, cols, rows, aspect),
                        self.project(p.0, p.1, next, cols, rows, aspect),
                        z,
                        rgb(gain),
                        false,
                    );
                }
            }
        }
        // Ceiling light strips along the bore blink in step with the bass.
        for n in 0..12 {
            let z = 1. + (n as f32 * 3.6 - self.travel).rem_euclid(43.);
            let on =
                0.35 + self.sound.bass * 0.65 * (0.5 + 0.5 * (self.time * 6. + n as f32).sin());
            let project = |x, y, z| self.project(x, y, z, cols, rows, aspect);
            raster.line_with_glyph(
                project(-0.6, -2.15, z),
                project(0.6, -2.15, z),
                z - 0.01,
                ((255. * on) as u8, (240. * on) as u8, (200. * on) as u8),
                true,
                Some('▬'),
            );
        }
        // Dust streaks past the camera give the bore a sense of speed.
        for i in 0..28 {
            let h = (i as u32).wrapping_mul(2654435761);
            let x = ((h >> 4) & 1023) as f32 / 1023. * 5.6 - 2.8;
            let y = ((h >> 14) & 1023) as f32 / 1023. * 3.6 - 1.8;
            let z =
                0.9 + (((h >> 24) & 255) as f32 / 255. * 30. - self.travel * 2.2).rem_euclid(30.);
            let project = |x, y, z| self.project(x, y, z, cols, rows, aspect);
            raster.line_with_glyph(
                project(x, y, z),
                project(x, y, z + 0.6),
                z,
                (70, 80, 110),
                false,
                Some('.'),
            );
        }
        // Fast amber couriers: moving depth, no screen-space confetti.
        for i in 0..6 {
            let z = 0.8 + (i as f32 * 2.31 - self.travel * 1.7).rem_euclid(40.);
            let x = if i % 2 == 0 { -3.17 } else { 3.17 };
            let y = (i % 5) as f32 * 0.9 - 1.8;
            raster.line(
                self.project(x, y, z, cols, rows, aspect),
                self.project(x, y, z + 0.7, cols, rows, aspect),
                z,
                (255, 145 + (self.sound.treble * 100.) as u8, 45),
                false,
            );
        }
        // Musical packets are born far away and persist through an approach.
        for &(x, y, z) in &self.sparks {
            let fade = crate::ambient::journey::arrival_visibility(z, 42., 28.);
            if fade == 0. {
                continue;
            }
            let light = |c: (u8, u8, u8)| {
                (
                    (c.0 as f32 * fade) as u8,
                    (c.1 as f32 * fade) as u8,
                    (c.2 as f32 * fade) as u8,
                )
            };
            let project = |x, y, z| self.project(x, y, z, cols, rows, aspect);
            for dy in [-0.12, 0.12] {
                raster.line_with_glyph(
                    project(x, y + dy, z),
                    project(x, y + dy, z + 1.3),
                    z,
                    light((255, 200, 80)),
                    true,
                    Some('▬'),
                );
            }
            raster.line(
                project(x, y - 0.12, z),
                project(x, y + 0.12, z),
                z - 0.01,
                light((245, 250, 255)),
                true,
            );
        }
        // Amber navigation chevrons on the floor give a second, grounded speed cue.
        for n in 0..10 {
            let z = 1.2 + (n as f32 * 4.2 - self.travel).rem_euclid(42.);
            let project = |x, y, z| self.project(x, y, z, cols, rows, aspect);
            for sign in [-1., 1.] {
                raster.line(
                    project(sign * 0.55, 2.17, z + 0.9),
                    project(0., 2.17, z),
                    z,
                    (255, 175, 50),
                    true,
                );
            }
        }
        // Light bikes overtake the camera: their depth INCREASES, unlike scenery.
        for bike in 0..BIKES {
            let Some(Rider { seed, z, .. }) = self.rider(bike) else {
                continue;
            };
            let color = bike_color(bike);
            let lane_at = |depth: f32| bike_route(seed, self.travel + depth);
            let project = |x, y, z| self.project(x, y, z, cols, rows, aspect);
            // Rear-facing hoop wheels, front/back separation and a low chassis.
            for wheel_z in [z, z + 1.15] {
                let lane = lane_at(wheel_z);
                for segment in 0..16 {
                    let a = segment as f32 * std::f32::consts::TAU / 16.;
                    let b = (segment + 1) as f32 * std::f32::consts::TAU / 16.;
                    raster.line(
                        project(lane + a.cos() * 0.25, 1.83 + a.sin() * 0.34, wheel_z),
                        project(lane + b.cos() * 0.25, 1.83 + b.sin() * 0.34, wheel_z),
                        wheel_z - 0.02,
                        color,
                        true,
                    );
                }
            }
            for side in [-0.26, 0.26] {
                let (rear, front) = (lane_at(z), lane_at(z + 1.2));
                raster.line_with_glyph(
                    project(rear + side, 1.65, z),
                    project(front + side, 1.44, z + 1.2),
                    z - 0.04,
                    color,
                    true,
                    Some('▬'),
                );
                raster.line(
                    project(front + side, 1.44, z + 1.2),
                    project(lane_at(z + 0.55), 1.28, z + 0.55),
                    z - 0.05,
                    color,
                    true,
                );
            }
            // Trail remains behind the bike, clipped at the camera near plane.
            // It is many short pieces so it follows the bends of the tunnel.
            let tail = (z - TRAIL_LENGTH).max(0.65);
            for (y, glyph, tint, solid) in [
                (2.12, None, (color.0 / 2, color.1 / 2, color.2 / 2), true),
                (1.95, Some('▌'), color, false),
            ] {
                let points = self.trail(seed, y, tail, z, cols, rows, aspect);
                for (i, piece) in points.windows(2).enumerate() {
                    let near = tail + i as f32 * TRAIL_PIECE;
                    raster.line_with_glyph(piece[0], piece[1], near, tint, solid, glyph);
                }
            }
        }
        for bike in 0..BIKES {
            if let Some(crash) = self.crashes[bike as usize] {
                self.draw_crash(&mut raster, crash, bike_color(bike), cols, rows, aspect);
            }
        }
        raster.recycle(cells);
        *slot = Some(raster);
    }
    /// The bike bursts into shards where it hit, and what it laid falls apart.
    fn draw_crash(&self, raster: &mut Raster, crash: Crash, color: (u8, u8, u8), cols: usize, rows: usize, aspect: f32) {
        let t = self.time - crash.at;
        if !(0. ..CRASH_TIME).contains(&t) {
            return;
        }
        let life = 1. - t / CRASH_TIME;
        let shade = |c: (u8, u8, u8), k: f32| {
            (
                (c.0 as f32 * k).min(255.) as u8,
                (c.1 as f32 * k).min(255.) as u8,
                (c.2 as f32 * k).min(255.) as u8,
            )
        };
        let project = |x, y, z| self.project(x, y, z, cols, rows, aspect);
        let pieces = ((crash.head - crash.tail) / TRAIL_PIECE) as u32;
        for i in 0..pieces {
            let z = crash.tail + i as f32 * TRAIL_PIECE - self.travel;
            let gone = (i.wrapping_mul(2654435761) >> 22) as f32 / 1024.;
            if z < 0.65 || gone < t * 1.4 {
                continue;
            }
            let (a, b) = (bike_route(crash.seed, z + self.travel), bike_route(crash.seed, z + self.travel + TRAIL_PIECE));
            raster.line_with_glyph(project(a, 1.95, z), project(b, 1.95, z + TRAIL_PIECE), z, shade(color, life), false, Some('▌'));
        }
        let depth = crash.head - self.travel;
        for i in 0..80u32 {
            let h = i.wrapping_mul(2654435761);
            let share = |shift: u32| ((h >> shift) & 255) as f32 / 255. * 2. - 1.;
            let fly = t * (1.4 + ((h >> 26) & 63) as f32 / 12.);
            let x = (crash.x + share(2) * fly).clamp(-3.15, 3.15);
            let y = (1.7 + share(10) * fly - fly * fly * 0.12).clamp(-2.15, 2.15);
            let z = depth + share(18) * fly * 1.6;
            if z < 0.7 {
                continue;
            }
            let (px, py) = project(x, y, z);
            let tint = if i % 3 == 0 { (255, 250, 235) } else { color };
            let glyph = if t < 0.5 { '#' } else if i % 2 == 0 { '*' } else { '+' };
            // Shards keep their size on screen at any grid, and loom up close.
            let size = (rows as f32 / 130. * (6. / z).clamp(0.5, 1.8)) as i32;
            let wide = (size as f32 * aspect) as i32;
            for dy in 0..=size {
                for dx in 0..=wide {
                    raster.dot(px as i32 + dx - wide / 2, py as i32 + dy - size / 2, z - 0.1, glyph, shade(tint, life * 1.3));
                }
            }
        }
        if t < 0.35 && depth > 0.7 {
            let reach = t * 4.5 + 0.2;
            for segment in 0..24 {
                let (a, b) = (segment as f32 * 0.2618, (segment + 1) as f32 * 0.2618);
                raster.line(
                    project(crash.x + a.cos() * reach, 1.7 + a.sin() * reach * 0.8, depth),
                    project(crash.x + b.cos() * reach, 1.7 + b.sin() * reach * 0.8, depth),
                    depth - 0.2,
                    (255, 250, 235),
                    true,
                );
            }
        }
    }
}
