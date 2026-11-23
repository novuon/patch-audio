//! Visualiser Unit: the music drives patch-visual's director; our own
//! backdrops and acts are drawn here every frame, at the window's own
//! resolution, with a glow and a lift on the beat.

use std::cell::{Cell, RefCell};

use patch_visual::choreography::{self as show, Material};
use patch_visual::generative::Mode;
use patch_visual::director::{Director, Handover, Mood, Music};
use patch_visual::pack::Palette;
use patch_visual::render::{Glyphs, Look};
use patch_visual::stage::{self, Act, Backdrop};

/// Longest side drawn (px): a 4K screen at its own resolution (a frame
/// takes ~9 ms on all cores); only beyond that is it scaled up.
const CAP: f32 = 4096.0;

#[derive(Default)]
pub struct Panel {
    /// The window drawing on its GPU (by window), once its GL is ready.
    gpu: RefCell<Option<(usize, std::rc::Rc<crate::visual_gl::Shared>)>>,
    director: RefCell<Director>,
    steering: RefCell<patch_visual::steering::Steering>,
    status_written: Cell<f64>,
    glyphs: RefCell<Option<Glyphs>>,
    /// The system's faces for the titles (loaded once).
    fonts: std::cell::OnceCell<patch_visual::fonts::Fonts>,
    last_t: Cell<f64>,
    broadcast_tick: Cell<f64>,
    /// The last frame drawn while nothing plays (idle runs at 20 fps).
    idle_tick: Cell<f64>,
    broadcast_published: Cell<f64>,
    scene_at: Cell<f64>,
    skip: Cell<bool>,
    held: Cell<bool>,
    caption: RefCell<String>,
    /// The AI DJ's state, polled a few times a second.
    ai: RefCell<Option<patch_core::aivis::VisualState>>,
    ai_polled: Cell<f64>,
    ai_polls: crate::poll::Poller,
    scene_polls: crate::poll::Poller,
    scene_polled: Cell<f64>,
    scene_sequence: Cell<u64>,
    requested_scene: RefCell<Option<(Mood, usize)>>,
    /// The scene on stage: its backdrop and act, and when it ends.
    current: RefCell<Option<Current>>,
    previous: RefCell<Option<(Current, f64)>>,
    /// Loudness over the last seconds (the skyline).
    history: RefCell<std::collections::VecDeque<f32>>,
    sampled: Cell<f64>,
    /// The scene's length in beats.
    span: Cell<f64>,
    /// How much the beat hits now, followed (0 silence or no kick … 1).
    gate: Cell<f32>,
    /// The generative layer (modes, scene, compiler, signals).
    generative: crate::visual_gen::Gen,
    layers: crate::visual_layers::Layers,
    ambient: RefCell<patch_visual::ambient::Scenes>,
    controls: std::rc::Rc<crate::visual_control::Control>,
    accents: RefCell<show::AccentEnvelope>,
    manual_motion: RefCell<show::Motion>,
    /// A backdrop under another Unit (Tidal's code): the same show, but it
    /// doesn't speak for the Visualiser (no status file).
    backdrop: bool,
}

#[derive(Clone)]
struct Current {
    material_id: String,
    shader: Option<String>,
    mode: Mode,
    mood: Mood,
    enabled: bool,
    transition: Option<u64>,
    /// Its canvas's cell scale (smaller cells for real type).
    k: f32,
    backdrop: Backdrop,
    /// A visual scene's second backdrop, over the first.
    overlay: Option<Backdrop>,
    act: Act,
    /// The song's motion curve.
    ease: patch_visual::Easing,
    /// The title's size and place, the backdrop's brightness.
    size: u8,
    place: u8,
    bright: f32,
    font: u8,
    style: u8,
    /// Its beat feel.
    beat: patch_visual::beat::Beat,
    title: String,
    artist: String,
    palette: Palette,
    punch: f32,
    glow: f32,
    seed: u64,
    world_seed: u64,
    /// When it began (in beats; seconds × 2 without a grid) and ends (s).
    began_beat: f64,
    began_motion: f64,
    timed_title: bool,
    ends: f64,
    cols: usize,
    rows: usize,
}

/// Cell geometry depends only on aspect ratio and the scene's type scale.
fn stage_size(width: f32, height: f32, k: f32) -> (usize, usize) {
    let cols = (240.0 / k).round().clamp(24.0, 480.0) as usize;
    let rows = (cols as f32 * height / width.max(1.0) / 2.0)
        .round()
        .clamp(8.0, 200.0) as usize;
    (cols, rows)
}

fn rgb(c: slint::Color) -> [u8; 3] {
    [c.red(), c.green(), c.blue()]
}

impl Panel {
    /// The Visualiser as a backdrop under another Unit's window.
    pub fn backdrop() -> Panel {
        Panel {
            backdrop: true,
            ..Panel::default()
        }
    }

    pub fn bind(self: &std::rc::Rc<Self>, w: &crate::ui::PatchWindow) {
        use slint::ComponentHandle;
        if w.get_surface() == "visualiser" { self.layers.bind(w); }
        let v = w.global::<crate::ui::VisualState>();
        if w.get_surface() == "visualiser" { self.controls.bind(w); }
        if w.get_surface() == "visualiser-controls" {
            self.controls.bind(w);
            // Layer switches edited here reach every Visualiser window.
            crate::visual_layers::apply_settings(w, crate::visual_layers::load());
            let weak = w.as_weak();
            let me = self.clone();
            v.on_layers_changed(move || {
                if let Some(cw) = weak.upgrade() {
                    let s = crate::visual_layers::settings(&cw);
                    crate::visual_layers::save(&s);
                    for vw in me.controls.viewers() {
                        crate::visual_layers::apply_settings(&vw, s);
                    }
                }
            });
        }
        let me = self.clone();
        v.on_next(move || {
            me.controls.next(false);
            me.held.set(false);
            me.skip.set(true);
        });
        let me = self.clone();
        v.on_hold(move || me.held.set(!me.held.get()));
        let me = self.clone();
        v.on_cycle_mode(move || me.generative.cycle_mode());
        let me = self.clone();
        v.on_next_scene(move || me.generative.next_scene());
        let win = w.as_weak();
        v.on_toggle_fullscreen(move || {
            if let Some(w) = win.upgrade() {
                let full = w.window().is_fullscreen();
                w.window().set_fullscreen(!full);
            }
        });
    }

    /// Nothing drawn any more (the backdrop it was is off): the GPU stops
    /// drawing the last frame beneath the window.
    pub fn blank(&self) {
        if let Some((_, s)) = self.gpu.borrow().as_ref() {
            s.frame.borrow_mut().take();
        }
    }

    /// The AI DJ's transition, when one is ready or running: where it is
    /// on the outgoing deck's beat clock, and what comes in.
    fn handover(&self, t: f64) -> Option<Handover> {
        if t - self.ai_polled.get() >= 0.25 || t < self.ai_polled.get() {
            self.ai_polled.set(t);
            self.ai_polls
                .request("ai", serde_json::json!({"op": "ai_state"}));
        }
        if let Some(reply) = self.ai_polls.take("ai")
            && let Ok(value) = reply.result
        {
            *self.ai.borrow_mut() = serde_json::from_value(value).ok();
        }
        let ai = self.ai.borrow();
        patch_visual::performance::handover_at(ai.as_ref()?, patch_engine::now_ns())
    }

    /// Running while a window shows it; the show stops with the last one.
    pub fn tick(&self, wins: &[crate::ui::PatchWindow], t: f64, m: &Music, frozen: bool) {
        use slint::ComponentHandle;
        let Some(w) = wins.first() else { return };
        // Performer consumes this director's section/scene intent. Its own
        // image stage needs no duplicate character-grid or full-size raster.
        let broadcast_only = w.get_surface() == "performer";
        if broadcast_only && !frozen && t >= self.broadcast_tick.get()
            && t - self.broadcast_tick.get() < 0.05 { return; }
        self.broadcast_tick.set(t);
        // Nothing plays and nothing sounds: the idle show moves at 16 %
        // speed, so 20 frames a second look as smooth as 60, for a third of
        // the work (60 fps kept the UI thread at ~22 % of a core).
        if !frozen && !m.playing && m.level < 0.001 {
            if t >= self.idle_tick.get() && t - self.idle_tick.get() < 0.05 {
                return;
            }
            self.idle_tick.set(t);
        }
        let mut m = m.clone();
        if !frozen {
            m.handover = self.handover(t);
        }
        let m = &m;
        let dt = (t - self.last_t.get()).clamp(0.0, 0.25);
        self.last_t.set(t);
        let v = w.global::<crate::ui::VisualState>();
        // The compositor's true fullscreen (not windowed): the animation
        // alone.
        let full = w.window().is_fullscreen();
        if v.get_bare() != full {
            v.set_bare(full);
        }
        let th = w.global::<crate::ui::Theme>();
        let scale = w.window().scale_factor();
        let (lw, lh) = (v.get_view_w().max(1.0), v.get_view_h().max(1.0));
        // The effect's canvas: cells of about a terminal's size, smaller
        // (down to half) until the title's letters get square pixels.
        let title = if m.playing && !m.title.is_empty() {
            m.title.as_str()
        } else {
            "PATCH"
        };
        // Pixels drawn: the window's (capped), and whole cells across it,
        // so the canvas covers it edge to edge (a cell count from the
        // logical size left a border of up to a cell each side).
        // Above 2560 px wide: half the pixels, scaled up 2× sharp (whole
        // pixels per cell, so edges stay crisp; a quarter of the work: 4K
        // at 60 fps kept a core and more busy, and under a full machine
        // the audio device could miss a cycle).
        let big = lw * scale > 2560.0;
        let kcap = (CAP / (lw.max(lh) * scale)).min(1.0) * if big { 0.5 } else { 1.0 };
        let (pw, ph) = (
            ((lw * scale * kcap) as usize).max(24),
            ((lh * scale * kcap) as usize).max(16),
        );
        // A logical stage, independent of output pixels: the same aspect
        // ratio gives the same show in a window or fullscreen. GPU/CPU scale
        // the entire grid to the available viewport.
        let canvas = |k: f32| stage_size(lw, lh, k);
        let block_k = [1.0f32, 0.85, 0.72, 0.6, 0.5]
            .iter()
            .copied()
            .find(|&k| {
                let (c, r) = canvas(k);
                patch_visual::banner::fits_big(title, c.saturating_sub(4), r.saturating_sub(4))
            })
            .unwrap_or(0.5);
        // Real type needs finer cells than block letters: half theirs.
        let type_k = block_k * 0.5;

        let mut glyphs = self.glyphs.borrow_mut();
        let glyphs = glyphs.get_or_insert_with(|| Glyphs::load(th.get_font().as_str()));
        let mut d = self.director.borrow_mut();
        if t - self.scene_polled.get() >= 0.25 || t < self.scene_polled.get() {
            self.scene_polled.set(t);
            self.scene_polls.request("visual-scene", serde_json::json!({"op":"visual_scene_cue","action":"get"}));
        }
        if let Some(reply) = self.scene_polls.take("visual-scene") && let Ok(value) = reply.result {
            let sequence = value["sequence"].as_u64().unwrap_or(0);
            if sequence > self.scene_sequence.get() {
                self.scene_sequence.set(sequence);
                if value["clear"].as_bool() == Some(true) {
                    self.requested_scene.borrow_mut().take();
                } else if let (Some(part), Some(index)) = (value["part"].as_str(), value["index"].as_u64()) {
                    let requested_ms = value["requested_ms"].as_u64().unwrap_or(0);
                    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d|d.as_millis() as u64);
                    if now_ms.saturating_sub(requested_ms) <= 10_000 {
                        let mood = match part { "text" => Mood::Reveal, "lift" => Mood::Lift, "calm" => Mood::Calm, _ => Mood::Groove };
                        *self.requested_scene.borrow_mut() = Some((mood, index as usize));
                    }
                }
            }
        }
        let preferences = self.controls.preferences();
        if !self.backdrop && !broadcast_only {
            let ready=self.gpu.borrow().as_ref().is_some_and(|(_,s)| s.ready.get() && !s.failed.get());
            d.configure(&preferences,&self.controls.active(),ready);
            if self.controls.clear_hold.replace(false) {self.held.set(false);}
            if let Some((layers,generative,held))=self.controls.profile.borrow_mut().take() {
                crate::visual_layers::apply_settings(w,layers);
                self.generative.apply(generative);
                self.held.set(held);
                self.skip.set(true);
            }
            self.controls.refresh();
        }
        d.listen(m, dt);
        let layer_posted = if frozen { None } else { self.generative.posted_intent(t) };
        let posted = layer_posted
            .filter(|_| self.generative.mode() != patch_visual::generative::Mode::Generative);
        let mut steering = self.steering.borrow_mut();
        steering.advance(posted, d.motion().seconds, d.motion().beats, dt, m.bpm);
        d.steer(steering.intent());
        let steering = RefCell::new(steering);
        let theme = patch_visual::pack::from_theme(
            rgb(th.get_accent()),
            rgb(th.get_text_bright()),
            rgb(th.get_bg_deeper()),
            0.08,
        );
        let beat_now = if m.playing && m.beat >= 0.0 {
            m.beat
        } else {
            t * 2.0
        };
        let scene_for = |d: &mut Director, mood: Mood| {
            let cue = d.cue(m, mood, t, theme);
            let k = if cue.font > 0 && cue.act != Act::Visual {
                type_k
            } else {
                block_k
            };
            let (cols, rows) = canvas(k);
            let secs = cue.fit_s.unwrap_or(8.0);
            let bps = if m.playing && m.beat >= 0.0 && m.bpm > 0.0 {
                m.bpm / 60.0
            } else {
                2.0
            };
            (
                Current {
                    material_id: cue.material_id,
                    shader: cue.shader,
                    mode: cue.mode,
                    mood,
                    enabled: cue.enabled,
                    transition: cue.transition,
                    k,
                    backdrop: cue.backdrop,
                    overlay: cue.overlay,
                    act: cue.act,
                    ease: cue.ease,
                    size: cue.size,
                    place: cue.place,
                    bright: cue.bright,
                    font: cue.font,
                    style: cue.style,
                    beat: cue.beat,
                    title: cue.title.clone(),
                    artist: cue.small.clone(),
                    palette: cue.palette,
                    punch: cue.punch,
                    glow: cue.glow,
                    seed: cue.seed,
                    world_seed: cue.world_seed,
                    began_beat: beat_now,
                    began_motion: steering.borrow().clocks(d.motion().seconds, d.motion().beats).1,
                    timed_title: mood == Mood::Handover,
                    ends: t + secs,
                    cols,
                    rows,
                },
                secs * bps,
            )
        };
        // The spectrum's three bands, 0…1.
        let bands: Vec<f32> = m.bands.clone();
        let part = |a: f32, b: f32| -> f32 {
            if bands.is_empty() {
                return m.level;
            }
            let (i, j) = (
                (a * bands.len() as f32) as usize,
                ((b * bands.len() as f32) as usize).max((a * bands.len() as f32) as usize + 1),
            );
            bands[i..j.min(bands.len())].iter().sum::<f32>()
                / (j.min(bands.len()) - i).max(1) as f32
        };
        let (bass, mid, high) = (part(0.0, 0.2), part(0.3, 0.65), part(0.7, 1.0));
        let hist: Vec<f32> = {
            let mut h = self.history.borrow_mut();
            if t - self.sampled.get() >= 1.0 / 30.0 {
                self.sampled.set(t);
                h.push_back(if m.playing { m.level } else { 0.0 });
                while h.len() > 120 {
                    h.pop_front();
                }
            }
            h.iter().copied().collect()
        };
        let fonts = self.fonts.get_or_init(patch_visual::fonts::Fonts::load);
        // The beat hits only when a kick is heard: not in silence, not in
        // a beatless stretch (a breakdown); it fades in and out.
        let heard = d.motion().beat_strength(m.level);
        let want = if m.playing && !d.motion().calm() && m.beat >= 0.0 {
            heard
        } else {
            0.0
        };
        let gate = self.gate.get() + (want - self.gate.get()) * (1.0 - (-dt / 0.3).exp()) as f32;
        self.gate.set(gate);
        let (motion_t, motion_beat) = steering.borrow().clocks(d.motion().seconds, d.motion().beats);
        let mood=d.mood(m);
        let rule=self.current.borrow().as_ref().map(|c|self.controls.response(&c.material_id)).unwrap_or_default();
        let (motion_t,motion_beat)=if self.backdrop || broadcast_only {(motion_t,motion_beat)} else {
            self.manual_motion.borrow_mut().clocks(motion_t,motion_beat,preferences.timing[show::part(mood)].motion*rule.motion,dt)
        };
        let accent = if self.backdrop || broadcast_only {0.0} else {
            let mut envelope=self.accents.borrow_mut();
            let mut amount=envelope.advance((m.playing && m.beat>=0.0 && gate>0.01).then_some(m.beat),mood==Mood::Drop,dt as f32,preferences.accents);
            if self.controls.pulse.replace(false) {envelope.trigger(preferences.accents.strength);amount=preferences.accents.strength;}
            amount
        };
        let rhythmic = !d.motion().calm();
        let mixing = m.transition
            || m.handover
                .as_ref()
                .is_some_and(|h| h.beats_in >= 0.0 && h.beats_in < h.beats);
        let sources = self.controls.sources();
        let compose = |cur: &Current, beat: f64, energy: f32, span: f64| {
            let palette = steering.borrow().palette(cur.palette, theme, beat);
            let rule=self.controls.response(&cur.material_id);
            let intensity=if self.backdrop || broadcast_only {1.0}else{preferences.timing[show::part(cur.mood)].intensity*rule.response};
            let energy = steering.borrow().energy(energy, bass, mid, high)*intensity;
            let brightness = steering.borrow().brightness();
            let ctx = stage::Ctx {
                cols: cur.cols,
                rows: cur.rows,
                music_beat: beat,
                beat: if frozen { beat } else { motion_beat },
                t: if frozen { t } else { motion_t },
                age: if frozen || cur.timed_title {
                    beat - cur.began_beat
                } else {
                    motion_beat - cur.began_motion
                },
                span,
                energy,
                bass: bass*intensity,
                mid: mid*intensity,
                high: high*intensity,
                bands: &bands,
                history: &hist,
                pal: &palette,
                title: &cur.title,
                artist: &cur.artist,
                seed: cur.seed,
                ease: cur.ease,
                size: cur.size,
                place: cur.place,
                bright: cur.bright * brightness,
                fonts: Some(fonts),
                font: cur.font,
                style: cur.style,
                gate: gate*rule.beat,
            };
            let pointer = (v.get_pointer_present() && !v.get_layers_open()).then(|| (v.get_pointer_x(),v.get_pointer_y()));
            let kind = cur.backdrop;
            if !cur.enabled {return patch_visual::grid::Grid::blank(cur.cols,cur.rows);}
            // The world survives text/visual passages within the same track;
            // scene seeds still control title acts and transition patterns.
            // No cell scenes among the sources: a shader carries the picture
            // and the cells hold only the lettering, never a Patch backdrop.
            let mut g = if cur.shader.is_some() && !(sources.patch || sources.ambient) {
                patch_visual::grid::Grid::blank(cur.cols, cur.rows)
            } else if kind.ambient() {
                let ambient_ctx = stage::Ctx { music_beat: if m.playing && m.beat >= 0.0 { ctx.music_beat } else { -1.0 }, seed: cur.world_seed, bright: brightness, ..ctx };
                self.ambient.borrow_mut().draw(kind, &ambient_ctx, pointer)
            } else {
                stage::backdrop(kind, &ctx)
            };
            if kind.ambient() && preferences.palette!=show::PaletteMode::Original {
                for cell in &mut g.cells {if let Some(color)=cell.fg {
                    let light=f32::from(*color.iter().max().unwrap_or(&0))/255.0;
                    cell.fg=Some(patch_visual::pack::mix(palette.a,palette.c,light).map(|v|(f32::from(v)*light) as u8));
                }}
            }
            match cur.overlay.filter(|_| !kind.ambient()) {
                // A visual: the second backdrop over the first, brighter.
                Some(o) => {
                    let enter = (ctx.age / 8.0).clamp(0.0, 1.0) as f32;
                    let leave = ((ctx.span - ctx.age) / 4.0).clamp(0.0, 1.0) as f32;
                    let develop = enter * enter * (3.0 - 2.0 * enter) * leave;
                    stage::over(
                        &mut g,
                        &stage::backdrop(
                            o,
                            &stage::Ctx {
                                bright: cur.bright * brightness * 1.2 * develop,
                                seed: cur.seed ^ 0x9e37,
                                ..ctx
                            },
                        ),
                    );
                    steering.borrow_mut().backdrop(&mut g, beat);
                }
                // Text: readable over its backdrop.
                None => {
                    steering.borrow_mut().backdrop(&mut g, beat);
                    let words = stage::act(
                        if mixing { Act::Visual } else { cur.act },
                        &stage::Ctx {
                            beat: if cur.timed_title || rhythmic { beat } else { ctx.beat },
                            ..ctx
                        },
                    );
                    stage::over_readable(&mut g, &words);
                }
            }
            g
        };
        // The kick: brightness, glow, a jump and a colour split, each as
        // hard as the song's look says.
        // A laid-back song: a soft pulse, no split, a small bump.
        let calm = m.laid_back();
        let look = |cur: &Current, at: f64, energy: f32| {
            let f = if m.playing && m.beat >= 0.0 {
                patch_visual::beat::feel(cur.beat, at, energy)
            } else {
                Default::default()
            };
            let f = if self.backdrop || broadcast_only {
                patch_visual::beat::Feel { pulse:f.pulse*gate,bump:f.bump*gate,split:f.split*gate,shake:f.shake*gate,tint:f.tint*gate }
            } else {
                let amount=accent*self.controls.response(&cur.material_id).accent;
                patch_visual::beat::Feel {pulse:amount,bump:if preferences.accents.bump {amount}else{0.0},split:if preferences.accents.split {amount}else{0.0},shake:if preferences.accents.shake {amount}else{0.0},tint:amount*0.2}
            };
            let text = cur.act != Act::Visual;
            let px = (pw as f32 / 1280.0).max(0.5);
            Look {
                // The ground flashes in the song's colour (less behind a title).
                bg: patch_visual::pack::mix(
                    cur.palette.bg,
                    cur.palette.a,
                    f.tint * if text { 0.12 } else { 0.28 },
                ),
                text: cur.palette.c,
                pulse: f.pulse * cur.punch * (1.0 - 0.5 * calm),
                glow: cur.glow * (0.6 + 0.6 * energy),
                bump: f.bump * 0.45 * cur.punch * (1.0 - 0.8 * calm),
                // (Text: a small split; a strong one doubles the letters.)
                split: f.split * 7.0 * cur.punch * (1.0 - calm) * if text { 0.3 } else { 1.0 } * px,
                shake: f.shake
                    * 10.0
                    * cur.punch
                    * (1.0 - calm)
                    * if text { 0.4 } else { 1.0 }
                    * px,
            }
        };
        // On the GPU when the window's renderer is ready (cells only), else
        // drawn here (every pixel) and handed over as an image.
        let gpu = if broadcast_only { None } else {
            let key = w.window() as *const slint::Window as usize;
            let mut g = self.gpu.borrow_mut();
            if g.as_ref().is_none_or(|(k, _)| *k != key) {
                let shared = std::rc::Rc::new(crate::visual_gl::Shared::default());
                *shared.font.borrow_mut() = th.get_font().to_string();
                let ok = crate::visual_gl::install(w.window(), shared.clone());
                if !ok {
                    shared.failed.set(true);
                }
                *g = Some((key, shared));
            }
            g.as_ref()
                .map(|(_, s)| s.clone())
                .filter(|s| s.ready.get() && !s.failed.get())
        };
        let rect = {
            let (win_w, win_h) = (
                w.window().size().width as i32,
                w.window().size().height as i32,
            );
            if full {
                [0, 0, win_w, win_h]
            } else {
                [
                    0,
                    (th.get_header_h() * scale).round() as i32,
                    (lw * scale).round() as i32,
                    (lh * scale).round() as i32,
                ]
            }
        };
        // The generative layer (GPU only; the CPU draws choreography).
        let gen_frame = RefCell::new(if frozen || gpu.is_none() {
            None
        } else {
            self.generative.frame(t, dt, m, d.intent())
        });
        let layer_seconds = d.motion().seconds as f32;
        let publish_frame = |g: &patch_visual::grid::Grid, look: Look, glyphs: &mut Glyphs, intent: patch_core::visual_intent::Intent, current: &Current| {
            if frozen || t < self.broadcast_published.get() || t - self.broadcast_published.get() >= 0.05 {
                self.broadcast_published.set(t);
                let broadcast: Vec<_> = wins.iter().filter(|w| w.get_surface() == "performer").collect();
                if !broadcast.is_empty() {
                    let section = format!("{:?}", intent.section).to_uppercase();
                    for win in broadcast {
                        let state = win.global::<crate::ui::BroadcastDjState>();
                        state.set_motion(intent.motion);
                        state.set_density(intent.density);
                        state.set_section(section.as_str().into());
                        state.set_scene_seed((current.seed >> 40) as f32 / 16_777_215.0);
                        state.set_scene_progress(if frozen { 0.25 } else { ((t-self.scene_at.get())/(current.ends-self.scene_at.get()).max(0.1)).clamp(0.0,1.0) as f32 });
                        use patch_core::visual_intent::Section;
                        state.set_section_id(match intent.section { Section::Unknown=>0.0,Section::Intro=>1.0,Section::Verse=>2.0,Section::Build=>3.0,Section::Drop=>4.0,Section::Breakdown=>5.0,Section::Transition=>6.0,Section::Outro=>7.0 });
                        state.set_handover(m.handover.as_ref().filter(|h| h.beats_in>=0.0&&h.beats>0.0).map_or(-1.0,|h| (h.beats_in/h.beats).clamp(0.0,1.0) as f32));
                        let color=|c:[u8;3]| slint::Color::from_rgb_u8(c[0],c[1],c[2]);
                        state.set_stage_a(color(current.palette.a));
                        state.set_stage_b(color(current.palette.b));
                    }
                }
            }
            if broadcast_only { return; }
            if let Some(sh) = &gpu {
                let mut frame = crate::visual_gl::Frame::from_grid(
                    g,
                    look,
                    pw as f32,
                    rect,
                    &mut sh.slots.borrow_mut(),
                );
                frame.generative = gen_frame.borrow_mut().take();
                let bands = std::array::from_fn(|i| {
                    let start = i * m.bands.len() / 3;
                    let end = (i + 1) * m.bands.len() / 3;
                    if end > start { m.bands[start..end].iter().sum::<f32>() / (end-start) as f32 } else { 0.0 }
                });
                let color = |c:[u8;3]| c.map(|v|v as f32/255.0);
                let handover = m.handover.as_ref();
                let scene = patch_visual::performance::Scene {
                    seconds: layer_seconds,
                    beat: (m.playing && m.beat>=0.0).then_some(m.beat as f32),
                    seed: (current.seed >> 40) as f32 / 16_777_215.0,
                    progress: ((t-self.scene_at.get())/(current.ends-self.scene_at.get()).max(0.1)).clamp(0.0,1.0) as f32,
                    intent: steering.borrow().intent().or(layer_posted).unwrap_or(intent),
                    palette: { let pal=steering.borrow().palette(current.palette,theme,beat_now); [color(pal.a),color(pal.b)] },
                    handover: handover.filter(|h|h.beats_in>=0.0 && h.beats>0.0).map(|h|(h.beats_in/h.beats).clamp(0.0,1.0) as f32),
                    transition_preparing: handover.is_some_and(|h|h.beats_in<0.0),
                    bands, kick: gate, crossfader: 0.5,
                };
                if !self.backdrop { frame.performance = self.layers.frame(w,scene,dt as f32,sh.layers_ms.get(),preferences.layers[show::part(current.mood)]); }
                // The Visualiser's own words stay in the scene, behind the
                // booth and the operator (they were redrawn over both).
                v.set_layers_gpu(sh.layers_ready.get());
                *sh.frame.borrow_mut() = Some(frame);
                if !v.get_gpu() {
                    v.set_gpu(true);
                    v.set_frame(slint::Image::default());
                }
                w.window().request_redraw();
                return;
            }
            // Headless shots of the track text and mix display
            // (`PATCH_SHOT_LAYERS=1`): no GPU layers there, the overlays show.
            let staged = patch_host::shot::active() && std::env::var_os("PATCH_SHOT_LAYERS").is_some();
            v.set_layers_gpu(staged);
            if staged {
                v.set_decks_enabled(true);
                v.set_a_title("Kerala".into());
                v.set_a_mix("Original Mix".into());
                v.set_b_title("Two Thousand And Seventeen".into());
                v.set_b_mix("Extended Mix".into());
            }
            if v.get_gpu() {
                v.set_gpu(false);
            }
            let px = patch_visual::render::draw(g, pw, ph, glyphs, &look);
            let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(pw as u32, ph as u32);
            buf.make_mut_bytes().copy_from_slice(&px);
            v.set_frame(slint::Image::from_rgba8(buf));
        };
        // Pictures (headless): a reveal a bar in, over its backdrop.
        if frozen {
            let (mut cur, span) = scene_for(&mut d, Mood::Reveal);
            cur.began_beat = 4.0;
            let g = compose(&cur, 8.1, 0.8, span);
            publish_frame(&g, look(&cur, 8.02, 0.8), glyphs, d.intent(), &cur);
            v.set_caption(
                format!(
                    "{} · {} · {}",
                    cur.act.name(),
                    cur.backdrop.name(),
                    cur.palette.name
                )
                .into(),
            );
            return;
        }
        let mut current = self.current.borrow_mut();
        // Reflow the scene without asking the director for a new one:
        // preserve its seed, act, palette, clocks and choreography.
        if let Some(cur) = current.as_mut() {
            cur.k = if cur.font > 0 && cur.act != Act::Visual {
                type_k
            } else {
                block_k
            };
            (cur.cols, cur.rows) = canvas(cur.k);
        }
        let ended = current.as_ref().is_none_or(|c| t >= c.ends);
        let safe_bar = m.beat < 0.0 || m.beat.rem_euclid(4.0) < 0.25;
        let requested = self.requested_scene.borrow().as_ref().copied();
        let manual=self.controls.request.borrow_mut().take();
        let skip=self.skip.take();
        let next = if manual.is_some() {Some(d.mood(m))} else if requested.is_some() && safe_bar {
            requested.map(|(mood, _)| mood)
        } else if current.is_none() || skip {
            Some(d.mood(m))
        } else if self.held.get() || !preferences.automatic {
            // Held: the same again when it ends, or at a new size.
            ended.then(|| d.mood.unwrap_or(Mood::Groove))
        } else {
            d.due(m, ended, t)
        };
        if let Some(mood) = next {
            if let Some((requested_mood, index)) = requested.filter(|(requested_mood, _)| *requested_mood == mood) {
                d.select_scene(requested_mood, index);
                self.requested_scene.borrow_mut().take();
            }
            let held = self
                .held
                .get()
                .then_some(true)
                .or((!preferences.automatic && manual.is_none()).then_some(true))
                .and_then(|_| current.as_ref().map(|c| (c.act, c.backdrop,c.material_id.clone(),c.shader.clone(),c.mode)));
            let (mut cur, span) = scene_for(&mut d, mood);
            if let Some((act, backdrop,id,shader,mode)) = held {
                cur.material_id=id;cur.shader=shader;cur.mode=mode;cur.enabled=true;
                // Holding an effect must respect the director's mandatory
                // visual gap and delayed first announcement.
                if cur.act != Act::Visual {
                    cur.act = act;
                }
                cur.backdrop = backdrop;
            }
            if let Some(entry)=manual {
                cur.material_id=entry.id.clone();cur.enabled=true;
                match entry.material {
                    Material::Grid(kind)=>{cur.backdrop=kind;cur.overlay=None;cur.act=Act::Visual;cur.bright=1.0;cur.shader=None;cur.mode=if self.controls.sources().shaders && self.generative.mode()==Mode::Hybrid {Mode::Hybrid}else{Mode::Choreography};},
                    Material::Shader(name)=>{cur.shader=Some(name);cur.mode=if self.generative.mode()==Mode::Hybrid {Mode::Hybrid}else{Mode::Generative};},
                    Material::Effect(act)=>{if let Some(old)=current.as_ref() {cur.backdrop=old.backdrop;}cur.act=if mixing {Act::Visual}else{act};cur.shader=None;cur.mode=Mode::Choreography;cur.overlay=None;}
                }
            }
            let sources=self.controls.sources();
            if cur.shader.is_some() && !(sources.patch || sources.ambient) {
                // Shader alone, or under the title's letters when Titles are on.
                cur.mode=if sources.titles && cur.act!=Act::Visual {Mode::Hybrid} else {Mode::Generative};
            }
            if let Some(shader)=&cur.shader {self.generative.select(shader,cur.mode);} else if self.generative.mode()!=cur.mode {self.generative.set_mode(cur.mode);}
            self.controls.observe(&cur.material_id);
            let over = cur
                .overlay
                .map(|o| format!(" + {}", o.name()))
                .unwrap_or_default();
            *self.caption.borrow_mut() = format!(
                "{} · {}{over} · {} · {} · {}",
                cur.act.name(),
                cur.backdrop.name(),
                cur.beat.name(),
                cur.palette.name,
                mood.name()
            );
            *self.previous.borrow_mut() = current
                .as_ref()
                .filter(|old| old.act == Act::Visual && cur.act == Act::Visual)
                .map(|old| (old.clone(), self.span.get()));
            *current = Some(cur);
            self.span.set(span);
            self.scene_at.set(t);
        }
        let energy = d.energy();
        let gl_error = gpu.as_ref().and_then(|s| s.gen_error.borrow().as_ref().map(|e| e.1.clone()));
        let caption = format!("{}{}{}", self.caption.borrow(), self.generative.caption(gl_error.as_deref()), if posted.is_some() { " · remote intent" } else { "" });
        if !self.backdrop && (t - self.status_written.get() >= 1.0 || t < self.status_written.get()) {
            self.status_written.set(t);
            if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
                let path = std::path::PathBuf::from(runtime).join("omarchy-patch/visualiser-status.json");
                let tmp = path.with_extension("tmp");
                let status = serde_json::json!({"updated_ms":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d|d.as_millis() as u64),"mode":self.generative.mode().name(),"caption":caption,"intent_active":posted.is_some(),"held":self.held.get(),"scenes":d.scene_choices()});
                if let Ok(bytes) = serde_json::to_vec(&status)
                    && std::fs::write(&tmp, bytes).is_ok()
                {
                    let _ = std::fs::rename(tmp, path);
                }
            }
        }
        v.set_caption(caption.as_str().into());
        v.set_held(self.held.get());
        if !self.backdrop {self.controls.mirror(w);}
        v.set_scene_age((t - self.scene_at.get()) as f32);
        // The stage moves every frame: drawn every frame.
        let Some(cur) = current.as_ref() else { return };
        if broadcast_only {
            publish_frame(&patch_visual::grid::Grid::blank(1, 1), look(cur, beat_now, energy), glyphs, d.intent(), cur);
            return;
        }
        let mut g = compose(cur, beat_now, energy, self.span.get().max(1.0));
        let transition_s = if self.backdrop {(if rhythmic {120.0}else{240.0})/m.bpm.max(60.0)} else {f64::from(preferences.transition_beats)*60.0/m.bpm.max(60.0)};
        let transition_s=if cur.transition.is_none() && !self.backdrop {0.0}else{transition_s};
        let progress = if transition_s>0.0 {((t - self.scene_at.get()) / transition_s).clamp(0.0, 1.0) as f32}else{1.0};
        let mut previous = self.previous.borrow_mut();
        if progress < 1.0 {
            if let Some((old, old_span)) = previous.as_mut() {
                (old.cols, old.rows) = canvas(old.k);
                let before = compose(old, beat_now, energy, *old_span);
                stage::transition_style(&mut g, &before, progress,cur.transition.unwrap_or(cur.seed%3),cur.seed);
            }
        } else {
            *previous = None;
        }
        publish_frame(&g, look(cur, beat_now, energy), glyphs, d.intent(), cur);
    }
}

/// "82. Shermanology - Insane (Original Mix).mp3" → ("Shermanology"): the
/// artist from a file name in the usual "NN. Artist - Title" form.
pub fn artist_of(path: &str) -> String {
    let stem = std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let stem = stem
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim_start_matches(['.', ' ', '-'])
        .to_string();
    stem.split_once(" - ")
        .map(|(a, _)| a.trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn stage_scales_without_changing_composition() {
        for k in [1.0, 0.5, 0.25] {
            let window = super::stage_size(640.0, 360.0, k);
            assert_eq!(window, super::stage_size(1920.0, 1080.0, k));
            assert_eq!(window, super::stage_size(3840.0, 2160.0, k));
        }
        let wide = super::stage_size(1920.0, 1080.0, 1.0);
        let square = super::stage_size(800.0, 800.0, 1.0);
        assert_eq!(wide.0, square.0);
        assert!(square.1 > wide.1);
    }

    #[test]
    fn the_artist_from_a_file_name() {
        assert_eq!(
            super::artist_of("/m/82. Shermanology - Insane (Original Mix).mp3"),
            "Shermanology"
        );
        assert_eq!(super::artist_of("/m/Insane.mp3"), "");
    }
}
