//! The Visualiser on the GPU, the way a terminal draws the screensaver: the
//! CPU decides which glyph in which colour goes in each cell, the GPU draws
//! them from a glyph atlas, with the glow and the colour split, beneath the
//! window's items (Slint's rendering notifier, in its own GL context).
//! Drawn on the CPU, a 4K frame (every pixel, the glow, a 12 MB upload)
//! took a core's 40 %; here a frame uploads two small cell textures.
//!
//! Passes: the scene into a texture at the canvas's pixel size; its bright
//! parts at an eighth, blurred (box, both ways, twice); then scene + glow,
//! red and blue split apart, into the canvas area of the window.

pub mod capture;
mod layers;
mod performer_film;
#[cfg(test)]
mod performance_test;

pub(crate) fn performer_source(source: &str) -> String { layers::surface(source) }

use glow::HasContext;
use patch_visual::grid::Grid;
use patch_visual::render::{Glyphs, Look};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// One frame to draw: the grid as two cell textures, and how it looks.
pub struct Frame {
    pub cols: usize,
    pub rows: usize,
    /// Per cell RGBA: foreground colour, glyph slot (0: nothing).
    pub a: Vec<u8>,
    /// Per cell RGBA: background colour, flags (1: own background,
    /// 2: own foreground).
    pub b: Vec<u8>,
    /// Every glyph given a slot so far (slot = index + 1): slots are kept
    /// across frames, so the atlas only gains glyphs.
    pub glyphs: Vec<char>,
    /// Bumped when the slots start over (all 255 were taken).
    pub generation: u32,
    pub look: Look,
    /// Pixel lengths in `look` (split, shake) were set for a canvas this
    /// many pixels wide.
    pub look_width: f32,
    /// The canvas in the window, physical pixels from the top left.
    pub rect: [i32; 4],
    /// The generative layer, when the Visualiser's mode has one.
    pub generative: Option<GenFrame>,
    /// Bound the internal stage resolution independently of the window.
    pub render_cap: Option<i32>,
    pub performance: Option<patch_visual::performance::Frame>,
}

/// The generative layer of a frame: a compiled scene (recompiled only when
/// `key` changes), its uniforms' values now, and how it meets the cells.
pub struct PointField {
    pub vertex: Rc<String>,
    pub count: i32,
}

pub struct GenFrame {
    pub key: u64,
    pub frag: Rc<String>,
    pub uniforms: Vec<(String, f32)>,
    pub time: f32,
    pub compositor: patch_visual::generative::Compositor,
    /// The cells are drawn over it (hybrid); else the picture alone.
    pub cells: bool,
    /// Procedurally generated geometry; otherwise the fullscreen shader.
    pub points: Option<PointField>,
}

/// What the panel and the window's renderer share.
#[derive(Default)]
pub struct Shared {
    pub frame: RefCell<Option<Frame>>,
    /// The GL renderer is set up (else the panel draws on the CPU).
    pub ready: std::cell::Cell<bool>,
    pub layers_ready: std::cell::Cell<bool>,
    /// What the DJ layers cost the GPU last measured, in ms (see
    /// `layers::Layers::draw`).
    pub layers_ms: std::cell::Cell<Option<f32>>,
    /// It failed (no OpenGL, a shader error): the CPU for good.
    pub failed: std::cell::Cell<bool>,
    /// The font the glyphs come from.
    pub font: RefCell<String>,
    /// Glyph slots, kept from frame to frame.
    pub slots: RefCell<Slots>,
    /// The generative shader didn't compile on this GPU (with its key): the
    /// cells still draw.
    pub gen_error: RefCell<Option<(u64, String)>>,
}

/// Which glyph sits in which atlas slot. Slots were handed out per frame
/// in order of first use, so they changed nearly every frame and the
/// atlas was rebuilt (glTexImage2D) 60 times a second: the driver's heap
/// grew by ~6 MB a minute. Now a glyph keeps its slot; new ones are added.
#[derive(Default)]
pub struct Slots {
    map: HashMap<char, u8>,
    list: Vec<char>,
    generation: u32,
}

impl Slots {
    /// Slots for every glyph `chars` needs; when they don't all fit, the
    /// table starts over (a new generation: the atlas is rebuilt once).
    fn admit(&mut self, chars: &[char]) {
        let missing = chars.iter().filter(|c| !self.map.contains_key(c)).count();
        if self.list.len() + missing > 255 {
            self.map.clear();
            self.list.clear();
            self.generation += 1;
        }
        for &c in chars {
            if !self.map.contains_key(&c) && self.list.len() < 255 {
                self.list.push(c);
                self.map.insert(c, self.list.len() as u8);
            }
        }
    }
}

/// Slots per atlas row and column: 255 glyphs and the empty slot 0.
const ATLAS_COLS: i32 = 16;

thread_local! {
    static BUFFERS: std::cell::RefCell<Vec<(Vec<u8>, Vec<u8>)>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl Drop for Frame {
    fn drop(&mut self) {
        let (a, b) = (std::mem::take(&mut self.a), std::mem::take(&mut self.b));
        BUFFERS.with(|p| {
            let mut p = p.borrow_mut();
            if p.len() < 3 {
                p.push((a, b));
            }
        });
    }
}

impl Frame {
    /// The frame for `g`, its glyphs in their slots (kept in `slots`).
    pub fn from_grid(
        g: &Grid,
        look: Look,
        look_width: f32,
        rect: [i32; 4],
        slots: &mut Slots,
    ) -> Frame {
        let n = g.cols * g.rows;
        // The glyphs in use (a few dozen of up to 96 000 cells): one pass,
        // runs of the same glyph skipped, no sort (letter art's shapes are
        // drawn by the shader: no atlas slot).
        let mut used: Vec<char> = Vec::with_capacity(64);
        let mut last = ' ';
        for c in g.cells.iter().take(n) {
            let ch = c.ch;
            if ch == last
                || ch == ' '
                || ch == '\u{a0}'
                || patch_visual::grid::shape_levels(ch).is_some()
            {
                continue;
            }
            last = ch;
            if !used.contains(&ch) {
                used.push(ch);
            }
        }
        slots.admit(&used);
        // The cell textures' buffers come back from the last frame (60 a
        // second of two 400 kB buffers otherwise).
        let (mut a, mut b) = BUFFERS.with(|p| p.borrow_mut().pop()).unwrap_or_default();
        a.clear();
        a.resize(n * 4, 0);
        b.clear();
        b.resize(n * 4, 0);
        for (i, c) in g.cells.iter().enumerate().take(n) {
            // A shape's 16 bits of ink: the low byte in the slot, the high
            // one in the background's red (a letter's cell has none).
            let shape = patch_visual::grid::shape_levels(c.ch);
            let slot = match shape {
                Some(l) => (l & 0xff) as u8,
                None if c.ch == ' ' || c.ch == '\u{a0}' => 0,
                None => slots.map.get(&c.ch).copied().unwrap_or(0),
            };
            let fg = c.fg.unwrap_or([0; 3]);
            let bg = match shape {
                Some(l) => [(l >> 8) as u8, 0, 0],
                None => c.bg.unwrap_or([0; 3]),
            };
            a[i * 4..i * 4 + 4].copy_from_slice(&[fg[0], fg[1], fg[2], slot]);
            let flags = (c.bg.is_some() && shape.is_none()) as u8
                | ((c.fg.is_some() as u8) << 1)
                | ((shape.is_some() as u8) << 2);
            b[i * 4..i * 4 + 4].copy_from_slice(&[bg[0], bg[1], bg[2], flags]);
        }
        Frame {
            cols: g.cols,
            rows: g.rows,
            a,
            b,
            glyphs: slots.list.clone(),
            generation: slots.generation,
            look,
            look_width,
            rect,
            generative: None,
            render_cap: None,
            performance: None,
        }
    }
}

const VERT: &str = r#"
in vec2 pos;
out vec2 uv;
void main() {
    uv = pos * 0.5 + 0.5;
    gl_Position = vec4(pos, 0.0, 1.0);
}"#;

/// The scene: each pixel's cell, its glyph's coverage, fg over bg.
const SCENE: &str = r#"
uniform sampler2D cells_a;
uniform sampler2D cells_b;
uniform sampler2D atlas;
uniform ivec2 grid;
uniform ivec2 cell;
uniform vec2 span;
uniform ivec2 origin;
uniform ivec2 size;
uniform vec3 ground;
uniform vec3 text;
uniform float lift;
uniform bool foreground_only;
out vec4 frag;
// Letter art: the ink (0…1) of sub-pixel s (2 across, 4 down a cell),
// 16 bits a cell (the slot byte and the background's red); outside the
// grid or in a cell without a shape, none.
float sub_px(ivec2 s) {
    ivec2 c = ivec2(s.x >> 1, s.y >> 2);
    if (s.x < 0 || s.y < 0 || c.x >= grid.x || c.y >= grid.y) return 0.0;
    vec4 b = texelFetch(cells_b, c, 0);
    int flags = int(b.a * 255.0 + 0.5);
    if ((flags & 4) == 0) return 0.0;
    int ink = int(texelFetch(cells_a, c, 0).a * 255.0 + 0.5) | (int(b.r * 255.0 + 0.5) << 8);
    return float((ink >> (((s.y & 3) * 2 + (s.x & 1)) * 2)) & 3) / 3.0;
}
void main() {
    ivec2 p = ivec2(gl_FragCoord.xy);
    p.y = size.y - 1 - p.y;
    vec3 col = ground;
    float coverage = 0.0;
    vec3 ink_colour = text;
    ivec2 q = p - origin;
    if (q.x >= 0 && q.y >= 0) {
        // The grid spans the canvas: cells of `span` pixels (fractional),
        // their glyphs drawn at the atlas's whole-pixel `cell`.
        vec2 f = vec2(q) / span;
        ivec2 c = ivec2(floor(f));
        if (c.x < grid.x && c.y < grid.y) {
            vec4 a = texelFetch(cells_a, c, 0);
            vec4 b = texelFetch(cells_b, c, 0);
            int flags = int(b.a * 255.0 + 0.5);
            if (foreground_only && (flags & 8) == 0) discard;
            vec3 bg = (flags & 1) != 0 ? b.rgb : ground;
            col = bg;
            int slot = int(a.a * 255.0 + 0.5);
            if ((flags & 4) != 0) {
                // The sub-pixels around this point, blended and cut at half:
                // curves and diagonals come out smooth, not in steps; the
                // cut's edge is anti-aliased over one screen pixel.
                vec3 fg = (flags & 2) != 0 ? a.rgb : text;
                fg = min(fg * lift, vec3(1.0));
                vec2 u = f * vec2(2.0, 4.0) - 0.5;
                ivec2 i0 = ivec2(floor(u));
                vec2 t = fract(u);
                float v = mix(mix(sub_px(i0), sub_px(i0 + ivec2(1, 0)), t.x),
                              mix(sub_px(i0 + ivec2(0, 1)), sub_px(i0 + ivec2(1, 1)), t.x), t.y);
                float px = 2.0 / max(span.x, 1.0);
                float cover = clamp((v - 0.5) / px + 0.5, 0.0, 1.0);
                col = mix(bg, fg, cover);
                coverage = cover; ink_colour = fg;
            } else if (slot > 0) {
                vec3 fg = (flags & 2) != 0 ? a.rgb : text;
                fg = min(fg * lift, vec3(1.0));
                ivec2 in_cell = min(ivec2(fract(f) * vec2(cell)), cell - 1);
                ivec2 at = ivec2((slot % 16) * cell.x + in_cell.x, (slot / 16) * cell.y + in_cell.y);
                float cover = texelFetch(atlas, at, 0).r;
                col = mix(bg, fg, cover);
                coverage = cover; ink_colour = fg;
            }
        }
    }
    if (foreground_only && coverage <= 0.0) discard;
    frag = foreground_only ? vec4(ink_colour, coverage) : vec4(col, 1.0);
}"#;

/// The bright parts: each `scale`² block's mean above the threshold.
const DOWN: &str = r#"
uniform sampler2D scene;
uniform int scale;
uniform float amount;
out vec4 frag;
void main() {
    ivec2 o = ivec2(gl_FragCoord.xy) * scale;
    ivec2 lim = textureSize(scene, 0) - 1;
    vec3 acc = vec3(0.0);
    for (int y = 0; y < scale; y++) {
        for (int x = 0; x < scale; x++) {
            acc += texelFetch(scene, min(o + ivec2(x, y), lim), 0).rgb;
        }
    }
    vec3 m = acc / float(scale * scale) * 255.0;
    // Stored halved: the glow can pass 255 before it's blurred.
    frag = vec4(max(m - 90.0, 0.0) * amount * 1.6 / 510.0, 1.0);
}"#;

/// A box blur of radius 3 along `dir`.
const BLUR: &str = r#"
uniform sampler2D src;
uniform ivec2 dir;
out vec4 frag;
void main() {
    ivec2 p = ivec2(gl_FragCoord.xy);
    ivec2 lim = textureSize(src, 0) - 1;
    vec3 acc = vec3(0.0);
    for (int i = -3; i <= 3; i++) {
        acc += texelFetch(src, clamp(p + dir * i, ivec2(0), lim), 0).rgb;
    }
    frag = vec4(acc / 7.0, 1.0);
}"#;

/// Scene + glow, red from the right and blue from the left.
const FINAL: &str = r#"
uniform sampler2D scene;
uniform sampler2D glow;
uniform float split;
uniform float use_glow;
uniform vec2 size;
in vec2 uv;
out vec4 frag;
vec3 at(vec2 u) {
    vec3 s = texture(scene, u).rgb;
    vec3 g = texture(glow, u).rgb * 2.0 * use_glow;
    return min(s + min(g, vec3(1.0)), vec3(1.0));
}
void main() {
    float dx = split / size.x;
    vec3 c = at(uv);
    float r = at(uv + vec2(dx, 0.0)).r;
    float b = at(uv - vec2(dx, 0.0)).b;
    frag = vec4(r, c.g, b, 1.0);
}"#;

/// The generative picture (`gen`) and the cells (`cells`) into one, as
/// `patch_visual::generative::composite` (whose GLSL is prepended).
const COMP: &str = r#"
uniform sampler2D cells;
uniform sampler2D gen;
uniform float gen_op;
uniform float gen_lift;
uniform float cell_op;
uniform int blend;
uniform int mask_on;
in vec2 uv;
out vec4 frag;
void main() {
    vec3 top = texture(cells, uv).rgb;
    vec3 base = texture(gen, uv).rgb;
    frag = vec4(composite(base * gen_lift, top, gen_op, cell_op, blend, mask_on != 0), 1.0);
}"#;

fn comp_source() -> String {
    format!("{}{COMP}", patch_visual::generative::COMPOSITE_GLSL)
}

struct Target {
    fbo: glow::Framebuffer,
    tex: glow::Texture,
    w: i32,
    h: i32,
}

pub struct Renderer {
    gl: glow::Context,
    header: &'static str,
    scene_prog: glow::Program,
    performance: Option<layers::Layers>,
    down_prog: glow::Program,
    blur_prog: glow::Program,
    final_prog: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    cells_a: glow::Texture,
    cells_b: glow::Texture,
    cells_size: (i32, i32),
    atlas: glow::Texture,
    /// Glyphs in the atlas by slot, and the cell size they were drawn at.
    /// Glyphs in the atlas so far (slots 1…), of which generation, and the
    /// cell size it was made for.
    atlas_count: usize,
    atlas_gen: u32,
    atlas_cell: (i32, i32),
    glyphs: Glyphs,
    scene: Option<Target>,
    small: Option<(Target, Target)>,
    comp_prog: glow::Program,
    /// The generative shader and the scene key it was built for.
    gen_prog: Option<(u64, glow::Program)>,
    /// Feedback ping-pong at the canvas size (the last frame is `ping[ix]`),
    /// and the composite.
    ping: Option<[Target; 2]>,
    ping_ix: usize,
    comp: Option<Target>,
    pub gen_error: Option<(u64, String)>,
}

fn tex(gl: &glow::Context, filter: i32) -> Result<glow::Texture, String> {
    // SAFETY: plain GL calls on the current context.
    unsafe {
        let t = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(t));
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, filter);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, filter);
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_S,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_T,
            glow::CLAMP_TO_EDGE as i32,
        );
        Ok(t)
    }
}

fn target(gl: &glow::Context, w: i32, h: i32) -> Result<Target, String> {
    // SAFETY: plain GL calls on the current context.
    unsafe {
        let t = tex(gl, glow::LINEAR as i32)?;
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            w,
            h,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
        let fbo = gl.create_framebuffer()?;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(t),
            0,
        );
        if gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
            return Err("framebuffer incomplete".into());
        }
        Ok(Target { fbo, tex: t, w, h })
    }
}

fn drop_target(gl: &glow::Context, t: Target) {
    // SAFETY: objects of this context.
    unsafe {
        gl.delete_framebuffer(t.fbo);
        gl.delete_texture(t.tex);
    }
}

fn program(gl: &glow::Context, header: &str, frag: &str) -> Result<glow::Program, String> {
    program_with_vertex(gl, header, VERT, frag)
}

fn program_with_vertex(gl: &glow::Context, header: &str, vertex: &str, frag: &str) -> Result<glow::Program, String> {
    // SAFETY: plain GL calls on the current context.
    unsafe {
        let p = gl.create_program()?;
        let mut shaders = vec![];
        for (kind, src) in [(glow::VERTEX_SHADER, vertex), (glow::FRAGMENT_SHADER, frag)] {
            let s = gl.create_shader(kind)?;
            gl.shader_source(s, &format!("{header}{src}"));
            gl.compile_shader(s);
            if !gl.get_shader_compile_status(s) {
                return Err(gl.get_shader_info_log(s));
            }
            gl.attach_shader(p, s);
            shaders.push(s);
        }
        gl.bind_attrib_location(p, 0, "pos");
        gl.link_program(p);
        if !gl.get_program_link_status(p) {
            return Err(gl.get_program_info_log(p));
        }
        for s in shaders {
            gl.detach_shader(p, s);
            gl.delete_shader(s);
        }
        Ok(p)
    }
}

/// The GL state Slint's renderer relies on, put back after each frame.
struct Saved {
    fbo: Option<glow::Framebuffer>,
    viewport: [i32; 4],
    scissor_box: [i32; 4],
    caps: [(u32, bool); 5],
    program: Option<glow::Program>,
    vao: Option<glow::VertexArray>,
    buffer: Option<glow::Buffer>,
    active: u32,
    textures: [Option<glow::Texture>; 3],
    unpack: i32,
    blend: [i32; 6],
    clear: [f32; 4],
    renderbuffer: Option<glow::Renderbuffer>,
    depth_func: u32,
    depth_mask: bool,
    depth_clear: f32,
}

impl Saved {
    fn take(gl: &glow::Context) -> Saved {
        // SAFETY: state queries on the current context.
        unsafe {
            let id = |p| std::num::NonZeroU32::new(gl.get_parameter_i32(p) as u32);
            let mut viewport = [0; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            let mut clear = [0.0; 4];
            gl.get_parameter_f32_slice(glow::COLOR_CLEAR_VALUE, &mut clear);
            let blend = [glow::BLEND_SRC_RGB, glow::BLEND_DST_RGB, glow::BLEND_SRC_ALPHA, glow::BLEND_DST_ALPHA, glow::BLEND_EQUATION_RGB, glow::BLEND_EQUATION_ALPHA].map(|p| gl.get_parameter_i32(p));
            let mut scissor_box = [0; 4];
            gl.get_parameter_i32_slice(glow::SCISSOR_BOX, &mut scissor_box);
            let caps = [
                glow::BLEND,
                glow::SCISSOR_TEST,
                glow::STENCIL_TEST,
                glow::DEPTH_TEST,
                glow::CULL_FACE,
            ]
            .map(|c| (c, gl.is_enabled(c)));
            let active = gl.get_parameter_i32(glow::ACTIVE_TEXTURE) as u32;
            let mut textures = [None; 3];
            for (i, t) in textures.iter_mut().enumerate() {
                gl.active_texture(glow::TEXTURE0 + i as u32);
                *t = id(glow::TEXTURE_BINDING_2D).map(glow::NativeTexture);
            }
            gl.active_texture(active);
            Saved {
                fbo: id(glow::FRAMEBUFFER_BINDING).map(glow::NativeFramebuffer),
                viewport,
                scissor_box,
                caps,
                program: id(glow::CURRENT_PROGRAM).map(glow::NativeProgram),
                vao: id(glow::VERTEX_ARRAY_BINDING).map(glow::NativeVertexArray),
                buffer: id(glow::ARRAY_BUFFER_BINDING).map(glow::NativeBuffer),
                active,
                textures,
                unpack: gl.get_parameter_i32(glow::UNPACK_ALIGNMENT),
                blend,
                clear,
                renderbuffer: id(glow::RENDERBUFFER_BINDING).map(glow::NativeRenderbuffer),
                depth_func: gl.get_parameter_i32(glow::DEPTH_FUNC) as u32,
                depth_mask: gl.get_parameter_i32(glow::DEPTH_WRITEMASK) != 0,
                depth_clear: gl.get_parameter_f32(glow::DEPTH_CLEAR_VALUE),
            }
        }
    }

    fn restore(self, gl: &glow::Context) {
        // SAFETY: state changes on the current context.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, self.fbo);
            gl.viewport(
                self.viewport[0],
                self.viewport[1],
                self.viewport[2],
                self.viewport[3],
            );
            gl.scissor(
                self.scissor_box[0],
                self.scissor_box[1],
                self.scissor_box[2],
                self.scissor_box[3],
            );
            for (c, on) in self.caps {
                if on { gl.enable(c) } else { gl.disable(c) }
            }
            gl.use_program(self.program);
            gl.bind_vertex_array(self.vao);
            gl.bind_buffer(glow::ARRAY_BUFFER, self.buffer);
            for (i, t) in self.textures.iter().enumerate() {
                gl.active_texture(glow::TEXTURE0 + i as u32);
                gl.bind_texture(glow::TEXTURE_2D, *t);
            }
            gl.active_texture(self.active);
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, self.unpack);
            gl.blend_func_separate(self.blend[0] as u32, self.blend[1] as u32, self.blend[2] as u32, self.blend[3] as u32);
            gl.blend_equation_separate(self.blend[4] as u32, self.blend[5] as u32);
            gl.clear_color(self.clear[0], self.clear[1], self.clear[2], self.clear[3]);
            gl.bind_renderbuffer(glow::RENDERBUFFER,self.renderbuffer);
            gl.depth_func(self.depth_func);gl.depth_mask(self.depth_mask);gl.clear_depth_f32(self.depth_clear);
        }
    }
}

impl Renderer {
    pub fn new(gl: glow::Context, font: &str) -> Result<Renderer, String> {
        let header: &'static str = if gl.version().is_embedded {
            "#version 300 es\nprecision highp float;\nprecision highp int;\n"
        } else {
            "#version 330 core\n"
        };
        let saved = Saved::take(&gl);
        let made = (|| {
            let scene_prog = program(&gl, header, SCENE)?;
            let down_prog = program(&gl, header, DOWN)?;
            let blur_prog = program(&gl, header, BLUR)?;
            let final_prog = program(&gl, header, FINAL)?;
            let comp_prog = program(&gl, header, &comp_source())?;
            // SAFETY: plain GL calls on the current context.
            let (vao, vbo) = unsafe {
                let vbo = gl.create_buffer()?;
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
                let quad: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
                let bytes: Vec<u8> = quad.iter().flat_map(|f| f.to_ne_bytes()).collect();
                gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, &bytes, glow::STATIC_DRAW);
                let vao = gl.create_vertex_array()?;
                gl.bind_vertex_array(Some(vao));
                gl.enable_vertex_attrib_array(0);
                gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
                (vao, vbo)
            };
            let cells_a = tex(&gl, glow::NEAREST as i32)?;
            let cells_b = tex(&gl, glow::NEAREST as i32)?;
            let atlas = tex(&gl, glow::NEAREST as i32)?;
            Ok::<_, String>((
                scene_prog, down_prog, blur_prog, final_prog, comp_prog, vao, vbo, cells_a, cells_b,
                atlas,
            ))
        })();
        let performance = match layers::Layers::new(&gl, header) {
            Ok(layers) => Some(layers),
            Err(e) => { eprintln!("patch visualiser: performance layers unavailable ({e})"); None }
        };
        saved.restore(&gl);
        let (
            scene_prog,
            down_prog,
            blur_prog,
            final_prog,
            comp_prog,
            vao,
            vbo,
            cells_a,
            cells_b,
            atlas,
        ) = made?;
        Ok(Renderer {
            gl,
            header,
            scene_prog,
            performance,
            down_prog,
            blur_prog,
            final_prog,
            vao,
            vbo,
            cells_a,
            cells_b,
            cells_size: (0, 0),
            atlas,
            atlas_count: 0,
            atlas_gen: u32::MAX,
            atlas_cell: (0, 0),
            glyphs: Glyphs::load(font),
            scene: None,
            small: None,
            comp_prog,
            gen_prog: None,
            ping: None,
            ping_ix: 0,
            comp: None,
            gen_error: None,
        })
    }

    /// Draw `f` into the window beneath Slint's items (the viewport Slint
    /// set is the window, physical pixels).
    pub fn draw(&mut self, f: &Frame) {
        let [rx, ry, rw, rh] = f.rect;
        if rw < 4 || rh < 4 || f.cols == 0 || f.rows == 0 {
            return;
        }
        let saved = Saved::take(&self.gl);
        let window_h = saved.viewport[3];
        // SAFETY: GL calls on the current context, with our own objects.
        unsafe { self.passes(f, rx, window_h - ry - rh, rw, rh, saved.fbo) };
        saved.restore(&self.gl);
    }

    unsafe fn passes(
        &mut self,
        f: &Frame,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        window_fbo: Option<glow::Framebuffer>,
    ) {
        let (output_w, output_h) = (w, h);
        let factor = f.render_cap.map_or(1.0, |cap| (cap as f32 / w.max(h) as f32).min(1.0));
        let (w, h) = (((w as f32 * factor).round() as i32).max(1), ((h as f32 * factor).round() as i32).max(1));
        // The grid across the whole canvas; glyphs at the nearest whole
        // cell size. The cell textures (and the atlas when needed) first.
        let (span_x, span_y) = (w as f32 / f.cols as f32, h as f32 / f.rows as f32);
        let (cw, ch) = if f.generative.as_ref().is_some_and(|g| g.points.is_some() && !g.cells) {
            // Image-only stages have no glyphs to rasterize. A 1-cell frame
            // must not allocate a window-sized 16×16 glyph atlas.
            (1, 1)
        } else {
            ((span_x.round() as i32).max(1), (span_y.round() as i32).max(1))
        };
        self.upload(f, cw, ch);
        let gl = &self.gl;
        unsafe {
            for c in [
                glow::BLEND,
                glow::SCISSOR_TEST,
                glow::STENCIL_TEST,
                glow::DEPTH_TEST,
                glow::CULL_FACE,
            ] {
                gl.disable(c);
            }
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.bind_vertex_array(Some(self.vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
        }
        // Targets at the canvas's size, the glow's at an eighth.
        let sc = if w >= 2400 { 8 } else { 4 };
        if self.scene.as_ref().is_none_or(|t| (t.w, t.h) != (w, h)) {
            if let Some(t) = self.scene.take() {
                drop_target(gl, t);
            }
            if let Some((a, b)) = self.small.take() {
                drop_target(gl, a);
                drop_target(gl, b);
            }
            for t in self.ping.take().into_iter().flatten().chain(self.comp.take()) {
                drop_target(gl, t);
            }
            let (sw, sh) = ((w / sc).max(1), (h / sc).max(1));
            match (target(gl, w, h), target(gl, sw, sh), target(gl, sw, sh)) {
                (Ok(s), Ok(a), Ok(b)) => {
                    self.scene = Some(s);
                    self.small = Some((a, b));
                }
                _ => return,
            }
        }
        // The generative layer's targets, made when first wanted.
        let generative = f.generative.as_ref().filter(|g| self.gen_error.as_ref().is_none_or(|e| e.0 != g.key));
        if generative.is_some() && self.ping.is_none() {
            match (target(gl, w, h), target(gl, w, h), target(gl, w, h)) {
                (Ok(a), Ok(b), Ok(c)) => {
                    self.ping = Some([a, b]);
                    self.comp = Some(c);
                }
                _ => return,
            }
        }
        if let Some(g) = generative
            && self.gen_prog.as_ref().is_none_or(|(k, _)| *k != g.key)
        {
            if let Some((_, p)) = self.gen_prog.take() {
                unsafe { gl.delete_program(p) };
            }
            match program_with_vertex(gl, self.header, g.points.as_ref().map_or(VERT, |points| points.vertex.as_str()), &g.frag) {
                Ok(p) => {
                    self.gen_prog = Some((g.key, p));
                    // A new scene starts without the last one's trails.
                    // (Slint's clear colour is put back.)
                    unsafe {
                        let mut was = [0.0f32; 4];
                        gl.get_parameter_f32_slice(glow::COLOR_CLEAR_VALUE, &mut was);
                        for t in self.ping.iter().flatten() {
                            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(t.fbo));
                            gl.viewport(0, 0, t.w, t.h);
                            gl.clear_color(0.0, 0.0, 0.0, 1.0);
                            gl.clear(glow::COLOR_BUFFER_BIT);
                        }
                        gl.clear_color(was[0], was[1], was[2], was[3]);
                    }
                }
                Err(e) => self.gen_error = Some((g.key, e)),
            }
        }
        let generative = generative.filter(|_| self.gen_prog.is_some());
        let scale = w as f32 / f.look_width.max(1.0);
        let ox = (f.look.shake * scale).round() as i32;
        let oy = -(f.look.bump * span_y) as i32;
        let (Some(scene), Some((sa, sb))) = (&self.scene, &self.small) else {
            return;
        };
        // What the glow and the final pass read: the cells, or the
        // composite of the generative picture and the cells.
        let mut picture = scene.tex;
        let uni = |p: glow::Program, name: &str| unsafe { gl.get_uniform_location(p, name) };
        let rgb = |c: [u8; 3]| {
            [
                c[0] as f32 / 255.0,
                c[1] as f32 / 255.0,
                c[2] as f32 / 255.0,
            ]
        };
        unsafe {
            // 1 · The scene.
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(scene.fbo));
            gl.viewport(0, 0, w, h);
            let p = self.scene_prog;
            gl.use_program(Some(p));
            gl.uniform_1_i32(uni(p,"foreground_only").as_ref(),0);
            for (i, (t, name)) in [
                (self.cells_a, "cells_a"),
                (self.cells_b, "cells_b"),
                (self.atlas, "atlas"),
            ]
            .into_iter()
            .enumerate()
            {
                gl.active_texture(glow::TEXTURE0 + i as u32);
                gl.bind_texture(glow::TEXTURE_2D, Some(t));
                gl.uniform_1_i32(uni(p, name).as_ref(), i as i32);
            }
            gl.uniform_2_i32(uni(p, "grid").as_ref(), f.cols as i32, f.rows as i32);
            gl.uniform_2_i32(uni(p, "cell").as_ref(), cw, ch);
            gl.uniform_2_f32(uni(p, "span").as_ref(), span_x, span_y);
            gl.uniform_2_i32(uni(p, "origin").as_ref(), ox, oy);
            gl.uniform_2_i32(uni(p, "size").as_ref(), w, h);
            // Under a generative picture the empty cells are black (the
            // compositor's "nothing here").
            let [r, g, b] = if generative.is_some() { [0.0; 3] } else { rgb(f.look.bg) };
            gl.uniform_3_f32(uni(p, "ground").as_ref(), r, g, b);
            let [r, g, b] = rgb(f.look.text);
            gl.uniform_3_f32(uni(p, "text").as_ref(), r, g, b);
            gl.uniform_1_f32(uni(p, "lift").as_ref(), 1.0 + f.look.pulse * 0.45);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            // 1b · The generative picture (the last frame as `prevBuffer`),
            // then composited with the cells.
            if let (Some(g), Some((_, gp)), Some(ping), Some(comp)) =
                (generative, &self.gen_prog, &self.ping, &self.comp)
            {
                let (last, next) = (&ping[self.ping_ix], &ping[1 - self.ping_ix]);
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(next.fbo));
                gl.viewport(0, 0, w, h);
                gl.use_program(Some(*gp));
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(last.tex));
                gl.uniform_1_i32(uni(*gp, "prevBuffer").as_ref(), 0);
                gl.uniform_1_f32(uni(*gp, "time").as_ref(), g.time);
                gl.uniform_2_f32(uni(*gp, "resolution").as_ref(), w as f32, h as f32);
                // ISF scenes' own names for the same (`patch_visual::isf`).
                gl.uniform_1_f32(uni(*gp, "isf_TIME").as_ref(), g.time);
                gl.uniform_2_f32(uni(*gp, "isf_RENDERSIZE").as_ref(), w as f32, h as f32);
                for (name, v) in &g.uniforms {
                    gl.uniform_1_f32(uni(*gp, name).as_ref(), *v);
                }
                if let Some(points) = &g.points {
                    // Rebuild the field from geometry on every frame. There is
                    // no image source or feedback clip underneath it.
                    gl.clear_color(0.008, 0.012, 0.020, 1.0);
                    gl.clear(glow::COLOR_BUFFER_BIT);
                    gl.enable(glow::BLEND);
                    gl.blend_equation(glow::FUNC_ADD);
                    gl.blend_func(glow::SRC_ALPHA, glow::ONE);
                    let desktop = !gl.version().is_embedded;
                    let sized = desktop && gl.is_enabled(glow::PROGRAM_POINT_SIZE);
                    if desktop { gl.enable(glow::PROGRAM_POINT_SIZE); }
                    gl.draw_arrays(glow::POINTS, 0, points.count);
                    if desktop && !sized { gl.disable(glow::PROGRAM_POINT_SIZE); }
                    gl.disable(glow::BLEND);
                } else {
                    gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                }
                self.ping_ix = 1 - self.ping_ix;
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(comp.fbo));
                let p = self.comp_prog;
                gl.use_program(Some(p));
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(scene.tex));
                gl.uniform_1_i32(uni(p, "cells").as_ref(), 0);
                gl.active_texture(glow::TEXTURE1);
                gl.bind_texture(glow::TEXTURE_2D, Some(next.tex));
                gl.uniform_1_i32(uni(p, "gen").as_ref(), 1);
                let c = &g.compositor;
                gl.uniform_1_f32(uni(p, "gen_op").as_ref(), c.generative_opacity);
                gl.uniform_1_f32(uni(p, "gen_lift").as_ref(),1.0+f.look.pulse*0.45);
                gl.uniform_1_f32(uni(p, "cell_op").as_ref(), if g.cells { c.cells_opacity } else { 0.0 });
                gl.uniform_1_i32(uni(p, "blend").as_ref(), c.blend.index());
                gl.uniform_1_i32(uni(p, "mask_on").as_ref(), (g.cells && c.mask) as i32);
                gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                gl.active_texture(glow::TEXTURE0);
                picture = comp.tex;
            }
            if let (Some(layers), Some(performance)) = (&mut self.performance, &f.performance) {
                let fbo = if picture == scene.tex { scene.fbo } else { self.comp.as_ref().unwrap().fbo };
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
                gl.viewport(0,0,w,h);
                layers.draw(gl,performance,w,h,fbo);
            }
            // 2 · The glow: bright parts at an eighth, blurred twice.
            let glow_on = f.look.glow > 0.01;
            if glow_on {
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(sa.fbo));
                gl.viewport(0, 0, sa.w, sa.h);
                let p = self.down_prog;
                gl.use_program(Some(p));
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(picture));
                gl.uniform_1_i32(uni(p, "scene").as_ref(), 0);
                gl.uniform_1_i32(uni(p, "scale").as_ref(), sc);
                gl.uniform_1_f32(
                    uni(p, "amount").as_ref(),
                    f.look.glow * (0.7 + 0.5 * f.look.pulse),
                );
                gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                let p = self.blur_prog;
                gl.use_program(Some(p));
                gl.uniform_1_i32(uni(p, "src").as_ref(), 0);
                for _ in 0..2 {
                    for (from, to, dir) in [(sa, sb, (1, 0)), (sb, sa, (0, 1))] {
                        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(to.fbo));
                        gl.bind_texture(glow::TEXTURE_2D, Some(from.tex));
                        gl.uniform_2_i32(uni(p, "dir").as_ref(), dir.0, dir.1);
                        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                    }
                }
            }
            // 3 · Into the window's canvas area.
            gl.bind_framebuffer(glow::FRAMEBUFFER, window_fbo);
            gl.viewport(x, y, output_w, output_h);
            let p = self.final_prog;
            gl.use_program(Some(p));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(picture));
            gl.uniform_1_i32(uni(p, "scene").as_ref(), 0);
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(sa.tex));
            gl.uniform_1_i32(uni(p, "glow").as_ref(), 1);
            let split = f.look.split * scale;
            gl.uniform_1_f32(
                uni(p, "split").as_ref(),
                if split.round() >= 1.0 && split < w as f32 / 4.0 {
                    split.round()
                } else {
                    0.0
                },
            );
            gl.uniform_1_f32(uni(p, "use_glow").as_ref(), if glow_on { 1.0 } else { 0.0 });
            gl.uniform_2_f32(uni(p, "size").as_ref(), w as f32, h as f32);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        }
    }

    /// The cell textures every frame; into the atlas only glyphs it doesn't
    /// hold yet (made anew only for a new cell size or slot generation).
    fn upload(&mut self, f: &Frame, cw: i32, ch: i32) {
        let gl = &self.gl;
        let (cols, rows) = (f.cols as i32, f.rows as i32);
        // SAFETY: GL calls on the current context, with our own textures.
        unsafe {
            // Rows of any width (the atlas is a byte a pixel).
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            for (t, data) in [(self.cells_a, &f.a), (self.cells_b, &f.b)] {
                gl.bind_texture(glow::TEXTURE_2D, Some(t));
                if self.cells_size != (cols, rows) {
                    gl.tex_image_2d(
                        glow::TEXTURE_2D,
                        0,
                        glow::RGBA8 as i32,
                        cols,
                        rows,
                        0,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(Some(data)),
                    );
                } else {
                    gl.tex_sub_image_2d(
                        glow::TEXTURE_2D,
                        0,
                        0,
                        0,
                        cols,
                        rows,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(Some(data)),
                    );
                }
            }
            self.cells_size = (cols, rows);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.atlas));
            if self.atlas_cell != (cw, ch) || self.atlas_gen != f.generation {
                // 16 × 16 slots, empty; allocated once for this cell size.
                let (aw, ah) = (ATLAS_COLS * cw, ATLAS_COLS * ch);
                let zeros = vec![0u8; (aw * ah) as usize];
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::R8 as i32,
                    aw,
                    ah,
                    0,
                    glow::RED,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(&zeros)),
                );
                self.atlas_cell = (cw, ch);
                self.atlas_gen = f.generation;
                self.atlas_count = 0;
            }
            // The glyphs it doesn't have yet, each into its own slot.
            for (i, c) in f.glyphs.iter().enumerate().skip(self.atlas_count) {
                let slot = i as i32 + 1;
                let cov = self.glyphs.coverage_at(*c, cw as usize, ch as usize);
                let (x0, y0) = ((slot % ATLAS_COLS) * cw, (slot / ATLAS_COLS) * ch);
                gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    x0,
                    y0,
                    cw,
                    ch,
                    glow::RED,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(&cov)),
                );
            }
            self.atlas_count = f.glyphs.len();
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        let gl = &self.gl;
        let _ = self.header;
        // SAFETY: objects of this context, which is still current during
        // Slint's teardown notification.
        if let Some(layers)=&self.performance { layers.destroy(gl); }
        unsafe {
            for p in [
                self.scene_prog,
                self.down_prog,
                self.blur_prog,
                self.final_prog,
                self.comp_prog,
            ] {
                gl.delete_program(p);
            }
            if let Some((_, p)) = self.gen_prog.take() {
                gl.delete_program(p);
            }
            gl.delete_vertex_array(self.vao);
            gl.delete_buffer(self.vbo);
            for t in [self.cells_a, self.cells_b, self.atlas] {
                gl.delete_texture(t);
            }
        }
        if let Some(t) = self.scene.take() {
            drop_target(gl, t);
        }
        if let Some((a, b)) = self.small.take() {
            drop_target(gl, a);
            drop_target(gl, b);
        }
        for t in self.ping.take().into_iter().flatten().chain(self.comp.take()) {
            drop_target(gl, t);
        }
    }
}

/// A window's extra rendering hook: a window has one rendering notifier
/// (Patch's frame watch, set on every window), which runs these too.
type Hook = Box<dyn FnMut(slint::RenderingState, &slint::GraphicsAPI)>;

thread_local! {
    static HOOKS: RefCell<HashMap<usize, Hook>> = RefCell::new(HashMap::new());
}

fn key(window: &slint::Window) -> usize {
    window as *const slint::Window as usize
}

/// Called from the window's rendering notifier.
pub fn run_hook(window_key: usize, state: slint::RenderingState, api: &slint::GraphicsAPI) {
    // Taken out while it runs (it may not re-enter the table).
    let hook = HOOKS.with(|h| h.borrow_mut().remove(&window_key));
    if let Some(mut f) = hook {
        let teardown = matches!(state, slint::RenderingState::RenderingTeardown);
        f(state, api);
        if !teardown {
            HOOKS.with(|h| {
                h.borrow_mut().entry(window_key).or_insert(f);
            });
        }
    }
}

/// The notifier's key for `window` (captured when it's set).
pub fn window_key(window: &slint::Window) -> usize {
    key(window)
}

/// Draw the Visualiser of `window` on its GPU from now on: the renderer
/// is made when the window's GL context is ready (at setup, or its next
/// frame), fed from `shared`.
pub fn install(window: &slint::Window, shared: Rc<Shared>) -> bool {
    let mut renderer: Option<Renderer> = None;
    // Made at setup, or at the first frame when the window was set up
    // before this was installed (it had drawn already): the context is
    // current in both.
    let make = |api: &slint::GraphicsAPI, renderer: &mut Option<Renderer>, shared: &Shared| {
        if renderer.is_some() || shared.failed.get() {
            return;
        }
        let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = api else {
            shared.failed.set(true);
            return;
        };
        // SAFETY: the loader of the context Slint made current.
        let gl = unsafe { glow::Context::from_loader_function_cstr(|s| get_proc_address(s)) };
        match Renderer::new(gl, &shared.font.borrow()) {
            Ok(r) => {
                shared.layers_ready.set(r.performance.is_some());
                *renderer = Some(r);
                shared.ready.set(true);
            }
            Err(e) => {
                eprintln!("patch visualiser: GPU drawing unavailable ({e}); drawing on the CPU");
                shared.failed.set(true);
            }
        }
    };
    let hook: Hook = Box::new(move |state, api| match state {
        slint::RenderingState::RenderingSetup => make(api, &mut renderer, &shared),
        slint::RenderingState::BeforeRendering => {
            make(api, &mut renderer, &shared);
            if let (Some(r), Some(f)) = (renderer.as_mut(), shared.frame.borrow().as_ref()) {
                r.draw(f);
                shared.layers_ms.set(r.performance.as_ref().and_then(|l| l.gpu_ms));
                if r.gen_error != *shared.gen_error.borrow() {
                    *shared.gen_error.borrow_mut() = r.gen_error.clone();
                }
            }
        }
        slint::RenderingState::RenderingTeardown => {
            renderer = None;
            shared.ready.set(false);
            shared.layers_ready.set(false);
        }
        _ => {}
    });
    HOOKS.with(|h| h.borrow_mut().insert(key(window), hook));
    window.request_redraw();
    true
}

#[cfg(test)]
mod tests {
    /// Every shader compiles as the driver will see it (desktop GLSL 3.30).
    /// A broken one isn't an error at run time: the Visualiser quietly
    /// draws on the CPU instead (it did, for a day, after an edit dropped
    /// `void main()`). Needs glslangValidator; skipped without it.
    #[test]
    fn every_shader_compiles() {
        let Ok(out) = std::process::Command::new("glslangValidator")
            .arg("--version")
            .output()
        else {
            eprintln!("glslangValidator not installed: skipped");
            return;
        };
        assert!(out.status.success());
        let dir = std::env::temp_dir().join(format!("patch-shaders-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, kind, src) in [
            ("vert", "vert", super::VERT),
            ("scene", "frag", super::SCENE),
            ("down", "frag", super::DOWN),
            ("blur", "frag", super::BLUR),
            ("final", "frag", super::FINAL),
            ("comp", "frag", &super::comp_source()),
            ("performer", "frag", include_str!("../ui/broadcast-performer.frag")),
            ("performer-points", "vert", include_str!("../ui/broadcast-performer.vert")),
            ("decks-points", "vert", super::layers::DECK_VERTEX),
            ("performer-solid", "vert", super::layers::SOLID_VERTEX),
            ("performer-solid-fragment", "frag", super::layers::SOLID_FRAGMENT),
            ("performer-film", "vert", super::layers::FILM_VERTEX),
            ("performer-film-fragment", "frag", super::layers::FILM_FRAGMENT),
            ("booth-vertex", "vert", super::layers::BOOTH_VERTEX),
            ("booth-fragment", "frag", super::layers::BOOTH_FRAGMENT),
        ]
        .into_iter()
        .map(|(n, k, s)| (n.to_string(), k, super::layers::surface(s)))
        .chain(patch_visual::isf::builtin().into_iter().map(|(n, src)| {
            let c = patch_visual::isf::parse(src).and_then(|i| patch_visual::isf::compile(&i, &Default::default()));
            (format!("isf-{n}"), "frag", c.unwrap().frag)
        }))
        {
            let file = dir.join(format!("{name}.{kind}"));
            std::fs::write(&file, format!("#version 330 core\n{src}")).unwrap();
            let r = std::process::Command::new("glslangValidator")
                .arg(&file)
                .output()
                .unwrap();
            assert!(
                r.status.success(),
                "{name}: {}",
                String::from_utf8_lossy(&r.stdout)
            );
        }
        let linked = std::process::Command::new("glslangValidator")
            .arg("-l").arg(dir.join("performer-points.vert")).arg(dir.join("performer.frag"))
            .output().unwrap();
        assert!(linked.status.success(), "performer vertex/fragment link: {} {}", String::from_utf8_lossy(&linked.stdout), String::from_utf8_lossy(&linked.stderr));
        let _ = std::fs::remove_dir_all(dir);
    }
}
