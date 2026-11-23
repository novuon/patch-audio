//! Ambient scenes adapted to Patch's existing cell stage. No desktop, capture or
//! clock service: the host supplies motion time, musical events and pointer.
mod horizon;
mod telemetry;
// Vendored upstream scene code: kept close to its source for diffing, so
// clippy's style and complexity rewrites are not applied there.
#[allow(clippy::style, clippy::complexity)]
mod upstream;
mod jellyfish;
mod journey;
mod raster;
mod text;

use crate::{
    grid::{Cell, Grid},
    stage::{Backdrop, Ctx},
};
use raster::{DirectCell, Raster};

#[derive(Clone, Copy, Default)]
struct Sound {
    bass: f32,
    treble: f32,
    beat: bool,
}
impl Sound {
    fn follow(&mut self, target: Self, dt: f32) {
        let mix = |old: f32, new: f32| {
            old + (new - old) * (1.0 - (-dt * if new > old { 24.0 } else { 8.0 }).exp())
        };
        self.beat = target.beat;
        self.bass = mix(self.bass, target.bass);
        self.treble = mix(self.treble, target.treble);
    }
}

enum Simulation {
    Sunset(Box<journey::Scene>),
    Jellyfish(jellyfish::Jellyfish),
    Horizon(horizon::Horizon),
    Other(Box<upstream::Scene>),
}
struct Entry {
    kind: Backdrop,
    seed: u64,
    time: f64,
    beat: Option<i64>,
    gaze: (f32, f32),
    simulation: Simulation,
    sound: Sound,
    raster: Raster,
    cells: Vec<DirectCell>,
    logical_size: Option<(usize,usize)>,
    painted:bool,
}
/// Bounded to the current and departing scenes (including overlays). Resize
/// changes only raster storage; it never recreates the world or its particles.
#[derive(Default)]
pub struct Scenes {
    entries: Vec<Entry>,
}
impl Scenes {
    pub fn draw(&mut self, kind: Backdrop, c: &Ctx, pointer: Option<(f32, f32)>) -> Grid {
        if !kind.ambient() {
            return crate::stage::backdrop(kind, c);
        }
        if c.cols == 0 || c.rows == 0 {
            return Grid::blank(c.cols, c.rows);
        }
        let key = self
            .entries
            .iter()
            .position(|e| e.kind == kind && e.seed == c.seed);
        let index = key.unwrap_or_else(|| {
            if self.entries.len() == 4 {
                self.entries.remove(0);
            }
            let seed = (c.seed ^ (c.seed >> 32)) as u32;
            let simulation = match kind {
                Backdrop::SunsetDrive => {
                    Simulation::Sunset(Box::new(journey::Scene::new(journey::Style::Sunset, seed)))
                }
                Backdrop::Jellyfish => Simulation::Jellyfish(jellyfish::Jellyfish::new(seed)),
                Backdrop::EventHorizon => Simulation::Horizon(horizon::Horizon::new(seed)),
                Backdrop::Ambient(name) => match name {
                    "neon-city"|"vector-highway"|"circuit-run" => Simulation::Sunset(Box::new(journey::Scene::new(journey::Style::parse(name).unwrap(),seed))),
                    _=>Simulation::Other(Box::new(upstream::Scene::new(name,seed)))
                },
                _=>unreachable!("native Ambient kind"),
            };
            self.entries.push(Entry {
                kind,
                seed: c.seed,
                time: c.t,
                beat: None,
                gaze: (0.0, 0.0),
                simulation,
                sound: Sound::default(),
                raster: Raster::new(c.cols, c.rows),
                cells: vec![],
                logical_size:None,painted:false,
            });
            self.entries.len() - 1
        });
        let e = &mut self.entries[index];
        let dt = (c.t - e.time).clamp(0.0, 0.1) as f32;
        e.time = c.t;
        let clean = |v: f32| {
            if v.is_finite() {
                v.clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        // Musical impulses are supplied by Patch. Continuous bands retain the
        // original look, while the common gate follows silence/breakdowns.
        let beat = (c.gate > 0.0 && c.music_beat.is_finite() && c.music_beat >= 0.0)
            .then_some(c.music_beat.floor() as i64);
        let hit = dt > 0.0 && beat.is_some_and(|b| e.beat.is_some_and(|last| b > last));
        e.beat = beat;
        let mut sound = Sound {
            bass: clean(c.bass),
            treble: clean(c.high),
            beat: hit,
        };
        if matches!(&e.simulation,Simulation::Sunset(_)|Simulation::Other(_)) {
            // Journey supplies its own soft gain and envelope.
            e.sound = sound;
        } else {
            let drive = |v: f32| 1.0 - (-3.0 * v).exp();
            sound.bass = drive(sound.bass);
            sound.treble = drive(sound.treble);
            e.sound.follow(sound, dt);
        }
        let pointer = pointer
            .filter(|(x, y)| x.is_finite() && y.is_finite())
            .map(|(x, y)| (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)));
        let target = pointer
            .map(|(x, y)| (x * 2.0 - 1.0, y * 2.0 - 1.0))
            .unwrap_or((0.0, 0.0));
        let ease = 1.0 - (-dt * 4.0).exp();
        e.gaze.0 += (target.0 - e.gaze.0) * ease;
        e.gaze.1 += (target.1 - e.gaze.1) * ease;
        let gaze = pointer.map(|_| e.gaze);
        let aspect = 2.0;
        if !e.raster.fits(c.cols, c.rows) {
            e.raster = Raster::new(c.cols, c.rows);
        }
        match &mut e.simulation {
            Simulation::Sunset(s) => {
                s.step(dt, pointer, e.sound);
                s.draw(c.cols, c.rows, aspect, &mut e.cells);
            }
            Simulation::Jellyfish(s) => {
                s.step(dt, &e.sound, gaze);
                s.draw(&mut e.raster, c.cols, c.rows, aspect, &e.sound);
                e.raster.recycle(&mut e.cells);
            }
            Simulation::Other(s) => {
                let (cols,rows)=*e.logical_size.get_or_insert((c.cols,c.rows));
                if dt>0.0 || !e.painted {s.draw(dt,pointer,e.sound,cols,rows,&mut e.cells);e.painted=true;}
            }
            Simulation::Horizon(s) => {
                s.step(dt, &e.sound, gaze);
                s.paint(&mut e.raster, c.cols, c.rows, aspect, &e.sound, e.gaze);
                e.raster.recycle(&mut e.cells);
            }
        }
        let (cols,rows)=e.logical_size.unwrap_or((c.cols,c.rows));
        let mut g = Grid::blank(cols,rows);
        let brightness = c.bright.clamp(0.0, 2.0);
        for cell in &e.cells {
            if cell.x < cols && cell.y < rows {
                let rgb = [cell.color.0, cell.color.1, cell.color.2]
                    .map(|v| (f32::from(v) * brightness).min(255.0) as u8);
                g.cells[cell.y * cols + cell.x] = Cell {
                    ch: cell.ch,
                    fg: Some(rgb),
                    bg: None,
                };
            }
        }
        let g=if (cols,rows)!=(c.cols,c.rows) {
            let mut scaled=Grid::blank(c.cols,c.rows);
            for y in 0..c.rows {for x in 0..c.cols {scaled.cells[y*c.cols+x]=g.cells[(y*rows/c.rows)*cols+x*cols/c.cols];}}
            scaled
        }else{g};
        // Keep active worlds at the recent end: a newly visited scene can
        // evict an inactive world, never the one drawing every frame.
        let entry = self.entries.remove(index);
        self.entries.push(entry);
        g
    }
}

/// The clocks display local civil time; tests can supply a stable clock
/// through the scene fixture rather than depending on frame pacing.
#[cfg(not(test))]
fn clock_hms()->(u32,u32,u32) {
    unsafe {let time=libc::time(std::ptr::null_mut());let mut value:libc::tm=std::mem::zeroed();libc::localtime_r(&time,&mut value);(value.tm_hour as u32,value.tm_min as u32,value.tm_sec as u32)}
}

#[cfg(test)]
fn clock_hms()->(u32,u32,u32) {(12,34,56)}

#[cfg(test)]
mod tests {
    use super::*;
    fn ctx(t: f64) -> Ctx<'static> {
        Ctx {
            cols: 160,
            rows: 45,
            t,
            beat: t * 2.0,
            music_beat: t * 2.0,
            age: t,
            span: 32.0,
            ease: crate::Easing::InOutSine,
            size: 1,
            place: 0,
            bright: 1.0,
            fonts: None,
            font: 0,
            style: 0,
            gate: 1.0,
            energy: 0.7,
            bass: 0.6,
            mid: 0.4,
            high: 0.3,
            bands: &[],
            history: &[],
            pal: &crate::pack::PALETTES[0],
            title: "Ambient",
            artist: "Patch",
            seed: 42,
        }
    }
    #[test]
    fn worlds_are_seeded_animate_and_keep_state_through_resize() {
        for &kind in Backdrop::AMBIENT {
            let (mut a, mut b) = (Scenes::default(), Scenes::default());
            let first = a.draw(kind, &ctx(0.0), None);
            assert!(
                first.cells.iter().filter(|c| c.ch != ' ').count() > 50,
                "{} paints",
                kind.name()
            );
            let mut last = Grid::default();
            for i in 0..60 {
                let c = ctx(f64::from(i) / 30.0);
                last = a.draw(kind, &c, None);
                assert_eq!(
                    last.cells,
                    b.draw(kind, &c, None).cells,
                    "{} replays",
                    kind.name()
                );
            }
            assert_ne!(first.cells, last.cells, "{} moves", kind.name());
            let c = ctx(59.0 / 30.0);
            let resized = Ctx {
                cols: 240,
                rows: 68,
                ..c
            };
            assert_eq!(a.draw(kind, &resized, None).cells.len(), 240 * 68);
            assert_eq!(
                a.draw(kind, &c, None).cells,
                last.cells,
                "resize preserves {}",
                kind.name()
            );
            assert_eq!(a.entries.len(), 1);
        }
    }
    #[test]
    fn sound_and_local_pointer_affect_each_scene() {
        for &kind in Backdrop::AMBIENT {
            let (mut quiet, mut live, mut pointer) =
                (Scenes::default(), Scenes::default(), Scenes::default());
            let (mut a, mut b, mut p) = (Grid::default(), Grid::default(), Grid::default());
            for i in 0..90 {
                let c = ctx(f64::from(i) / 30.0);
                a = quiet.draw(
                    kind,
                    &Ctx {
                        bass: 0.0,
                        high: 0.0,
                        gate: 0.0,
                        ..c
                    },
                    None,
                );
                b = live.draw(kind, &c, None);
                p = pointer.draw(kind, &c, Some(if i<70 {(0.5+0.35*(i as f32*0.27).sin(), 0.5+0.25*(i as f32*0.19).cos())}else{(0.7,0.3)}));
            }
            assert_ne!(a.cells, b.cells, "{} hears the music", kind.name());
            assert_ne!(b.cells, p.cells, "{} follows the pointer", kind.name());
        }
    }
    #[test]
    fn simulation_cache_is_bounded_and_bad_input_is_safe() {
        let mut scenes = Scenes::default();
        for seed in 0..20 {
            scenes.draw(Backdrop::Jellyfish, &Ctx { seed, ..ctx(1.0) }, None);
        }
        assert_eq!(scenes.entries.len(), 4);
        let c = Ctx {
            bass: f32::NAN,
            high: f32::INFINITY,
            ..ctx(2.0)
        };
        assert_eq!(
            scenes
                .draw(Backdrop::EventHorizon, &c, Some((f32::NAN, 0.0)))
                .cells
                .len(),
            160 * 45
        );
        assert!(
            scenes
                .draw(
                    Backdrop::SunsetDrive,
                    &Ctx {
                        cols: 0,
                        rows: 0,
                        ..c
                    },
                    None
                )
                .cells
                .is_empty()
        );
    }
    #[test]
    fn continuous_loudness_does_not_invent_beats() {
        let mut scenes = Scenes::default();
        let kind = Backdrop::SunsetDrive;
        for t in [0.0, 0.03, 0.06, 0.09] {
            scenes.draw(
                kind,
                &Ctx {
                    music_beat: 0.2,
                    ..ctx(t)
                },
                None,
            );
            assert!(!scenes.entries[0].sound.beat);
        }
        scenes.draw(
            kind,
            &Ctx {
                music_beat: 1.01,
                ..ctx(0.12)
            },
            None,
        );
        assert!(scenes.entries[0].sound.beat);
        scenes.draw(
            kind,
            &Ctx {
                music_beat: 1.01,
                ..ctx(0.12)
            },
            None,
        );
        assert!(
            !scenes.entries[0].sound.beat,
            "a repeated frame never retriggers a beat"
        );
    }
    #[test]
    fn scene_cost_at_the_visualisers_largest_grid() {
        for &kind in Backdrop::AMBIENT {
            let mut scenes = Scenes::default();
            let mut times = Vec::new();
            for i in 0..90 {
                let c = Ctx {
                    cols: 480,
                    rows: 135,
                    ..ctx(f64::from(i) / 60.0)
                };
                let start = std::time::Instant::now();
                std::hint::black_box(scenes.draw(kind, &c, None));
                times.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            times.sort_by(f64::total_cmp);
            eprintln!(
                "Ambient {} 480x135 CPU: median {:.3} ms, p95 {:.3} ms",
                kind.name(),
                times[45],
                times[85]
            );
        }
    }
}

