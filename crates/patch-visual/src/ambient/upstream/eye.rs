// Adapted from avillagran/omarchy-ttfx-layer, MIT, c969b6c172108c5f6ab937d99db67c17ddd28196.
// See ../LICENSE and ../README.md for provenance and integration notes.
use crate::ambient::raster::DirectCell;
use rand::Rng;
pub(crate) struct Eye {
    gaze: (f32, f32),
    until_blink: f32,
    blink: f32,
    time: f32,
    position: (f32, f32),
    destination: (f32, f32),
    until_move: f32,
}
impl Eye {
    pub(crate) fn new() -> Self {
        Self {
            gaze: (0., 0.),
            until_blink: 3.,
            blink: -1.,
            time: 0.,
            position: (0., 0.),
            destination: (0., 0.),
            until_move: 3.,
        }
    }
    pub(crate) fn step(&mut self, dt: f32, pointer: Option<(f32, f32)>, rng: &mut impl Rng) -> f32 {
        let dt = dt.clamp(0., 0.1);
        self.until_move -= dt;
        if self.until_move <= 0. {
            self.destination = (rng.gen_range(-0.20..0.20), rng.gen_range(-0.23..0.23));
            self.until_move = rng.gen_range(4.5..8.5);
            if self.blink < 0. {
                self.until_blink = 0.;
            }
        }
        let drift = 1. - (-dt * 1.35).exp();
        self.position.0 += (self.destination.0 - self.position.0) * drift;
        self.position.1 += (self.destination.1 - self.position.1) * drift;
        self.time = (self.time + dt) % (std::f32::consts::TAU * 100.);
        let target = pointer
            .map(|(x, y)| {
                (
                    (x - 0.5 - self.position.0) * 2.,
                    (y - 0.5 - self.position.1) * 2.,
                )
            })
            .unwrap_or((0., 0.));
        // A large redirected glance gets one protective blink, not a continuous flutter.
        let redirect = (target.0 - self.gaze.0).hypot(target.1 - self.gaze.1);
        if pointer.is_some() && redirect > 1.6 && self.blink < 0. && self.until_blink > 1. {
            self.until_blink = 0.;
        }
        let ease = 1. - (-dt * 12.).exp();
        self.gaze.0 += (target.0.clamp(-1., 1.) - self.gaze.0) * ease;
        self.gaze.1 += (target.1.clamp(-1., 1.) - self.gaze.1) * ease;
        if self.blink >= 0. {
            self.blink += dt;
            if self.blink >= 0.26 {
                self.blink = -1.;
                self.until_blink = rng.gen_range(2.8..6.0);
            }
        } else {
            self.until_blink -= dt;
            if self.until_blink <= 0. {
                self.blink = 0.;
            }
        }
        if self.blink < 0. {
            1.
        } else if self.blink < 0.09 {
            1. - self.blink / 0.09
        } else if self.blink < 0.12 {
            0.
        } else {
            ((self.blink - 0.12) / 0.14).clamp(0., 1.)
        }
    }
    pub(crate) fn draw(
        &self,
        cols: usize,
        rows: usize,
        aspect: f32,
        open: f32,
        audio: f32,
        cells: &mut Vec<DirectCell>,
    ) {
        cells.clear();
        let (cx, cy) = (
            cols as f32 * (0.5 + self.position.0),
            rows as f32 * (0.47 + self.position.1),
        );
        if cols == 0 || rows == 0 {
            return;
        }
        let (rx, ry) = (cols as f32 * 0.25, rows as f32 * 0.18);
        let (ix, iy) = (cx + self.gaze.0 * rx * 0.31, cy + self.gaze.1 * ry * 0.25);
        let radius = (ry * 0.78).max(1.);
        let aspect = aspect.clamp(1., 4.);
        let x0 = (cx - rx - 2.).max(0.) as usize;
        let x1 = ((cx + rx + 2.) as usize).min(cols);
        let y0 = (cy - ry - 2.).max(0.) as usize;
        let y1 = ((cy + ry + 2.) as usize).min(rows);
        let palette = b".:!ilt1LCG08@";
        // Backdrop: a slow starfield, drifting motes and a faint halo ring that
        // breathes with the audio. All of it stays dim and outside the eye.
        let inside = |x: usize, y: usize| {
            ((x as f32 + 0.5 - cx) / rx).abs() < 1.02 && ((y as f32 + 0.5 - cy) / ry).abs() < 1.1
        };
        let mut taken = std::collections::HashSet::new();
        let mut push = |x: f32, y: f32, ch: char, g: f32| {
            if x < 0. || y < 0. || x >= cols as f32 || y >= rows as f32 {
                return;
            }
            let (x, y) = (x as usize, y as usize);
            if inside(x, y) || !taken.insert((x, y)) {
                return;
            }
            let g = g.clamp(0., 255.) as u8;
            cells.push(DirectCell {
                x,
                y,
                ch,
                color: (g, g, g.saturating_sub(2)),
                background: None,
                attrs: 0,
            });
        };
        let energy = audio.clamp(0., 1.);
        for i in 0..110u32 {
            let h = i.wrapping_mul(2654435761);
            let x = ((h >> 3) & 4095) as f32 / 4096. * cols as f32;
            let y = ((h >> 15) & 4095) as f32 / 4096. * rows as f32;
            let twinkle = 0.5 + 0.5 * (self.time * (0.7 + (i % 5) as f32 * 0.3) + i as f32).sin();
            let g = 18. + twinkle * 30. + energy * 35. * twinkle;
            push(x, y, if twinkle > 0.92 { '+' } else { '.' }, g);
        }
        // Geometry that always drifts and turns, and whips around with music:
        // nested polygons orbiting the eye, and sine ribbons across the screen.
        let spin = self.time * 0.15 + energy * energy * 1.2;
        for ring in 0..3u32 {
            let sides = 3 + ring as usize * 2;
            let orbit = self.time * (0.08 + ring as f32 * 0.05) + ring as f32 * 2.1;
            let ox = cx + orbit.cos() * rx * (1.9 + ring as f32 * 0.35);
            let oy = cy + (orbit * 1.3).sin() * ry * (1.6 + ring as f32 * 0.3);
            let size = ry * (0.45 + ring as f32 * 0.2) * (1. + energy * 0.3);
            let turn = spin * (1. + ring as f32 * 0.4) * if ring % 2 == 0 { 1. } else { -1. };
            let g = 40. + energy * 70. + ring as f32 * 10.;
            for depth in 0..2 {
                let r = size * (1. - depth as f32 * 0.45);
                let t = turn + depth as f32 * 0.5;
                let steps = 26 + ring as usize * 6;
                for i in 0..sides {
                    let a = t + i as f32 / sides as f32 * std::f32::consts::TAU;
                    let b = t + (i + 1) as f32 / sides as f32 * std::f32::consts::TAU;
                    let (ax, ay) = (ox + a.cos() * r * aspect, oy + a.sin() * r);
                    let (bx, by) = (ox + b.cos() * r * aspect, oy + b.sin() * r);
                    for k in 0..=steps {
                        let f = k as f32 / steps as f32;
                        let ch = if depth == 0 { ':' } else { '.' };
                        push(
                            ax + (bx - ax) * f,
                            ay + (by - ay) * f,
                            ch,
                            g - depth as f32 * 15.,
                        );
                    }
                }
            }
        }
        for band in 0..3u32 {
            let base_y = rows as f32 * (0.12 + band as f32 * 0.38);
            let amp = rows as f32 * (0.02 + energy * 0.03) * (1. + band as f32 * 0.3);
            let speed = self.time * (0.6 + band as f32 * 0.3) * (1. + energy * 1.5);
            for x in (0..cols).step_by(2) {
                let ph = x as f32 / cols as f32 * std::f32::consts::TAU * (2. + band as f32);
                let y =
                    base_y + (ph + speed).sin() * amp + (ph * 2.3 - speed * 0.7).sin() * amp * 0.4;
                let g = 24. + energy * 55. + (ph * 3. + speed).sin().max(0.) * 20.;
                push(x as f32, y, '-', g);
            }
        }
        if open > 0.05 {
            let breathe = 1.28 + (self.time * 0.9).sin() * 0.03;
            for i in 0..160 {
                let a = i as f32 / 160. * std::f32::consts::TAU;
                let x = cx + a.cos() * rx * breathe;
                let y = cy + a.sin() * ry * breathe * 1.35;
                let g = 30. + energy * 50. + (a * 6. + self.time * 2.).sin().max(0.) * 16.;
                push(x, y, if i % 2 == 0 { '.' } else { ' ' }, g);
            }
        }
        for y in y0..y1 {
            for x in x0..x1 {
                let nx = ((x as f32 + 0.5 - cx) / rx).abs();
                if nx > 1. {
                    continue;
                }
                let lid = (1. - nx.powf(1.6)).max(0.) * ry * open;
                let dy = (y as f32 + 0.5 - cy).abs();
                let (ch, gray) = if (dy - lid).abs() < 0.60 {
                    ('_', 120.)
                } else if dy < lid {
                    let dx = (x as f32 + 0.5 - ix) / aspect;
                    let yy = y as f32 + 0.5 - iy;
                    let r = (dx * dx + yy * yy).sqrt() / radius;
                    let pupil =
                        0.24 + (self.time * 1.15).sin() * 0.012 + audio.clamp(0., 1.) * 0.32;
                    // Corneal glints move less than the iris, giving cursor-driven parallax.
                    let hx = cx + self.gaze.0 * rx * 0.12 - radius * aspect * 0.25;
                    let hy = cy + self.gaze.1 * ry * 0.08 - radius * 0.30;
                    let sx = (x as f32 + 0.5 - hx) / (radius * aspect);
                    let sy = (y as f32 + 0.5 - hy) / radius;
                    let glint = (-((sx / 0.20).powi(2) + (sy / 0.17).powi(2)) * 1.8).exp();
                    let secondary =
                        (-(((sx - 0.42) / 0.08).powi(2) + ((sy - 0.56) / 0.07).powi(2)) * 1.5)
                            .exp()
                            * 0.55;
                    let spec = glint.max(secondary);
                    if spec > 0.08 {
                        let gray = (140. + 115. * spec.sqrt() + audio * 30.).min(255.);
                        (if gray > 220. { '@' } else { '*' }, gray)
                    } else if r < pupil {
                        (' ', 0.)
                    } else if r < pupil + 0.08 {
                        ('#', 165. + audio * 70.)
                    } else if r < 1. {
                        let angle = yy.atan2(dx);
                        // Radial fibers remain attached to the iris; concentric folds add depth.
                        let fiber = ((angle * 39. + r * 17. + (angle * 13.).sin() * 1.4).sin()
                            * 0.34
                            + (angle * 73. - r * 9.).sin() * 0.16
                            + 0.5)
                            .clamp(0., 1.);
                        let rim = if r > 0.91 { 0.4 } else { 1. };
                        let gray = (65. + fiber * (65. + audio * 90.)) * rim;
                        (
                            palette[((fiber * 11.) as usize).min(palette.len() - 1)] as char,
                            gray,
                        )
                    } else {
                        let ex = (x as f32 + 0.5 - cx) / rx;
                        let ey = (y as f32 + 0.5 - cy) / (ry * open.max(0.15));
                        let z = (1. - ex * ex - ey * ey).max(0.).sqrt();
                        let diffuse = (-0.42 * ex - 0.55 * ey + 0.72 * z).max(0.);
                        let gray = 12. + diffuse.powf(1.7) * 135. + audio * 25. * z;
                        let dither = ((x * 13 + y * 7) % 7) as f32 * 0.018;
                        let idx = (((gray / 180. + dither).clamp(0., 1.))
                            * (palette.len() - 1) as f32)
                            as usize;
                        (palette[idx] as char, gray)
                    }
                } else {
                    continue;
                };
                let g = gray.clamp(0., 255.) as u8;
                cells.push(DirectCell {
                    x,
                    y,
                    ch,
                    color: (g, g, g.saturating_sub(3)),
                    background: None,
                    attrs: 1,
                });
            }
        }
        // A faint dotted sightline stays behind windows.
        if open > 0.2 && self.gaze.0.abs() + self.gaze.1.abs() > 0.1 {
            let tx = ((self.gaze.0 * 0.5 + 0.5 + self.position.0) * (cols - 1) as f32)
                .clamp(0., (cols - 1) as f32);
            let ty = ((self.gaze.1 * 0.5 + 0.5 + self.position.1) * (rows - 1) as f32)
                .clamp(0., (rows - 1) as f32);
            let dx = tx - ix;
            let dy = ty - iy;
            let steps = (dx.abs().max(dy.abs()) as usize).max(1);
            for i in (0..=steps).step_by(3) {
                let t = i as f32 / steps as f32;
                let x = (ix + dx * t).clamp(0., (cols - 1) as f32) as usize;
                let y = (iy + dy * t).clamp(0., (rows - 1) as f32) as usize;
                // Do not overwrite the eye with the guide line.
                if ((x as f32 - cx) / rx).abs() < 1. && ((y as f32 - cy) / ry).abs() < 1. {
                    continue;
                }
                cells.push(DirectCell {
                    x,
                    y,
                    ch: '.',
                    color: (35, 38, 36),
                    background: None,
                    attrs: 0,
                });
            }
            cells.push(DirectCell {
                x: tx as usize,
                y: ty as usize,
                ch: '+',
                color: (135, 145, 138),
                background: None,
                attrs: 0,
            });
        }
    }
}
