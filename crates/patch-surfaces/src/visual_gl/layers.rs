//! Fixed programs and bounded draw calls in the Visualiser's own GL context.
//! Targets, post-processing, and musical time remain owned by the parent.
use super::*;
use patch_visual::performance::{Frame as Performance, MAX_SOURCES};

pub(crate) const DECK_VERTEX: &str = include_str!("../../ui/visual-decks.vert");
pub(crate) const SOLID_VERTEX: &str = include_str!("../../ui/visual-performer-solid.vert");
pub(crate) const SOLID_FRAGMENT: &str = include_str!("../../ui/visual-performer-solid.frag");
pub(crate) const FILM_VERTEX: &str = include_str!("../../ui/visual-performer-video.vert");
pub(crate) const FILM_FRAGMENT: &str = include_str!("../../ui/visual-performer-video.frag");
pub(crate) const BOOTH_VERTEX: &str = include_str!("../../ui/visual-booth.vert");
pub(crate) const BOOTH_FRAGMENT: &str = include_str!("../../ui/visual-booth.frag");
fn shader(_name: &str, embedded: &'static str) -> std::borrow::Cow<'static, str> {
    // Optical iteration uses the production compositor test, never a second
    // renderer or a production file-watching path.
    #[cfg(test)]
    if let Ok(root) = std::env::var("PATCH_VISUAL_TEST_SHADERS")
        && let Ok(source) = std::fs::read_to_string(std::path::Path::new(&root).join(_name))
    {
        return std::borrow::Cow::Owned(source);
    }
    std::borrow::Cow::Borrowed(embedded)
}
pub(crate) fn surface(source: &str) -> String {
    source.replace(
        "// PATCH_SURFACE",
        &shader(
            "performer-surface.glsl",
            include_str!("../../ui/performer-surface.glsl"),
        ),
    )
}
const NAMES: &[&str] = &[
    "resolution",
    "clock_s",
    "time",
    "scene_beat",
    "body_beat",
    "rim",
    "emblem",
    "beat",
    "energy",
    "motion",
    "density",
    "section",
    "chapter",
    "seed",
    "handover",
    "kick",
    "layer_opacity",
    "appearance",
    "film_glitch",
    "presence",
    "composed",
    "colour_a",
    "colour_b",
    "bands",
    "ar",
    "ag",
    "ab",
    "br",
    "bg",
    "bb",
    "source_count",
    "point_step",
    "preparing",
    "crossfader",
];
struct Program {
    id: glow::Program,
    uniforms: HashMap<&'static str, Option<glow::UniformLocation>>,
    state: [Option<glow::UniformLocation>; MAX_SOURCES],
    tone: [Option<glow::UniformLocation>; MAX_SOURCES],
    fx: [Option<glow::UniformLocation>; MAX_SOURCES],
    gain: [Option<glow::UniformLocation>; MAX_SOURCES],
    progress: [Option<glow::UniformLocation>; MAX_SOURCES],
    meter: [Option<glow::UniformLocation>; MAX_SOURCES],
    body: [Option<glow::UniformLocation>; 4],
}
// Draw sizes, as performer-surface.glsl defines them. The DJ is one quad.
const BOOTH_PARTS: i32 = 48;
const BOOTH_VERTICES: i32 = 192;
impl Program {
    fn new(gl: &glow::Context, header: &str, vert: &str, frag: &str) -> Result<Self, String> {
        // The fragment stages light with the same camera, so they share the
        // surface code too.
        let id = program_with_vertex(gl, header, &surface(vert), &surface(frag))?;
        // SAFETY: the parent owns the current context, all queries are setup-only.
        unsafe {
            Ok(Self {
                id,
                uniforms: NAMES
                    .iter()
                    .map(|&n| (n, gl.get_uniform_location(id, n)))
                    .collect(),
                state: std::array::from_fn(|i| {
                    gl.get_uniform_location(id, &format!("source_state[{i}]"))
                }),
                tone: std::array::from_fn(|i| {
                    gl.get_uniform_location(id, &format!("source_tone[{i}]"))
                }),
                gain: std::array::from_fn(|i| {
                    gl.get_uniform_location(id, &format!("source_gain[{i}]"))
                }),
                fx: std::array::from_fn(|i| {
                    gl.get_uniform_location(id, &format!("source_fx[{i}]"))
                }),
                progress: std::array::from_fn(|i| {
                    gl.get_uniform_location(id, &format!("source_progress[{i}]"))
                }),
                meter: std::array::from_fn(|i| {
                    gl.get_uniform_location(id, &format!("source_meter[{i}]"))
                }),
                body: ["gesture", "gesture2", "body0", "body1"]
                    .map(|n| gl.get_uniform_location(id, n)),
            })
        }
    }
    fn location(&self, name: &str) -> Option<&glow::UniformLocation> {
        self.uniforms.get(name).and_then(Option::as_ref)
    }
    fn bind(&self, gl: &glow::Context, f: &Performance, w: i32, h: i32, opacity: f32) {
        let s = &f.scene;
        let colors = if f.settings.director_colours {
            s.palette
        } else {
            f.settings.colours
        };
        let section = match s.intent.section {
            patch_core::visual_intent::Section::Unknown => 0.0,
            patch_core::visual_intent::Section::Intro => 1.0,
            patch_core::visual_intent::Section::Verse => 2.0,
            patch_core::visual_intent::Section::Build => 3.0,
            patch_core::visual_intent::Section::Drop => 4.0,
            patch_core::visual_intent::Section::Breakdown => 5.0,
            patch_core::visual_intent::Section::Transition => 6.0,
            patch_core::visual_intent::Section::Outro => 7.0,
        };
        // SAFETY: fixed programs in the parent's current context. No driver
        // queries, allocations, compilation, readbacks or target creation here.
        unsafe {
            gl.use_program(Some(self.id));
            gl.uniform_2_f32(self.location("resolution"), w as f32, h as f32);
            for (n, v) in [
                ("clock_s", s.seconds),
                ("time", s.seconds),
                ("scene_beat", s.beat.unwrap_or(-1.0)),
                ("beat", s.beat.unwrap_or(-1.0)),
                ("energy", s.intent.intensity),
                ("motion", s.intent.motion),
                ("density", s.intent.density),
                ("section", section),
                ("chapter", s.progress),
                ("seed", s.seed),
                ("handover", s.handover.unwrap_or(-1.0)),
                ("kick", s.kick),
                ("crossfader", s.crossfader),
                ("point_step", f.point_step.clamp(1, 4) as f32),
                ("layer_opacity", opacity),
                ("appearance", f.appearance),
                ("composed", 1.0),
                ("preparing", if s.transition_preparing { 1.0 } else { 0.0 }),
                ("ar", colors[0][0]),
                ("ag", colors[0][1]),
                ("ab", colors[0][2]),
                ("br", colors[1][0]),
                ("bg", colors[1][1]),
                ("bb", colors[1][2]),
            ] {
                gl.uniform_1_f32(self.location(n), v);
            }
            gl.uniform_3_f32(
                self.location("colour_a"),
                colors[0][0],
                colors[0][1],
                colors[0][2],
            );
            gl.uniform_3_f32(
                self.location("colour_b"),
                colors[1][0],
                colors[1][1],
                colors[1][2],
            );
            gl.uniform_3_f32(self.location("bands"), s.bands[0], s.bands[1], s.bands[2]);
            let b = &f.body;
            for (at, v) in self.body.iter().zip([
                b.gesture,
                b.gesture2,
                [b.nod, b.tilt, b.sway, b.bounce],
                [b.look, b.lean, b.shrug[0], b.shrug[1]],
            ]) {
                gl.uniform_4_f32(at.as_ref(), v[0], v[1], v[2], v[3]);
            }
            // The body's steady beat clock, for the hands' small moves.
            gl.uniform_1_f32(self.location("body_beat"), b.beat);
            gl.uniform_1_f32(self.location("rim"), b.rim);
            gl.uniform_1_f32(self.location("emblem"), f.settings.emblem.index() as f32);
            gl.uniform_1_i32(self.location("source_count"), f.sources().len() as i32);
            for (i, source) in f.sources().iter().enumerate() {
                gl.uniform_4_f32(
                    self.state[i].as_ref(),
                    source.level,
                    source.beat.unwrap_or(-1.0),
                    source.weight,
                    if source.active { 1.0 } else { 0.0 },
                );
                gl.uniform_4_f32(
                    self.tone[i].as_ref(),
                    source.eq[0],
                    source.eq[1],
                    source.eq[2],
                    source.filter,
                );
                gl.uniform_1_f32(self.fx[i].as_ref(), source.fx);
                gl.uniform_1_f32(self.gain[i].as_ref(), source.gain);
                gl.uniform_1_f32(self.progress[i].as_ref(), source.progress);
                gl.uniform_2_f32(self.meter[i].as_ref(), source.meter[0], source.meter[1]);
            }
        }
    }
}

pub(super) struct Layers {
    decks: Program,
    booth: Program,
    solid: Program,
    depth: glow::Renderbuffer,
    depth_size: (i32, i32),
    film_program: Program,
    film_sampler: Option<glow::UniformLocation>,
    film_look_at: Option<glow::UniformLocation>,
    film_glitch_at: Option<glow::UniformLocation>,
    /// The looks as they are now, easing toward what is asked, and when.
    film_look: [f32; 3],
    /// Each performer's own amount (transparency), kept while it fades out.
    film_amount: [f32; 3],
    film_eased: Option<std::time::Instant>,
    /// A test's own clock, so a recording steps one video frame per draw.
    #[cfg(test)]
    pub(super) clock: Option<std::time::Instant>,
    /// Open only while the filmed DJ is on.
    film: Option<super::performer_film::Player>,
    film_texture: Option<glow::Texture>,
    film_ready: bool,
    film_missing: bool,
    attached: [Option<glow::Framebuffer>; 2],
    /// What the layers cost the GPU, in ms: two timer queries taken in
    /// turn, each read a frame later (never waiting on the GPU). None on
    /// OpenGL ES, which has no timer query, or while nothing is drawn.
    timers: Option<[glow::Query; 2]>,
    pending: [bool; 2],
    turn: usize,
    pub(super) gpu_ms: Option<f32>,
}
impl Layers {
    fn now(&self) -> std::time::Instant {
        #[cfg(test)]
        if let Some(t) = self.clock {
            return t;
        }
        std::time::Instant::now()
    }
    pub(super) fn new(gl: &glow::Context, header: &str) -> Result<Self, String> {
        let specifications = [
            (
                "visual-decks.vert",
                DECK_VERTEX,
                "broadcast-performer.frag",
                include_str!("../../ui/broadcast-performer.frag"),
            ),
            (
                "visual-booth.vert",
                BOOTH_VERTEX,
                "visual-booth.frag",
                BOOTH_FRAGMENT,
            ),
            (
                "visual-performer-solid.vert",
                SOLID_VERTEX,
                "visual-performer-solid.frag",
                SOLID_FRAGMENT,
            ),
            (
                "visual-performer-video.vert",
                FILM_VERTEX,
                "visual-performer-video.frag",
                FILM_FRAGMENT,
            ),
        ];
        let mut built: Vec<Program> = Vec::with_capacity(4);
        for (vn, vs, fn_, fs) in specifications {
            match Program::new(gl, header, &shader(vn, vs), &shader(fn_, fs)) {
                Ok(p) => built.push(p),
                Err(e) => {
                    for p in built {
                        unsafe { gl.delete_program(p.id) }
                    }
                    return Err(e);
                }
            }
        }
        let depth = match unsafe { gl.create_renderbuffer() } {
            Ok(d) => d,
            Err(e) => {
                for p in built {
                    unsafe { gl.delete_program(p.id) }
                }
                return Err(e);
            }
        };
        let film_program = built.pop().unwrap();
        let film_sampler = unsafe { gl.get_uniform_location(film_program.id, "film") };
        let film_look_at = unsafe { gl.get_uniform_location(film_program.id, "film_look") };
        let film_glitch_at = unsafe { gl.get_uniform_location(film_program.id, "film_glitch") };
        let solid = built.pop().unwrap();
        let booth = built.pop().unwrap();
        let decks = built.pop().unwrap();
        // SAFETY: setup in the parent's current context.
        let timers = (!gl.version().is_embedded)
            .then(|| unsafe { Some([gl.create_query().ok()?, gl.create_query().ok()?]) })
            .flatten();
        Ok(Self {
            decks,
            booth,
            solid,
            film_program,
            film_sampler,
            film_look_at,
            film_glitch_at,
            film_look: [1.0, 0.0, 0.0],
            film_amount: [1.0, 1.0, 1.0],
            film_eased: None,
            #[cfg(test)]
            clock: None,
            film: None,
            film_texture: None,
            film_ready: false,
            film_missing: false,
            depth,
            depth_size: (0, 0),
            attached: [None; 2],
            timers,
            pending: [false; 2],
            turn: 0,
            gpu_ms: None,
        })
    }
    /// Draw the layers, timing them on the GPU.
    pub(super) fn draw(
        &mut self,
        gl: &glow::Context,
        f: &Performance,
        w: i32,
        h: i32,
        target: glow::Framebuffer,
    ) {
        if !f.visible() {
            self.gpu_ms = None;
            return;
        }
        let Some(timers) = self.timers else {
            self.draw_layers(gl, f, w, h, target);
            return;
        };
        let (q, i) = (timers[self.turn], self.turn);
        // SAFETY: queries made in this context; a result is read only once
        // the GPU says it is available.
        unsafe {
            if self.pending[i] && gl.get_query_parameter_u32(q, glow::QUERY_RESULT_AVAILABLE) != 0 {
                let ns = gl.get_query_parameter_u32(q, glow::QUERY_RESULT);
                self.gpu_ms = Some(ns as f32 / 1e6);
                self.pending[i] = false;
            }
            if self.pending[i] {
                // Not back yet: skip timing this frame rather than wait.
                self.draw_layers(gl, f, w, h, target);
                return;
            }
            gl.begin_query(glow::TIME_ELAPSED, q);
            self.draw_layers(gl, f, w, h, target);
            gl.end_query(glow::TIME_ELAPSED);
        }
        self.pending[i] = true;
        self.turn = 1 - self.turn;
    }
    fn draw_layers(
        &mut self,
        gl: &glow::Context,
        f: &Performance,
        w: i32,
        h: i32,
        target: glow::Framebuffer,
    ) {
        // One shared depth attachment, on the existing scene/composite target.
        // Storage is created only at setup/resize, never once per layer/frame.
        unsafe {
            if self.depth_size != (w, h) {
                gl.bind_renderbuffer(glow::RENDERBUFFER, Some(self.depth));
                gl.renderbuffer_storage(glow::RENDERBUFFER, glow::DEPTH_COMPONENT24, w, h);
                self.depth_size = (w, h);
                self.attached = [None; 2];
            }
            if !self.attached.contains(&Some(target)) {
                gl.framebuffer_renderbuffer(
                    glow::FRAMEBUFFER,
                    glow::DEPTH_ATTACHMENT,
                    glow::RENDERBUFFER,
                    Some(self.depth),
                );
                if self.attached[0].is_none() {
                    self.attached[0] = Some(target);
                } else {
                    self.attached[1] = Some(target);
                }
            }
            gl.depth_mask(true);
            gl.clear_depth_f32(1.0);
            gl.clear(glow::DEPTH_BUFFER_BIT);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
            gl.enable(glow::BLEND);
            gl.blend_equation(glow::FUNC_ADD);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
            // 1 · The booth, solid, in depth.
            if f.settings.decks && f.settings.deck_opacity > 0.0 {
                self.booth.bind(gl, f, w, h, f.settings.deck_opacity);
                gl.draw_arrays(glow::TRIANGLES, 0, BOOTH_PARTS * BOOTH_VERTICES);
                // 2 · The decks' ribbons, behind the DJ, hidden by the booth.
                gl.depth_mask(false);
            }
            if f.settings.decks && f.settings.deck_opacity > 0.0 && f.settings.ribbons {
                gl.blend_func(glow::SRC_ALPHA, glow::ONE);
                let desktop = !gl.version().is_embedded;
                let was_sized = desktop && gl.is_enabled(glow::PROGRAM_POINT_SIZE);
                if desktop {
                    gl.enable(glow::PROGRAM_POINT_SIZE);
                }
                self.decks.bind(gl, f, w, h, f.settings.deck_opacity);
                gl.draw_arrays(
                    glow::POINTS,
                    0,
                    f.sources().len() as i32 * 4096 / f.point_step.clamp(1, 4) as i32,
                );
                if desktop && !was_sized {
                    gl.disable(glow::PROGRAM_POINT_SIZE);
                }
                gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
            }
            // 3 · The DJ: one silhouette, behind the booth, hands on it.
            if f.settings.performer && f.settings.performer_opacity > 0.0 {
                // Depth per pixel (gl_FragDepth): the booth hides the body,
                // the hands lie over the controls.
                gl.depth_mask(false);
                self.draw_performer(gl, f, w, h);
            }
            gl.depth_mask(true);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::BLEND);
        }
    }
    /// The three performers: the drawn DJ as a silhouette or as points (its
    /// own body and moves, the chest mark fixed on its chest), and the filmed
    /// DJ, the person. Each has its own amount, so they mix and any can be
    /// see-through; a change of performer passes through the hologram's
    /// distortion. Without the film, the drawn DJ stands in for the person.
    fn draw_performer(&mut self, gl: &glow::Context, f: &Performance, w: i32, h: i32) {
        use super::performer_film::{HEIGHT, Player, WIDTH};
        let target = if f.settings.me {
            patch_visual::performance::film_look(f.settings, &f.scene)
        } else if f.appearance >= 0.5 {
            [0.0, 0.0, 1.0]
        } else {
            [0.0, 1.0, 0.0]
        };
        // A change is a hologram's morph, about 2 s: the old performer
        // breaks up into flickering points as the new one forms out of them,
        // both tinted and torn while it lasts. At rest only one shows.
        let now = self.now();
        let dt = self
            .film_eased
            .map_or(1.0, |t| now.duration_since(t).as_secs_f32().min(0.25));
        self.film_eased = Some(now);
        for i in 0..3 {
            if target[i] > 0.0 {
                self.film_amount[i] = target[i];
            }
            let gap = target[i] - self.film_look[i];
            self.film_look[i] += gap.clamp(-dt * 0.5, dt * 0.5);
        }
        let lead = (0..3).fold(0, |b, i| if target[i] > target[b] { i } else { b });
        let progress = self.film_look[lead] / target[lead].max(1e-3);
        let changing = progress < 0.999;
        let glitch = if changing { 0.35 + 0.65 * (1.0 - (2.0 * progress - 1.0).abs()) } else { 0.0 };
        let presence = |i: usize, look: &[f32; 3], amount: &[f32; 3]| (look[i] / amount[i].max(1e-3)).min(1.0);
        let [mut person, silhouette, points] = self.film_look;
        let amount = self.film_amount;
        let look = self.film_look;
        // The film is decoded only while the person shows or is coming.
        let wanted = person > 0.003 || target[0] > 0.003;
        if !wanted {
            self.film = None;
            self.film_ready = false;
        } else if self.film.is_none() && !self.film_missing {
            self.film = Player::open();
            self.film_missing = self.film.is_none();
        }
        let mut frame = None;
        if wanted && let Some(player) = self.film.as_mut() {
            let drop = f.scene.intent.section == patch_core::visual_intent::Section::Drop;
            let headphones = patch_visual::performance::film_headphones(f.settings, &f.scene);
            let cue = super::performer_film::Cue::from_gestures(f.body.gesture, f.body.gesture2);
            frame = player.advance(now, f.scene.beat, drop, headphones, cue);
        }
        // Until the film is here (or where it is not installed) the drawn
        // silhouette stands in for the person.
        let stand_in = if self.film_ready || frame.is_some() { 0.0 } else { person };
        person -= stand_in;
        let drawn = (silhouette + points + stand_in).min(1.0);
        // SAFETY: the parent's current context; fixed programs; the frame is
        // exactly the size the texture was made with.
        unsafe {
            // The silhouette and the points are one drawn body, so a change
            // between them morphs in place; each is drawn at its own amount.
            if drawn > 0.003 {
                let morph = points / (silhouette + points + stand_in).max(1e-4);
                let solid_amount = amount[1] * (1.0 - morph) + amount[2] * morph;
                let shown = (presence(1, &look, &amount) + presence(2, &look, &amount)).min(1.0);
                let shown = if stand_in > 0.0 { 1.0 } else { shown };
                self.solid.bind(gl, f, w, h, f.settings.performer_opacity * solid_amount.max(drawn.min(1.0)).min(1.0));
                gl.uniform_1_f32(self.solid.location("appearance"), morph);
                gl.uniform_1_f32(self.solid.location("film_glitch"), glitch);
                gl.uniform_1_f32(self.solid.location("presence"), shown);
                gl.draw_arrays(glow::TRIANGLES, 0, 6);
            }
            if !wanted {
                return;
            }
            gl.active_texture(glow::TEXTURE7);
            if self.film_texture.is_none() {
                let Ok(t) = gl.create_texture() else {
                    gl.active_texture(glow::TEXTURE0);
                    return;
                };
                gl.bind_texture(glow::TEXTURE_2D, Some(t));
                for (k, v) in [
                    (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                    (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                    (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                    (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
                ] {
                    gl.tex_parameter_i32(glow::TEXTURE_2D, k, v as i32);
                }
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA8 as i32,
                    WIDTH,
                    HEIGHT,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(None),
                );
                self.film_texture = Some(t);
            }
            gl.bind_texture(glow::TEXTURE_2D, self.film_texture);
            if let Some(frame) = frame {
                gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    0,
                    0,
                    WIDTH,
                    HEIGHT,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(&frame)),
                );
                self.film_ready = true;
                if let Some(player) = self.film.as_ref() {
                    player.give_back(frame);
                }
            }
            if self.film_ready && person > 0.003 {
                self.film_program.bind(gl, f, w, h, f.settings.performer_opacity * amount[0]);
                gl.uniform_1_i32(self.film_sampler.as_ref(), 7);
                gl.uniform_1_f32(self.film_look_at.as_ref(), (person / amount[0].max(1e-3)).min(1.0));
                gl.uniform_1_f32(self.film_glitch_at.as_ref(), glitch);
                gl.draw_arrays(glow::TRIANGLES, 0, 6);
            }
            gl.active_texture(glow::TEXTURE0);
        }
    }
    pub(super) fn destroy(&self, gl: &glow::Context) {
        unsafe {
            if let Some(t) = self.film_texture {
                gl.delete_texture(t);
            }
            for p in [&self.decks, &self.booth, &self.solid, &self.film_program] {
                gl.delete_program(p.id);
            }
            gl.delete_renderbuffer(self.depth);
        }
    }
}
