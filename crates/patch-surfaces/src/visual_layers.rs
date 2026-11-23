//! Adapt existing UI mirrors to the shared layer contract. No extra polling.
use crate::ui::{DjState, PatchWindow, VisualState};
use patch_visual::performance::{Frame, MAX_SOURCES, Scene, Settings, Source};
use slint::ComponentHandle;
use std::cell::Cell;

fn path() -> std::path::PathBuf {
    patch_engine::session::state_dir().join("visualiser-layers.json")
}
pub(crate) fn settings(w: &PatchWindow) -> Settings {
    let v = w.global::<VisualState>();
    let rgb = |c: slint::Color| {
        [
            c.red() as f32 / 255.0,
            c.green() as f32 / 255.0,
            c.blue() as f32 / 255.0,
        ]
    };
    Settings {
        decks: v.get_decks_enabled(),
        performer: v.get_performer_enabled(),
        deck_opacity: v.get_deck_opacity(),
        performer_opacity: v.get_performer_opacity(),
        appearance: if v.get_point_appearance() { 1.0 } else { 0.0 },
        automatic: v.get_auto_layers(),
        performer_first: v.get_performer_first(),
        director_colours: v.get_director_colours(),
        colours: [rgb(v.get_deck_colour_a()), rgb(v.get_deck_colour_b())],
        emblem: patch_visual::performance::Emblem::from_index(v.get_emblem()),
        me: v.get_performer_me(),
        look: [v.get_film_photo(), v.get_film_shadow(), v.get_film_points()],
        ribbons: v.get_ribbons(),
    }
    .sanitized()
}
#[derive(Default)]
pub struct Layers {
    morph: Cell<f32>,
    groove: Cell<patch_visual::groove::Groove>,
    shedding: Cell<patch_visual::performance::Shedding>,
    /// The layers as the DJ last set them (a change resets the shedding).
    chosen: Cell<(bool, bool)>,
    /// Seconds the last shedding note still shows.
    note_left: Cell<f32>,
    budget: std::cell::RefCell<patch_visual::performance::Budget>,
}
/// The saved layer settings (first run: the brand colours).
pub(crate) fn load() -> Settings {
    let mut saved = std::fs::read(path())
        .ok()
        .and_then(|s| serde_json::from_slice::<Settings>(&s).ok())
        .unwrap_or_default()
        .sanitized();
    if patch_visual::performance::first_defaults(saved.colours) {
        saved.colours = patch_visual::performance::BRAND;
    }
    saved
}
pub(crate) fn save(s: &Settings) {
    let dest = path();
    let temp = dest.with_extension("tmp");
    if let Some(dir) = dest.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(data) = serde_json::to_vec(s)
        && std::fs::write(&temp, data).is_ok()
    {
        let _ = std::fs::rename(temp, dest);
    }
}
impl Layers {
    pub fn bind(&self, w: &PatchWindow) {
        let saved = load();
        let v=w.global::<VisualState>();
        apply_settings(w,saved);
        self.morph.set(saved.appearance);
        let weak = w.as_weak();
        v.on_layers_changed(move || {
            if let Some(w) = weak.upgrade() {
                save(&settings(&w));
            }
        });
    }
    pub fn frame(&self, w: &PatchWindow, mut scene: Scene, dt: f32, gpu_ms: Option<f32>, plan:patch_visual::choreography::LayerPlan) -> Option<Frame> {
        let mut c = settings(w);
        if c.automatic {
            if plan.booth>0 {c.decks=plan.booth==1;}
            if plan.performer>0 {c.performer=plan.performer==1;}
            c.deck_opacity*=plan.opacity;c.performer_opacity*=plan.opacity;
            if plan.looks!=[1.0;3] {
                let sum: f32=plan.looks.iter().sum();
                if sum>0.0 {let mut pick=(scene.seed+scene.beat.unwrap_or(0.0).div_euclid(64.0)*0.618034).fract()*sum;let mut choice=2;
                    for (i,weight) in plan.looks.iter().enumerate() {pick-=weight;if pick<=0.0 {choice=i;break;}}
                    c.automatic=false;c.me=true;c.look=[0.0;3];c.look[match choice {0=>1,1=>2,_=>0}]=1.0;
                }
            }
        }
        let colours = if c.director_colours {
            scene.palette
        } else {
            c.colours
        };
        let color = |rgb: [f32; 3]| {
            slint::Color::from_rgb_u8(
                (rgb[0] * 255.0) as u8,
                (rgb[1] * 255.0) as u8,
                (rgb[2] * 255.0) as u8,
            )
        };
        let v = w.global::<VisualState>();
        v.set_signal_colour_a(color(colours[0]));
        v.set_signal_colour_b(color(colours[1]));
        // Each deck's title and its mix apart ("Kerala" · "Original Mix"),
        // for the track text beside the DJ.
        let d = w.global::<DjState>();
        for (i, title) in [d.get_deck_a().title, d.get_deck_b().title].into_iter().enumerate() {
            let (main, mix) = split_title(&title);
            let (now_main, now_mix) = if i == 0 {
                (v.get_a_title(), v.get_a_mix())
            } else {
                (v.get_b_title(), v.get_b_mix())
            };
            if now_main.as_str() != main || now_mix.as_str() != mix {
                if i == 0 {
                    v.set_a_title(main.into());
                    v.set_a_mix(mix.into());
                } else {
                    v.set_b_title(main.into());
                    v.set_b_mix(mix.into());
                }
            }
        }
        // On a GPU too slow for them, the layers step down (and back up) by
        // themselves; the DJ's setting stays as chosen, and changing it (D,
        // the panel) starts afresh.
        let mut shed = self.shedding.get();
        if self.chosen.replace((c.decks, c.performer)) != (c.decks, c.performer) {
            shed = Default::default();
        }
        let note = shed.advance(dt, c.decks || c.performer, gpu_ms);
        self.shedding.set(shed);
        if shed.level >= 1 {
            c.performer = false;
        }
        if shed.level >= 2 {
            c.decks = false;
        }
        if v.get_layers_shed() != i32::from(shed.level) {
            v.set_layers_shed(i32::from(shed.level));
        }
        if let Some(n) = note {
            v.set_layers_note(n.into());
            self.note_left.set(5.0);
        } else if self.note_left.get() > 0.0 {
            self.note_left.set(self.note_left.get() - dt.clamp(0.0, 0.25));
            if self.note_left.get() <= 0.0 {
                v.set_layers_note("".into());
            }
        }
        if !c.decks && !c.performer {
            return None;
        }
        let target = patch_visual::performance::appearance(c, &scene);
        let morph = self.morph.get()
            + (target - self.morph.get()) * (1.0 - (-dt.clamp(0.0, 0.25) * 3.0).exp());
        self.morph.set(morph);
        let d = w.global::<DjState>();
        scene.crossfader = d.get_xfader();
        let mut sources = [Source::default(); MAX_SOURCES];
        let law = patch_engine::session::crossfade(d.get_xfader());
        let fx_deck = d.get_fx_deck();
        for (i, (item, level, beat, eq, gain, weight)) in [
            (
                d.get_deck_a(),
                d.get_levels_a(),
                d.get_phase_a(),
                d.get_eq_a(),
                d.get_gain_a(),
                law.0,
            ),
            (
                d.get_deck_b(),
                d.get_levels_b(),
                d.get_phase_b(),
                d.get_eq_b(),
                d.get_gain_b(),
                law.1,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let active = item.loaded && item.playing;
            sources[i] = Source {
                active,
                // The fast channel levels, as the DJ surface's meters: the
                // slow VU reads nothing for a quiet deck in a blend.
                level: if active {
                    level.level_l.max(level.level_r).clamp(0.0, 1.0)
                } else {
                    0.0
                },
                meter: if active {
                    [level.level_l.clamp(0.0, 1.0), level.level_r.clamp(0.0, 1.0)]
                } else {
                    [0.0; 2]
                },
                beat: (active && beat.is_finite() && beat >= 0.0).then_some(beat),
                progress: item.progress.clamp(0.0, 1.0),
                gain,
                weight: if active && gain > 0.0 {
                    10.0_f32.powf(patch_engine::session::fader_db(gain) / 20.0) * weight
                } else {
                    0.0
                },
                eq: if d.get_eq_live() {
                    [eq.low, eq.mid, eq.high]
                } else {
                    [item.eq_low, item.eq_mid, item.eq_high]
                },
                filter: if d.get_eq_live() {
                    eq.filter
                } else {
                    item.filter
                },
                fx: if d.get_fx_on() && fx_deck.as_str() == if i == 0 { "a" } else { "b" } {
                    d.get_fx_depth()
                } else {
                    0.0
                },
            };
        }
        let mut groove = self.groove.get();
        let body = groove.advance(&scene, &sources[..2], dt);
        self.groove.set(groove);
        Some(Frame {
            scene,
            sources,
            count: 2,
            settings: c,
            appearance: morph,
            point_step: self.budget.borrow_mut().advance(dt),
            body,
        })
    }
}

/// "Kerala (Original Mix)" → ("Kerala", "Original Mix"); a title without a
/// mix in brackets stays whole.
pub fn split_title(title: &str) -> (&str, &str) {
    let t = title.trim();
    match (t.strip_suffix(')'), t.rfind(" (")) {
        (Some(_), Some(at)) => (t[..at].trim_end(), &t[at + 2..t.len() - 1]),
        _ => (t, ""),
    }
}

pub(crate) fn apply_settings(w:&PatchWindow,saved:Settings) {
        let v = w.global::<VisualState>();
        v.set_decks_enabled(saved.decks);
        v.set_performer_enabled(saved.performer);
        v.set_deck_opacity(saved.deck_opacity);
        v.set_performer_opacity(saved.performer_opacity);
        v.set_point_appearance(saved.appearance >= 0.5);
        v.set_auto_layers(saved.automatic);
        v.set_performer_first(saved.performer_first);
        v.set_director_colours(saved.director_colours);
        v.set_emblem(saved.emblem.index());
        v.set_performer_me(saved.me);
        v.set_ribbons(saved.ribbons);
        v.set_film_photo(saved.look[0]);
        v.set_film_shadow(saved.look[1]);
        v.set_film_points(saved.look[2]);
        let color = |c: [f32; 3]| {
            slint::Color::from_rgb_u8(
                (c[0] * 255.0) as u8,
                (c[1] * 255.0) as u8,
                (c[2] * 255.0) as u8,
            )
        };
        v.set_deck_colour_a(color(saved.colours[0]));
        v.set_deck_colour_b(color(saved.colours[1]));
}

#[cfg(test)]
mod tests {
    use super::split_title;
    #[test]
    fn titles_split_from_their_mix() {
        assert_eq!(split_title("Kerala (Original Mix)"), ("Kerala", "Original Mix"));
        assert_eq!(split_title("Two Thousand And Seventeen"), ("Two Thousand And Seventeen", ""));
        assert_eq!(split_title("A (B) (Extended Mix)"), ("A (B)", "Extended Mix"));
    }
}
