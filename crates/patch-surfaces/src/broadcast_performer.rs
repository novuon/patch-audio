//! Patch AI DJ's identity is a live point field, generated on the GPU.
//! The engine's mix state and the existing director's scene cue are inputs;
//! this module never plays media, loads an image, or changes audio controls.
use crate::ui::{BroadcastDjState, DjState, PatchWindow};
use crate::visual_gl::{Frame, GenFrame, PointField, Shared};
use patch_visual::{generative::Compositor, grid::Grid, render::Look};
use slint::ComponentHandle;
use std::{cell::Cell, rc::Rc};

const VERTEX: &str = include_str!("../ui/broadcast-performer.vert");
const FRAGMENT: &str = include_str!("../ui/broadcast-performer.frag");
const POINTS: i32 = 144_000;

pub struct View {
    window: slint::Weak<PatchWindow>,
    shared: Rc<Shared>,
    vertex: Rc<String>,
    fragment: Rc<String>,
    last: Cell<f64>,
    scene: Cell<[f32; 3]>,
    initial_fullscreen: Cell<bool>,
}

impl View {
    pub fn bind(w: &PatchWindow, frozen: bool) -> Self {
        let shared = Rc::new(Shared::default());
        if frozen || !crate::visual_gl::install(w.window(), shared.clone()) {
            shared.failed.set(true);
        }
        Self {
            window: w.as_weak(),
            shared,
            vertex: Rc::new(crate::visual_gl::performer_source(VERTEX)),
            fragment: Rc::new(FRAGMENT.to_string()),
            last: Cell::new(f64::NEG_INFINITY),
            scene: Cell::new([0.23, 0.5, 0.5]),
            initial_fullscreen: Cell::new(false),
        }
    }

    pub fn alive(&self) -> bool {
        self.window.upgrade().is_some()
    }

    pub fn tick(&self, t: f64, frozen: bool) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let state = w.global::<BroadcastDjState>();
        let ready = !frozen
            && self.shared.ready.get()
            && !self.shared.failed.get()
            && self.shared.gen_error.borrow().is_none();
        state.set_performer_gpu(ready);
        state.set_performer_note(
            if frozen {
                "GPU GENERATIVE STAGE · HEADLESS DATA PREVIEW"
            } else if ready {
                ""
            } else if self.shared.failed.get() || self.shared.gen_error.borrow().is_some() {
                "GPU stage unavailable"
            } else {
                "Preparing generative stage…"
            }
            .into(),
        );
        if !ready {
            return;
        }
        if !self.initial_fullscreen.replace(true) {
            w.window().set_fullscreen(true);
        }
        let dt = if self.last.get().is_finite() {
            (t - self.last.get()).clamp(0.0, 0.1) as f32
        } else {
            1.0 / 60.0
        };
        self.last.set(t);
        let target = [
            state.get_scene_seed(),
            state.get_motion(),
            state.get_density(),
        ];
        let old = self.scene.get();
        let k = 1.0 - (-dt * 3.0).exp();
        let scene = std::array::from_fn::<_, 3, _>(|i| old[i] + (target[i] - old[i]) * k);
        self.scene.set(scene);
        let d = w.global::<DjState>();
        let a = d.get_deck_a();
        let b = d.get_deck_b();
        let la = d.get_levels_a();
        let lb = d.get_levels_b();
        let lead = if a.playing && (!b.playing || la.vu >= lb.vu) {
            d.get_phase_a()
        } else {
            d.get_phase_b()
        };
        let energy = la.vu.max(lb.vu).clamp(0.0, 1.0);
        let size = w.window().size();
        let look = Look {
            bg: [2, 3, 5],
            text: [0; 3],
            pulse: 0.0,
            glow: 0.24,
            bump: 0.0,
            split: 0.0,
            shake: 0.0,
        };
        let grid = Grid::blank(1, 1);
        let mut frame = Frame::from_grid(
            &grid,
            look,
            size.width as f32,
            [0, 0, size.width as i32, size.height as i32],
            &mut self.shared.slots.borrow_mut(),
        );
        frame.render_cap = Some(1280);
        let ca = state.get_stage_a();
        let cb = state.get_stage_b();
        frame.generative = Some(GenFrame {
            key: 0x5041_5443_4800_0002,
            frag: self.fragment.clone(),
            uniforms: vec![
                ("beat".into(), lead),
                ("energy".into(), energy),
                ("blend".into(), d.get_xfader()),
                ("seed".into(), scene[0]),
                ("motion".into(), scene[1]),
                ("density".into(), scene[2]),
                ("chapter".into(), state.get_scene_progress()),
                ("section".into(), state.get_section_id()),
                ("handover".into(), state.get_handover()),
                (
                    "low_a".into(),
                    if d.get_eq_live() {
                        d.get_eq_a().low
                    } else {
                        a.eq_low
                    },
                ),
                (
                    "low_b".into(),
                    if d.get_eq_live() {
                        d.get_eq_b().low
                    } else {
                        b.eq_low
                    },
                ),
                ("ar".into(), ca.red() as f32 / 255.0),
                ("ag".into(), ca.green() as f32 / 255.0),
                ("ab".into(), ca.blue() as f32 / 255.0),
                ("br".into(), cb.red() as f32 / 255.0),
                ("bg".into(), cb.green() as f32 / 255.0),
                ("bb".into(), cb.blue() as f32 / 255.0),
            ],
            time: t as f32,
            compositor: Compositor {
                generative_opacity: 1.0,
                cells_opacity: 0.0,
                mask: false,
                ..Default::default()
            },
            cells: false,
            points: Some(PointField {
                vertex: self.vertex.clone(),
                count: POINTS,
            }),
        });
        *self.shared.frame.borrow_mut() = Some(frame);
        w.window().request_redraw();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn identity_is_generated_geometry_without_media_inputs() {
        // User decision: never replace the identity with an image-warp or
        // video loop. The identity program has geometry and signal inputs.
        assert!(super::VERTEX.contains("gl_VertexID"));
        assert!(super::FRAGMENT.contains("gl_PointCoord"));
        assert!(!super::VERTEX.contains("sampler"));
        assert!(!super::FRAGMENT.contains("sampler"));
    }
}
