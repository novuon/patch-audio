//! The broadcast's picture, taken from a window as it is drawn: its GPU
//! stage with the UI on top, the frame the screen shows. After a frame is
//! drawn (before it is presented) the back buffer is scaled on the GPU into
//! a 1280×720 picture, upright and letterboxed, and read into a ring of
//! pixel buffers; a finished one is copied to the broadcast a frame or two
//! later. The UI thread never waits on the GPU, and nothing runs at all
//! unless a broadcast wants this window's picture.

use glow::HasContext;
use patch_broadcast::video::{FPS, FRAME_BYTES, FrameSink, HEIGHT, WIDTH};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

thread_local! {
    /// The window (its notifier key) whose frames go out, and where to.
    static TARGET: RefCell<Option<(usize, Rc<FrameSink>)>> = const { RefCell::new(None) };
    /// The GL side, per window that has been captured (its context's).
    static GRABS: RefCell<HashMap<usize, Grab>> = RefCell::new(HashMap::new());
}

/// Send `window`'s frames to `sink` from its next frame on (`None`: stop).
pub fn set_target(target: Option<(usize, Rc<FrameSink>)>) {
    TARGET.with(|t| {
        let mut t = t.borrow_mut();
        let same = match (t.as_ref(), target.as_ref()) {
            (Some((a, x)), Some((b, y))) => a == b && Rc::ptr_eq(x, y),
            (None, None) => true,
            _ => false,
        };
        if !same {
            *t = target;
        }
    });
}

/// From a window's rendering notifier: its finished frame, when wanted.
/// `size` is the window's size in pixels (asked only then).
pub fn run(
    window_key: usize,
    state: &slint::RenderingState,
    api: &slint::GraphicsAPI,
    size: impl FnOnce() -> Option<(u32, u32)>,
) {
    match state {
        slint::RenderingState::AfterRendering => {
            let sink = TARGET.with(|t| {
                t.borrow()
                    .as_ref()
                    .filter(|(k, _)| *k == window_key)
                    .map(|(_, s)| s.clone())
            });
            GRABS.with(|g| {
                let mut grabs = g.borrow_mut();
                let Some(sink) = sink else {
                    // Not (or no longer) the picture: its GL objects go
                    // while its context is current.
                    if let Some(old) = grabs.remove(&window_key) {
                        old.delete();
                    }
                    return;
                };
                let grab = match grabs.entry(window_key) {
                    std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                    std::collections::hash_map::Entry::Vacant(e) => {
                        let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = api else {
                            return;
                        };
                        // SAFETY: the loader of the context Slint made current.
                        let gl = unsafe {
                            glow::Context::from_loader_function_cstr(|s| get_proc_address(s))
                        };
                        match Grab::new(gl) {
                            Ok(grab) => e.insert(grab),
                            Err(err) => {
                                eprintln!("patch broadcast: the picture can't be taken here ({err})");
                                return;
                            }
                        }
                    }
                };
                if grab.failed || !grab.due() {
                    return;
                }
                if let Some((w, h)) = size() {
                    grab.frame(w as i32, h as i32, &mut |src| {
                        sink.submit(|dst| dst.copy_from_slice(src))
                    });
                }
            });
        }
        slint::RenderingState::RenderingTeardown => {
            GRABS.with(|g| {
                if let Some(old) = g.borrow_mut().remove(&window_key) {
                    old.delete();
                }
            });
        }
        _ => {}
    }
}

const RING: usize = 3;

struct Grab {
    gl: glow::Context,
    /// The picture: 1280×720.
    picture: super::Target,
    /// A multisampled window is resolved at its own size first (a scaled
    /// blit from one is not allowed).
    resolve: Option<(super::Target, i32, i32)>,
    buffers: [glow::Buffer; RING],
    fences: [Option<glow::Fence>; RING],
    /// Ring slots read and not yet handed out, oldest first.
    pending: VecDeque<usize>,
    due: Instant,
    failed: bool,
}

impl Grab {
    fn new(gl: glow::Context) -> Result<Grab, String> {
        unsafe {
            let picture = super::target(&gl, WIDTH as i32, HEIGHT as i32)?;
            let mut buffers = Vec::with_capacity(RING);
            for _ in 0..RING {
                let b = gl.create_buffer()?;
                gl.bind_buffer(glow::PIXEL_PACK_BUFFER, Some(b));
                gl.buffer_data_size(glow::PIXEL_PACK_BUFFER, FRAME_BYTES as i32, glow::STREAM_READ);
                buffers.push(b);
            }
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            Ok(Grab {
                picture,
                resolve: None,
                buffers: buffers.try_into().map_err(|_| "pixel buffers")?,
                fences: [None; RING],
                pending: VecDeque::new(),
                due: Instant::now(),
                failed: false,
                gl,
            })
        }
    }

    /// 30 a second on average, whatever rate the window draws at.
    fn due(&mut self) -> bool {
        let now = Instant::now();
        if now < self.due {
            return false;
        }
        let period = Duration::from_secs(1) / FPS;
        self.due += period;
        if self.due < now {
            self.due = now + period;
        }
        true
    }

    /// The `w`×`h` frame just drawn into the window's back buffer; a
    /// finished earlier one goes to `out` (FRAME_BYTES, top row first).
    fn frame(&mut self, w: i32, h: i32, out: &mut dyn FnMut(&[u8])) {
        if w <= 0 || h <= 0 {
            return;
        }
        let gl = &self.gl;
        unsafe {
            // Slint's state, put back as it was.
            let read_fb = gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING);
            let draw_fb = gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING);
            let pack = gl.get_parameter_i32(glow::PIXEL_PACK_BUFFER_BINDING);
            let scissor = gl.is_enabled(glow::SCISSOR_TEST);
            let mask = gl.get_parameter_bool_array::<4>(glow::COLOR_WRITEMASK);
            let mut clear = [0f32; 4];
            gl.get_parameter_f32_slice(glow::COLOR_CLEAR_VALUE, &mut clear);

            // 1. What the GPU has finished goes out: the newest of them.
            let mut ready = None;
            while let Some(&slot) = self.pending.front() {
                let Some(fence) = self.fences[slot] else {
                    self.pending.pop_front();
                    continue;
                };
                let r = gl.client_wait_sync(fence, 0, 0);
                if r != glow::ALREADY_SIGNALED && r != glow::CONDITION_SATISFIED {
                    break;
                }
                gl.delete_sync(fence);
                self.fences[slot] = None;
                self.pending.pop_front();
                ready = Some(slot);
            }
            if let Some(slot) = ready {
                gl.bind_buffer(glow::PIXEL_PACK_BUFFER, Some(self.buffers[slot]));
                let ptr = gl.map_buffer_range(
                    glow::PIXEL_PACK_BUFFER,
                    0,
                    FRAME_BYTES as i32,
                    glow::MAP_READ_BIT,
                );
                if !ptr.is_null() {
                    // SAFETY: a mapping of the whole buffer, FRAME_BYTES long,
                    // read before it is unmapped.
                    out(std::slice::from_raw_parts(ptr as *const u8, FRAME_BYTES));
                    gl.unmap_buffer(glow::PIXEL_PACK_BUFFER);
                }
            }

            // 2. This frame, scaled and upright, into the picture.
            gl.disable(glow::SCISSOR_TEST);
            gl.color_mask(true, true, true, true);
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
            if gl.get_parameter_i32(glow::SAMPLE_BUFFERS) > 0 {
                if self.resolve.as_ref().is_none_or(|(_, rw, rh)| (*rw, *rh) != (w, h)) {
                    if let Some((old, _, _)) = self.resolve.take() {
                        super::drop_target(gl, old);
                    }
                    match super::target(gl, w, h) {
                        Ok(t) => self.resolve = Some((t, w, h)),
                        Err(_) => self.failed = true,
                    }
                }
                if let Some((t, _, _)) = &self.resolve {
                    gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(t.fbo));
                    gl.blit_framebuffer(0, 0, w, h, 0, 0, w, h, glow::COLOR_BUFFER_BIT, glow::NEAREST);
                    gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(t.fbo));
                }
            }
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(self.picture.fbo));
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            let [x0, y0, x1, y1] = fit(w, h);
            // Rows come back bottom first: drawn upside down here, they
            // read top first, as the encoder wants them.
            gl.blit_framebuffer(0, 0, w, h, x0, y1, x1, y0, glow::COLOR_BUFFER_BIT, glow::LINEAR);

            // 3. Read into a free slot of the ring (when all wait on the
            // GPU, the oldest is dropped), without waiting for it.
            let slot = match (0..RING).find(|s| !self.pending.contains(s)) {
                Some(s) => s,
                None => {
                    let s = self.pending.pop_front().unwrap();
                    if let Some(f) = self.fences[s].take() {
                        gl.delete_sync(f);
                    }
                    s
                }
            };
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(self.picture.fbo));
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, Some(self.buffers[slot]));
            gl.read_pixels(
                0,
                0,
                WIDTH as i32,
                HEIGHT as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::BufferOffset(0),
            );
            match gl.fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0) {
                Ok(f) => {
                    self.fences[slot] = Some(f);
                    self.pending.push_back(slot);
                }
                Err(_) => self.failed = true,
            }

            let bind = |target: u32, raw: i32| {
                let fb = std::num::NonZeroU32::new(raw as u32).map(glow::NativeFramebuffer);
                gl.bind_framebuffer(target, fb);
            };
            bind(glow::READ_FRAMEBUFFER, read_fb);
            bind(glow::DRAW_FRAMEBUFFER, draw_fb);
            gl.bind_buffer(
                glow::PIXEL_PACK_BUFFER,
                std::num::NonZeroU32::new(pack as u32).map(glow::NativeBuffer),
            );
            if scissor {
                gl.enable(glow::SCISSOR_TEST);
            }
            gl.color_mask(mask[0], mask[1], mask[2], mask[3]);
            gl.clear_color(clear[0], clear[1], clear[2], clear[3]);
            let err = gl.get_error();
            if err != glow::NO_ERROR {
                eprintln!("patch broadcast: taking the picture failed (GL error {err:#x}); the stream shows black");
                self.failed = true;
            }
        }
    }

    fn delete(self) {
        unsafe {
            for f in self.fences.into_iter().flatten() {
                self.gl.delete_sync(f);
            }
            for b in self.buffers {
                self.gl.delete_buffer(b);
            }
            if let Some((t, _, _)) = self.resolve {
                super::drop_target(&self.gl, t);
            }
            super::drop_target(&self.gl, self.picture);
        }
    }
}

/// Where a `w`×`h` frame sits in the picture: as large as fits, centred,
/// its shape kept (bars where it is narrower or wider than 16:9).
fn fit(w: i32, h: i32) -> [i32; 4] {
    let (pw, ph) = (WIDTH as i64, HEIGHT as i64);
    let (w, h) = (w.max(1) as i64, h.max(1) as i64);
    let (fw, fh) = if w * ph >= h * pw { (pw, (h * pw / w).max(1)) } else { ((w * ph / h).max(1), ph) };
    let (x, y) = ((pw - fw) / 2, (ph - fh) / 2);
    [x as i32, y as i32, (x + fw) as i32, (y + fh) as i32]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_keep_their_shape() {
        assert_eq!(fit(1920, 1080), [0, 0, 1280, 720]);
        assert_eq!(fit(3840, 2160), [0, 0, 1280, 720]);
        // Ultrawide: bars above and below.
        assert_eq!(fit(3440, 1440), [0, 92, 1280, 627]);
        // Square: bars at the sides.
        assert_eq!(fit(1000, 1000), [280, 0, 1000, 720]);
    }

    /// On a real GPU (opt-in, like the other GPU tests):
    /// EGL_PLATFORM=surfaceless cargo test -p patch-surfaces
    /// picture_is_upright_scaled_and_never_waits -- --ignored --nocapture
    #[test]
    #[ignore]
    fn picture_is_upright_scaled_and_never_waits() {
        use super::super::performance_test::{Egl, proc_address};
        let _egl = Egl::new();
        let gl = unsafe { glow::Context::from_loader_function_cstr(proc_address) };
        let gl2 = unsafe { glow::Context::from_loader_function_cstr(proc_address) };
        let mut grab = Grab::new(gl).unwrap();
        let sink = TestSink::default();
        // The window: 1920×1080, red on top, blue at the bottom, a green
        // square at the top left.
        let draw = |gl: &glow::Context| unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.viewport(0, 0, 1920, 1080);
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(0, 540, 1920, 540);
            gl.clear_color(1.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.scissor(0, 0, 1920, 540);
            gl.clear_color(0.0, 0.0, 1.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.scissor(0, 1080 - 108, 192, 108);
            gl.clear_color(0.0, 1.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            // A state the UI left behind (it must still be there after).
            gl.scissor(10, 10, 20, 20);
            gl.clear_color(0.25, 0.5, 0.75, 1.0);
        };
        let mut times = Vec::new();
        for _ in 0..12 {
            draw(&gl2);
            let t = Instant::now();
            grab.frame(1920, 1080, &mut |f| sink.put(f));
            times.push(t.elapsed().as_secs_f64() * 1e3);
            unsafe {
                assert!(gl2.is_enabled(glow::SCISSOR_TEST));
                let mut c = [0f32; 4];
                gl2.get_parameter_f32_slice(glow::COLOR_CLEAR_VALUE, &mut c);
                assert_eq!(c, [0.25, 0.5, 0.75, 1.0]);
                gl2.finish();
            }
        }
        let frame = sink.last().expect("no frame came back");
        let px = |x: usize, y: usize| {
            let i = (y * WIDTH as usize + x) * 4;
            [frame[i], frame[i + 1], frame[i + 2]]
        };
        assert_eq!(px(640, 100), [255, 0, 0], "top is red");
        assert_eq!(px(640, 620), [0, 0, 255], "bottom is blue");
        assert_eq!(px(20, 20), [0, 255, 0], "the green square is at the top left");
        assert_eq!(px(400, 20), [255, 0, 0]);
        times.sort_by(f64::total_cmp);
        eprintln!("picture grab on the UI thread: median {:.3} ms, max {:.3} ms", times[6], times[11]);
        // A square window: bars at the sides.
        draw(&gl2);
        for _ in 0..4 {
            grab.frame(1080, 1080, &mut |f| sink.put(f));
            unsafe { gl2.finish() };
        }
        let frame = sink.last().unwrap();
        let px = |x: usize, y: usize| frame[(y * WIDTH as usize + x) * 4..][..3].to_vec();
        assert_eq!(px(100, 360), [0, 0, 0], "a bar left of a square window");
        assert_eq!(px(640, 100), [255, 0, 0]);
        grab.delete();
    }

    #[derive(Default)]
    struct TestSink(RefCell<Vec<Vec<u8>>>);
    impl TestSink {
        fn put(&self, f: &[u8]) {
            self.0.borrow_mut().push(f.to_vec());
        }
        fn last(&self) -> Option<Vec<u8>> {
            self.0.borrow().last().cloned()
        }
    }
}
