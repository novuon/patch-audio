//! Opt-in real-context test of the production compositor, not a demo renderer.
//! EGL_PLATFORM=surfaceless cargo test --release -p patch-surfaces
//! performance_layers_gpu -- --ignored --nocapture --test-threads=1
use super::*;
use patch_visual::performance::{Frame as Performance, MAX_SOURCES, Scene, Settings, Source};
use std::ffi::{c_char, c_void};

#[link(name = "EGL")]
unsafe extern "C" {
    fn eglGetDisplay(native: *mut c_void) -> *mut c_void;
    fn eglInitialize(d: *mut c_void, major: *mut i32, minor: *mut i32) -> u32;
    fn eglBindAPI(api: u32) -> u32;
    fn eglChooseConfig(
        d: *mut c_void,
        attrs: *const i32,
        c: *mut *mut c_void,
        n: i32,
        count: *mut i32,
    ) -> u32;
    fn eglCreatePbufferSurface(d: *mut c_void, c: *mut c_void, attrs: *const i32) -> *mut c_void;
    fn eglCreateContext(
        d: *mut c_void,
        c: *mut c_void,
        share: *mut c_void,
        attrs: *const i32,
    ) -> *mut c_void;
    fn eglMakeCurrent(
        d: *mut c_void,
        draw: *mut c_void,
        read: *mut c_void,
        ctx: *mut c_void,
    ) -> u32;
    fn eglGetProcAddress(name: *const c_char) -> *const c_void;
    fn eglDestroyContext(d: *mut c_void, ctx: *mut c_void) -> u32;
    fn eglDestroySurface(d: *mut c_void, s: *mut c_void) -> u32;
    fn eglTerminate(d: *mut c_void) -> u32;
}
pub(super) struct Egl {
    d: *mut c_void,
    s: *mut c_void,
    c: *mut c_void,
}
/// A GL function of the current EGL context (for `glow`).
pub(super) fn proc_address(name: &std::ffi::CStr) -> *const c_void {
    unsafe { eglGetProcAddress(name.as_ptr()) }
}
impl Egl {
    /// A 1920×1080 pbuffer with an OpenGL 3.3 context, current.
    pub(super) fn new() -> Self {
        unsafe {
            let d = eglGetDisplay(std::ptr::null_mut());
            assert!(!d.is_null());
            let (mut a, mut b) = (0, 0);
            assert_ne!(eglInitialize(d, &mut a, &mut b), 0);
            assert_ne!(eglBindAPI(0x30a2), 0); // OpenGL, not a second abstraction.
            let attrs = [
                0x3033, 1, 0x3040, 8, 0x3024, 8, 0x3023, 8, 0x3022, 8, 0x3021, 8, 0x3038,
            ];
            let mut config = std::ptr::null_mut();
            let mut n = 0;
            assert_ne!(
                eglChooseConfig(d, attrs.as_ptr(), &mut config, 1, &mut n),
                0
            );
            assert!(n > 0);
            let s =
                eglCreatePbufferSurface(d, config, [0x3057, 1920, 0x3056, 1080, 0x3038].as_ptr());
            assert!(!s.is_null());
            let c = eglCreateContext(
                d,
                config,
                std::ptr::null_mut(),
                [0x3098, 3, 0x30fb, 3, 0x3038].as_ptr(),
            );
            assert!(!c.is_null());
            assert_ne!(eglMakeCurrent(d, s, s, c), 0);
            Self { d, s, c }
        }
    }
}
impl Drop for Egl {
    fn drop(&mut self) {
        unsafe {
            eglMakeCurrent(
                self.d,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            eglDestroyContext(self.d, self.c);
            eglDestroySurface(self.d, self.s);
            eglTerminate(self.d);
        }
    }
}
fn pixels(gl: &glow::Context) -> Vec<u8> {
    let mut p = vec![0; 1920 * 1080 * 4];
    unsafe {
        gl.read_pixels(
            0,
            0,
            1920,
            1080,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut p)),
        );
    }
    p
}
fn save(name: &str, p: &[u8]) {
    let Some(dir) = std::env::var_os("PATCH_VISUAL_GPU_SHOTS") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut out = b"P6\n1920 1080\n255\n".to_vec();
    for row in p.chunks_exact(1920 * 4).rev() {
        for px in row.chunks_exact(4) {
            out.extend_from_slice(&px[..3]);
        }
    }
    std::fs::write(dir.join(format!("{name}.ppm")), out).unwrap();
}
#[test]
#[ignore = "requires an EGL/OpenGL driver; renders production compositor at 1080p"]
fn performance_layers_gpu() {
    let _egl = Egl::new();
    let gl = unsafe { glow::Context::from_loader_function_cstr(|s| eglGetProcAddress(s.as_ptr())) };
    unsafe {
        eprintln!("GPU: {}", gl.get_parameter_string(glow::RENDERER));
    }
    let mut r = Renderer::new(gl, "monospace").unwrap();
    assert!(r.performance.is_some());
    let mut grid = Grid::blank(160, 45);
    // Real legacy choreography cells coexist with both layers.
    for (i, c) in grid.cells.iter_mut().enumerate() {
        if i % 53 == 0 {
            c.ch = '·';
            c.fg = Some([15, 28, 40]);
        }
    }
    let look = Look {
        bg: [0, 0, 0],
        text: [180, 220, 240],
        pulse: 0.0,
        glow: 0.2,
        bump: 0.0,
        split: 0.0,
        shake: 0.0,
    };
    let mut frame = Frame::from_grid(
        &grid,
        look,
        1920.0,
        [0, 0, 1920, 1080],
        &mut Slots::default(),
    );
    let mut scene = Scene {
        seconds: 12.0,
        beat: Some(24.2),
        palette: patch_visual::performance::BRAND,
        bands: [0.7, 0.4, 0.2],
        kick: 1.0,
        ..Default::default()
    };
    scene.intent.intensity = 0.7;
    let mut perf = Performance {
        scene,
        sources: [Source::default(); MAX_SOURCES],
        count: 2,
        settings: Settings {
            decks: true,
            performer: true,
            ..Default::default()
        },
        appearance: 0.6,
        point_step: 1,
        body: Default::default(),
    };
    perf.sources[0] = Source {
        active: true,
        level: 0.7,
        meter: [0.7, 0.64],
        beat: Some(24.2),
        weight: 0.85,
        gain: 0.909,
        progress: 0.4,
        ..Default::default()
    };
    perf.sources[1] = Source {
        active: true,
        level: 0.45,
        meter: [0.42, 0.47],
        beat: Some(8.2),
        weight: 0.4,
        gain: 0.7,
        progress: 0.1,
        ..Default::default()
    };
    r.draw(&frame);
    let legacy = pixels(&r.gl);
    frame.performance = Some(perf);
    r.draw(&frame);
    let both = pixels(&r.gl);
    assert_ne!(legacy, both);
    save("both", &both);
    perf.settings.performer = false;
    frame.performance = Some(perf);
    r.draw(&frame);
    let decks = pixels(&r.gl);
    assert_ne!(both, decks);
    save("decks", &decks);
    perf.sources[0].gain = 0.25;
    perf.scene.crossfader = 0.9;
    frame.performance = Some(perf);
    r.draw(&frame);
    assert_ne!(
        decks,
        pixels(&r.gl),
        "the booth must show actual fader positions"
    );
    perf.sources[0].gain = 0.909;
    perf.scene.crossfader = 0.0;
    perf.sources[0].eq[0] = -1.0;
    frame.performance = Some(perf);
    r.draw(&frame);
    save("decks-bass-kill", &pixels(&r.gl));
    assert_ne!(
        decks,
        pixels(&r.gl),
        "actual bass kill changes low geometry"
    );
    perf.sources[0].eq[0] = 0.0;
    perf.sources[0].fx = 0.8;
    frame.performance = Some(perf);
    r.draw(&frame);
    assert_ne!(decks, pixels(&r.gl), "enabled FX depth changes the source");
    perf.sources[0].fx = 0.0;
    perf.settings.deck_opacity = 0.35;
    frame.performance = Some(perf);
    r.draw(&frame);
    assert_ne!(decks, pixels(&r.gl), "partial opacity changes the layer");
    perf.settings.deck_opacity = 0.85;
    perf.settings.performer = true;
    perf.settings.decks = false;
    frame.performance = Some(perf);
    r.draw(&frame);
    let face = pixels(&r.gl);
    save("performer", &face);
    // The solid silhouette with the booth, for review.
    let mut solid = perf;
    solid.appearance = 0.0;
    solid.settings.decks = true;
    frame.performance = Some(solid);
    r.draw(&frame);
    save("solid", &pixels(&r.gl));
    // The same over a lit backdrop, as the Director's scene lights it.
    let dark = frame.look.bg;
    frame.look.bg = [38, 52, 78];
    r.draw(&frame);
    save("solid-lit", &pixels(&r.gl));
    // What the layers cost this GPU (read a frame or two later).
    for _ in 0..6 {
        r.draw(&frame);
        let _ = pixels(&r.gl);
    }
    eprintln!(
        "layers GPU cost: {:?} ms",
        r.performance.as_ref().and_then(|l| l.gpu_ms)
    );
    // The Omarchy chest mark.
    let mut marked = solid;
    marked.settings.emblem = patch_visual::performance::Emblem::Omarchy;
    frame.performance = Some(marked);
    r.draw(&frame);
    save("emblem-omarchy", &pixels(&r.gl));
    // Each gesture, for review.
    for (name, gesture, gesture2, motion) in [
        ("pose-ear", [1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 0.0]),
        ("pose-air", [0.0, 0.0, 0.0, 1.0], [0.0; 4], [0.012, 0.05, 0.0, 0.0]),
        ("pose-clap", [0.0; 4], [1.0, 0.0, 0.0, 0.0], [0.0; 4]),
        ("pose-point", [1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, -0.06, 0.01, 0.0]),
        // Half way up: a gesture passes through without the arm changing length.
        ("pose-half", [0.5, 0.0, 0.0, 0.5], [0.0; 4], [0.0; 4]),
    ] {
        let mut posed = solid;
        posed.body.gesture = gesture;
        posed.body.gesture2 = gesture2;
        posed.body.rim = 0.7;
        [posed.body.nod, posed.body.tilt, posed.body.sway, posed.body.bounce] = motion;
        frame.performance = Some(posed);
        // Settled, past any change of performer (the morph takes 2 s).
        for _ in 0..75 {
            r.draw(&frame);
            std::thread::sleep(std::time::Duration::from_millis(33));
        }
        save(name, &pixels(&r.gl));
    }
    // The filmed DJ, where its clips are installed.
    let mut filmed = solid;
    filmed.settings.me = true;
    frame.performance = Some(filmed);
    for n in 0..150 {
        r.draw(&frame);
        std::thread::sleep(std::time::Duration::from_millis(33));
        if n % 30 == 29 {
            save(&format!("film-{}", n / 30), &pixels(&r.gl));
        }
    }
    // The same pose in each look, and all three mixed.
    for (name, look) in [
        ("look-shadow", [0.0, 1.0, 0.0]),
        ("look-points", [0.0, 0.0, 1.0]),
        ("look-mix", [0.5, 0.2, 0.6]),
    ] {
        let mut looked = filmed;
        looked.settings.look = look;
        frame.performance = Some(looked);
        for _ in 0..40 {
            r.draw(&frame);
            std::thread::sleep(std::time::Duration::from_millis(33));
        }
        save(name, &pixels(&r.gl));
    }
    // The Director's choreography, for review: into a breakdown (a distorted
    // change to the see-through shadow, the headphones coming off), then a drop.
    if std::env::var_os("PATCH_VISUAL_GPU_SHOTS").is_some() {
        use patch_core::visual_intent::Section;
        let mut auto = filmed;
        auto.settings.automatic = true;
        auto.scene.intent.section = Section::Breakdown;
        frame.performance = Some(auto);
        for n in 0..390 {
            r.draw(&frame);
            std::thread::sleep(std::time::Duration::from_millis(33));
            if n == 8 {
                save("change-glitch", &pixels(&r.gl));
            }
            if n % 60 == 59 {
                save(&format!("breakdown-{}", n / 60), &pixels(&r.gl));
            }
        }
        auto.scene.intent.section = Section::Drop;
        frame.performance = Some(auto);
        for n in 0..20 {
            r.draw(&frame);
            std::thread::sleep(std::time::Duration::from_millis(33));
            if n == 8 {
                save("drop-glitch", &pixels(&r.gl));
            }
        }
    }
    // A minute and a half through the real compositor, clips and groove, at
    // 124 BPM, as a video for review: the three performers by hand, then verse,
    // build, drop and breakdown as the Director plays them.
    if let Some(path) = std::env::var_os("PATCH_VISUAL_GPU_VIDEO") {
        use patch_core::visual_intent::Section;
        use std::io::Write;
        let mut video = std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-s", "1920x1080", "-r", "30", "-i", "-", "-vf", "vflip"])
            .args(["-c:v", "libx264", "-crf", "20", "-preset", "veryfast"])
            .args(["-pix_fmt", "yuv420p", "-movflags", "+faststart"])
            .arg(path)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = video.stdin.take().unwrap();
        let mut groove = patch_visual::groove::Groove::default();
        let mut live = filmed;
        live.settings.automatic = true;
        let started = std::time::Instant::now();
        for n in 0..2700 {
            let due = std::time::Duration::from_secs_f32(n as f32 / 30.0);
            if let Some(wait) = due.checked_sub(started.elapsed()) {
                std::thread::sleep(wait);
            }
            // Video time, one frame per draw: however slowly this machine
            // draws, the recording plays at the music's speed.
            let t = n as f32 / 30.0;
            r.performance.as_mut().unwrap().clock = Some(started + due);
            let beat = t * 124.0 / 60.0;
            live.scene.beat = Some(beat);
            live.scene.seconds = t;
            live.sources[0].beat = Some(beat);
            // First each performer by hand, ten seconds each: the silhouette,
            // the dots, the person. Then the Director's choreography.
            live.settings.automatic = t >= 30.0;
            live.settings.look = match t {
                t if t < 10.0 => [0.0, 1.0, 0.0],
                t if t < 20.0 => [0.0, 0.0, 1.0],
                _ => [1.0, 0.0, 0.0],
            };
            let t = (t - 30.0).max(0.0);
            (live.scene.intent.section, live.scene.intent.motion, live.scene.intent.intensity) =
                match t {
                    t if t < 16.0 => (Section::Verse, 0.4, 0.5),
                    t if t < 24.0 => (Section::Build, (t - 16.0) / 8.0, 0.7),
                    t if t < 40.0 => (Section::Drop, 0.9, 1.0),
                    _ => (Section::Breakdown, 0.2, 0.3),
                };
            live.body = groove.advance(&live.scene, &live.sources[..2], 1.0 / 30.0);
            frame.performance = Some(live);
            r.draw(&frame);
            input.write_all(&pixels(&r.gl)).unwrap();
        }
        drop(input);
        video.wait().unwrap();
        r.performance.as_mut().unwrap().clock = None;
    }
    // A minute of the filmed DJ alone as the Director plays him, for review:
    // a groove, a drop, a breakdown with his headphones off, the drop again.
    if let Some(path) = std::env::var_os("PATCH_VISUAL_GPU_ME_VIDEO") {
        use patch_core::visual_intent::Section;
        use std::io::Write;
        let mut video = std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-s", "1920x1080", "-r", "30", "-i", "-", "-vf", "vflip"])
            .args(["-c:v", "libx264", "-crf", "18", "-preset", "veryfast"])
            .args(["-pix_fmt", "yuv420p", "-movflags", "+faststart"])
            .arg(path)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = video.stdin.take().unwrap();
        let mut groove = patch_visual::groove::Groove::default();
        let mut live = filmed;
        live.settings.automatic = true;
        let bg = frame.look.bg;
        frame.look.bg = [14, 16, 24];
        let started = std::time::Instant::now();
        for n in 0..1800 {
            let due = std::time::Duration::from_secs_f32(n as f32 / 30.0);
            if let Some(wait) = due.checked_sub(started.elapsed()) {
                std::thread::sleep(wait);
            }
            // Video time, one frame per draw: however slowly this machine
            // draws, the recording plays at the music's speed.
            let t = n as f32 / 30.0;
            r.performance.as_mut().unwrap().clock = Some(started + due);
            let beat = t * 124.0 / 60.0;
            live.scene.beat = Some(beat);
            live.scene.seconds = t;
            live.sources[0].beat = Some(beat);
            (live.scene.intent.section, live.scene.intent.motion, live.scene.intent.intensity) =
                match t {
                    t if t < 15.0 => (Section::Verse, 0.4, 0.5),
                    t if t < 30.0 => (Section::Drop, 0.9, 1.0),
                    t if t < 48.0 => (Section::Breakdown, 0.2, 0.3),
                    _ => (Section::Drop, 0.9, 1.0),
                };
            live.body = groove.advance(&live.scene, &live.sources[..2], 1.0 / 30.0);
            frame.performance = Some(live);
            r.draw(&frame);
            input.write_all(&pixels(&r.gl)).unwrap();
        }
        drop(input);
        video.wait().unwrap();
        r.performance.as_mut().unwrap().clock = None;
        frame.look.bg = bg;
    }
    eprintln!(
        "layers with the film, GPU cost: {:?} ms",
        r.performance.as_ref().and_then(|l| l.gpu_ms)
    );
    // Both hands on the decks' near controls, nothing playing.
    let mut idle = solid;
    idle.sources[0].active = false;
    idle.sources[1].active = false;
    frame.performance = Some(idle);
    for _ in 0..75 {
        r.draw(&frame);
        std::thread::sleep(std::time::Duration::from_millis(33));
    }
    save("pose-deck", &pixels(&r.gl));
    frame.look.bg = dark;
    frame.performance = Some(perf);
    r.draw(&frame);
    assert_ne!(both, face);
    assert_ne!(decks, face);
    // The body moves with the shared beat, through the groove as the
    // Visualiser runs it: a second later it stands elsewhere.
    let mut groove = patch_visual::groove::Groove::default();
    let mut moved = perf;
    for f in 0..60 {
        moved.scene.beat = Some(24.2 + f as f32 * 2.1 / 60.0);
        moved.body = groove.advance(&moved.scene, &moved.sources[..2], 1.0 / 60.0);
    }
    frame.performance = Some(moved);
    r.draw(&frame);
    assert_ne!(
        face,
        pixels(&r.gl),
        "the articulated operator must move with the shared beat"
    );
    perf.settings.performer_opacity = 0.0;
    frame.performance = Some(perf);
    r.draw(&frame);
    assert_eq!(legacy, pixels(&r.gl));
    perf.settings.performer_opacity = 0.95;
    perf.settings.decks = true;
    // Same production scene graph/feedback compositor used by hybrid mode.
    frame.generative = Some(GenFrame {
        key: 19,
        frag: Rc::new(
            "in vec2 uv;out vec4 frag;void main(){frag=vec4(0.005,0.012,0.025,1.0);}".into(),
        ),
        uniforms: vec![],
        time: 12.0,
        compositor: Default::default(),
        cells: true,
        points: None,
    });
    perf.scene.handover = Some(0.5);
    frame.performance = Some(perf);
    r.draw(&frame);
    save("hybrid", &pixels(&r.gl));
    // Shader programs and render targets must stay stable across frames.
    let resources = (
        r.scene.as_ref().unwrap().tex,
        r.comp.as_ref().unwrap().tex,
        r.gen_prog.as_ref().unwrap().1,
    );
    let mut times = Vec::new();
    for i in 0..180 {
        perf.scene.beat = Some(24.0 + i as f32 / 30.0);
        perf.scene.handover = Some(i as f32 / 179.0);
        frame.performance = Some(perf);
        let t = std::time::Instant::now();
        r.draw(&frame);
        unsafe { r.gl.finish() };
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    assert_eq!(
        resources,
        (
            r.scene.as_ref().unwrap().tex,
            r.comp.as_ref().unwrap().tex,
            r.gen_prog.as_ref().unwrap().1
        )
    );
    times.sort_by(f64::total_cmp);
    eprintln!(
        "production compositor 1080p both layers + hybrid + bloom, synchronized wall time: median {:0.3}ms p95 {:0.3}ms",
        times[90], times[171]
    );
    replay_ai_capture(&mut r, &mut frame, perf);
    // Resize/fullscreen canvas, missing sources and fully absent metadata.
    frame.rect = [0, 0, 960, 540];
    perf.sources = [Source::default(); MAX_SOURCES];
    perf.count = 0;
    perf.scene = Scene::default();
    frame.performance = Some(perf);
    r.draw(&frame);
    frame.rect = [0, 0, 1920, 1080];
    r.draw(&frame);
    unsafe {
        assert_eq!(r.gl.get_error(), glow::NO_ERROR);
    }
}

/// Replays a read-only `ai_state` capture through the *existing* Director and
/// compositor. Values absent from that contract (spectrum/EQ/stems) stay absent.
fn replay_ai_capture(r: &mut Renderer, frame: &mut Frame, mut perf: Performance) {
    use patch_visual::director::{Director, Mood, Music};
    let Ok(path) = std::env::var("PATCH_VISUAL_AI_REPLAY") else {
        return;
    };
    let states: Vec<patch_core::aivis::VisualState> =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(!states.is_empty());
    let mut director = Director::default();
    let mut end = 0.0;
    let mut start = 0.0;
    let mut previous = states[0].clock.nsec;
    let origin = previous;
    let mut seed = 0.0;
    let mut palette = [[0.0; 3]; 2];
    let (mut active, mut dropped, mut images) = (0, 0, 0);
    for st in &states {
        let dt = st.clock.nsec.saturating_sub(previous) as f64 / 1e9;
        previous = st.clock.nsec;
        let now = st.clock.nsec.saturating_sub(origin) as f64 / 1e9;
        let Some(lead) = st
            .sources
            .iter()
            .filter(|s| s.playing)
            .max_by(|a, b| a.presence.total_cmp(&b.presence))
        else {
            continue;
        };
        let handover = patch_visual::performance::handover_at(st, st.clock.nsec);
        let music = Music {
            title: lead.title.clone(),
            artist: lead.artist.clone(),
            playing: true,
            level: lead.energy,
            beat: lead.beat.unwrap_or(-1.0),
            bpm: lead.bpm.unwrap_or(0.0),
            transition: st.sources.iter().filter(|s| s.playing).count() > 1,
            handover,
            bands: vec![],
            kicking: true,
            calm: 0.0,
        };
        director.listen(&music, dt);
        let due = if now == 0.0 {
            Some(director.mood(&music))
        } else {
            director.due(&music, now >= end, now)
        };
        if let Some(mood) = due {
            dropped += usize::from(mood == Mood::Drop);
            let cue = director.cue(&music, mood, now, patch_visual::pack::PALETTES[0]);
            start = now;
            end = now + cue.fit_s.unwrap_or(8.0);
            seed = (cue.seed >> 40) as f32 / 16_777_215.0;
            palette = [
                cue.palette.a.map(|v| v as f32 / 255.0),
                cue.palette.b.map(|v| v as f32 / 255.0),
            ];
        }
        let progress = music
            .handover
            .as_ref()
            .filter(|h| h.beats_in >= 0.0)
            .map(|h| (h.beats_in / h.beats).clamp(0.0, 1.0) as f32);
        perf.scene = Scene {
            seconds: director.motion().seconds as f32,
            beat: lead.beat.map(|b| b as f32),
            seed,
            progress: ((now - start) / (end - start).max(0.1)).clamp(0.0, 1.0) as f32,
            intent: director.intent(),
            palette,
            handover: progress,
            transition_preparing: music.handover.as_ref().is_some_and(|h| h.beats_in < 0.0),
            bands: [0.0; 3],
            kick: 0.0,
            crossfader: 0.5,
        };
        perf.count = st.sources.len().min(MAX_SOURCES);
        perf.sources = [Source::default(); MAX_SOURCES];
        for (target, source) in perf.sources.iter_mut().zip(&st.sources) {
            *target = Source {
                active: source.playing,
                level: if source.playing { source.energy } else { 0.0 },
                beat: source.beat.map(|b| b as f32),
                weight: source.presence,
                progress: if source.position + source.remaining > 0.0 {
                    (source.position / (source.position + source.remaining)) as f32
                } else {
                    0.0
                },
                ..Default::default()
            };
        }
        perf.appearance = patch_visual::performance::appearance(perf.settings, &perf.scene);
        frame.performance = Some(perf);
        r.draw(frame);
        if let Some(progress) = progress {
            active += 1;
            if images == 0 || (images == 1 && progress > 0.45) {
                save(&format!("real-handover-{images}"), &pixels(&r.gl));
                images += 1;
            }
        }
    }
    assert!(
        active > 0,
        "capture must contain a real executing AI handover"
    );
    assert!(
        states.last().unwrap().transition.is_none(),
        "capture must include completion"
    );
    eprintln!(
        "actual AI state replay: {} stamped samples, {active} handover frames, {dropped} Director drop cues",
        states.len()
    );
}

/// Render the imported worlds through the production atlas, bloom and final
/// compositor. Optional review video is encoded from these actual GL frames.
#[test]
#[ignore = "requires EGL/OpenGL; captures the Ambient collection at 1080p"]
fn ambient_collection_gpu() {
    use patch_visual::{ambient, stage::{Backdrop, Ctx}};
    use std::io::Write;
    let _egl = Egl::new();
    let gl = unsafe { glow::Context::from_loader_function_cstr(|s| eglGetProcAddress(s.as_ptr())) };
    let mut renderer = Renderer::new(gl,"monospace").unwrap();
    let mut slots = Slots::default();
    let mut scenes = ambient::Scenes::default();
    let mut encoder = std::env::var_os("PATCH_VISUAL_AMBIENT_VIDEO").map(|path| {
        std::process::Command::new("ffmpeg")
            .args(["-y","-v","error","-f","rawvideo","-pix_fmt","rgba","-s","1920x1080","-r","30","-i","-","-vf","vflip","-c:v","libx264","-crf","20","-preset","veryfast","-pix_fmt","yuv420p","-movflags","+faststart"])
            .arg(path).stdin(std::process::Stdio::piped()).spawn().unwrap()
    });
    let mut input = encoder.as_mut().and_then(|e| e.stdin.take());
    let look = Look {bg:[3,5,12],text:[220,225,240],pulse:0.0,glow:0.35,bump:0.0,split:0.0,shake:0.0};
    for &kind in Backdrop::AMBIENT {
        let mut first = None;
        let mut costs = Vec::new();
        for n in 0..180 {
            let t = f64::from(n)/30.0;
            let beat = t*124.0/60.0;
            let c = Ctx {
                cols:240,rows:68,t,beat,music_beat:beat,age:t,span:32.0,
                ease:patch_visual::Easing::InOutSine,size:1,place:0,bright:1.0,
                fonts:None,font:0,style:0,gate:1.0,energy:0.7,
                bass:0.3+0.5*(-beat.rem_euclid(1.0) as f32*8.0).exp(),mid:0.4,high:0.3,
                bands:&[],history:&[],pal:&patch_visual::pack::PALETTES[0],title:"Ambient",artist:"Patch",seed:42,
            };
            let grid = scenes.draw(kind,&c,None);
            let frame = Frame::from_grid(&grid,look,1920.0,[0,0,1920,1080],&mut slots);
            let started = std::time::Instant::now();
            renderer.draw(&frame);
            unsafe { renderer.gl.finish(); }
            costs.push(started.elapsed().as_secs_f64()*1000.0);
            let p = pixels(&renderer.gl);
            if n == 0 { first = Some(p.clone()); }
            if let Some(input) = input.as_mut() { input.write_all(&p).unwrap(); }
            if n == 179 {
                let p = &p;
                assert_ne!(first.as_ref().unwrap(),p,"{} animates on the GPU",kind.name());
                assert!(p.chunks_exact(4).filter(|px| px[0]>40 || px[1]>40 || px[2]>40).count() > 1000);
                save(&format!("ambient-{}",kind.name()),p);
                let mut frame = frame;
                frame.performance = Some(Performance {
                    scene: Scene {seconds:t as f32,beat:Some(beat as f32),bands:[c.bass,0.4,0.3],palette:patch_visual::performance::BRAND,..Default::default()},
                    sources:[Source::default();MAX_SOURCES],count:2,
                    settings:Settings {decks:true,performer:true,automatic:false,..Default::default()},
                    appearance:0.0,point_step:1,body:Default::default()
                });
                renderer.draw(&frame);
                let layered = pixels(&renderer.gl);
                assert_ne!(p,&layered,"{} composes with the booth and performer",kind.name());
                save(&format!("ambient-{}-performer",kind.name()),&layered);
            }
        }
        costs.sort_by(f64::total_cmp);
        eprintln!("Ambient {} GPU compositor 1080p: median {:.3} ms, p95 {:.3} ms",kind.name(),costs[90],costs[171]);
    }
    drop(input);
    if let Some(mut encoder) = encoder { assert!(encoder.wait().unwrap().success()); }
    drop(renderer);
}

/// The composed example must move too: drive the real Groove, meters and
/// controls each frame, rather than appending a single layered still.
#[test]
#[ignore = "requires EGL/OpenGL; records the Ambient booth and moving performer"]
fn ambient_performer_gpu() {
    use patch_core::visual_intent::Section;
    use patch_visual::{ambient, groove::Groove, stage::{Backdrop, Ctx}};
    use std::io::Write;
    let _egl = Egl::new();
    let gl = unsafe { glow::Context::from_loader_function_cstr(|s| eglGetProcAddress(s.as_ptr())) };
    let mut renderer = Renderer::new(gl,"monospace").unwrap();
    let mut slots = Slots::default();
    let mut worlds = ambient::Scenes::default();
    let mut groove = Groove::default();
    let started = std::time::Instant::now();
    let mut encoder = std::env::var_os("PATCH_VISUAL_AMBIENT_PERFORMER_VIDEO").map(|path| {
        std::process::Command::new("ffmpeg")
            .args(["-y","-v","error","-f","rawvideo","-pix_fmt","rgba","-s","1920x1080","-r","30","-i","-","-vf","vflip","-c:v","libx264","-crf","20","-preset","veryfast","-pix_fmt","yuv420p","-movflags","+faststart"])
            .arg(path).stdin(std::process::Stdio::piped()).spawn().unwrap()
    });
    let mut input = encoder.as_mut().and_then(|e| e.stdin.take());
    let look = Look {bg:[3,5,12],text:[220,225,240],pulse:0.0,glow:0.35,bump:0.0,split:0.0,shake:0.0};
    let mut first_pose = None;
    let mut first_image = None;
    for n in 0..360 {
        let t = f64::from(n)/30.0;
        let beat = t*124.0/60.0;
        let bass = 0.3+0.5*(-beat.rem_euclid(1.0) as f32*8.0).exp();
        let c = Ctx {cols:240,rows:68,t,beat,music_beat:beat,age:t,span:32.0,
            ease:patch_visual::Easing::InOutSine,size:1,place:0,bright:1.0,fonts:None,font:0,style:0,
            gate:1.0,energy:0.7,bass,mid:0.4,high:0.3,bands:&[],history:&[],
            pal:&patch_visual::pack::PALETTES[0],title:"Ambient",artist:"Patch",seed:42};
        let grid = worlds.draw(Backdrop::EventHorizon,&c,None);
        let mut frame = Frame::from_grid(&grid,look,1920.0,[0,0,1920,1080],&mut slots);
        let mut scene = Scene {seconds:t as f32,beat:Some(beat as f32),bands:[bass,0.4,0.3],
            kick:bass,palette:patch_visual::performance::BRAND,progress:if t < 6.0 {t as f32/24.0} else {(t as f32-6.0)/24.0},
            crossfader:0.5+0.35*(t as f32*0.5).sin(),..Default::default()};
        scene.intent.section = if t < 6.0 {Section::Verse} else {Section::Drop};
        scene.intent.intensity = 0.8;
        scene.intent.motion = 0.7;
        let mut sources = [Source::default();MAX_SOURCES];
        for (i,source) in sources[..2].iter_mut().enumerate() {
            let level = (0.25+0.5*((t as f32*2.0+i as f32).sin()*0.5+0.5)).min(1.0);
            *source = Source {active:true,level,meter:[level,level*0.85],beat:Some(beat as f32),
                gain:0.8,weight:if i == 0 {1.0-scene.crossfader} else {scene.crossfader},..Default::default()};
        }
        let body = groove.advance(&scene,&sources[..2],1.0/30.0);
        frame.performance = Some(Performance {scene,sources,count:2,
            settings:Settings {decks:true,performer:true,automatic:false,me:false,look:[0.0,1.0,0.0],..Default::default()},
            appearance:0.0,point_step:1,body});
        renderer.performance.as_mut().unwrap().clock = Some(started+std::time::Duration::from_secs_f64(t));
        renderer.draw(&frame);
        let p = pixels(&renderer.gl);
        if n == 90 { first_pose = Some(body); first_image = Some(p.clone()); }
        if n == 270 {
            let before = first_pose.unwrap();
            assert_ne!((before.nod,before.sway,before.gesture,before.gesture2),(body.nod,body.sway,body.gesture,body.gesture2),"the performer must change pose, not just its clock");
            assert_ne!(first_image.as_ref().unwrap(),&p);
            save("ambient-moving-performer",&p);
        }
        if let Some(input) = input.as_mut() { input.write_all(&p).unwrap(); }
    }
    drop(input);
    if let Some(mut encoder) = encoder { assert!(encoder.wait().unwrap().success()); }
    drop(renderer);
}

/// Patch's own ISF generators through the production renderer: each one
/// animates with the music, is not blank, and stays cheap at 1080p.
#[test]
#[ignore = "requires EGL/OpenGL; renders every built-in ISF generator at 1080p"]
fn isf_generators_gpu() {
    use patch_visual::generative::{Bindings, Signals};
    let _egl = Egl::new();
    let gl = unsafe { glow::Context::from_loader_function_cstr(|s| eglGetProcAddress(s.as_ptr())) };
    let mut renderer = Renderer::new(gl, "monospace").unwrap();
    let mut slots = Slots::default();
    let look = Look { bg: [0, 0, 0], text: [220, 225, 240], pulse: 0.0, glow: 0.0, bump: 0.0, split: 0.0, shake: 0.0 };
    let grid = patch_visual::grid::Grid::blank(240, 68);
    for (key, (name, src)) in patch_visual::isf::builtin().into_iter().enumerate() {
        let shader = patch_visual::isf::compile(&patch_visual::isf::parse(src).unwrap(), &Default::default()).unwrap();
        let mut bindings = Bindings::new(&shader.uniforms);
        let mut costs = Vec::new();
        let mut shots = Vec::new();
        for n in 0..120 {
            let t = 40.0 + f64::from(n) / 30.0 * 4.0;
            let beat = t * 124.0 / 60.0;
            let mut s = Signals::default();
            s.set_clock(t, beat, 124.0);
            let kick = (-(beat.rem_euclid(1.0) as f32) * 6.0).exp();
            s.set("energy", 0.7);
            s.set("low", 0.25 + 0.5 * kick);
            s.set("mid", 0.4);
            s.set("high", 0.3);
            s.set("kick", kick);
            let uniforms = bindings.values(&s, 1.0 / 30.0).into_iter().map(|(u, v)| (u.to_string(), v)).collect();
            let mut frame = Frame::from_grid(&grid, look, 1920.0, [0, 0, 1920, 1080], &mut slots);
            frame.generative = Some(GenFrame {
                key: 1000 + key as u64,
                frag: Rc::new(shader.frag.clone()),
                uniforms,
                time: t as f32,
                compositor: Default::default(),
                cells: false,
                points: None,
            });
            let started = std::time::Instant::now();
            renderer.draw(&frame);
            unsafe { renderer.gl.finish() };
            costs.push(started.elapsed().as_secs_f64() * 1000.0);
            if n % 40 == 20 {
                shots.push(pixels(&renderer.gl));
            }
        }
        assert_ne!(shots[0], shots[2], "{name} animates");
        let lit = shots[1].chunks_exact(4).filter(|px| px[0] > 24 || px[1] > 24 || px[2] > 24).count();
        assert!(lit > 20_000, "{name} is not blank ({lit} lit pixels)");
        for (i, p) in shots.iter().enumerate() {
            save(&format!("isf-{name}-{i}"), p);
        }
        costs.sort_by(f64::total_cmp);
        eprintln!("ISF {name} 1080p: median {:.3} ms, p95 {:.3} ms", costs[60], costs[114]);
    }
    drop(renderer);
}

/// A demo reel: Ambient worlds carrying Patch's title effects, joined by the
/// Visualiser's own transitions, through the production renderer.
/// `PATCH_VISUAL_DEMO_VIDEO=<path.mp4>` writes it.
#[test]
#[ignore = "requires EGL/OpenGL and ffmpeg; renders the Ambient demo reel"]
fn ambient_demo_gpu() {
    use patch_visual::{ambient, stage::{self, Act, Backdrop, Ctx}};
    use std::io::Write;
    let Some(path) = std::env::var_os("PATCH_VISUAL_DEMO_VIDEO") else { return };
    let _egl = Egl::new();
    let gl = unsafe { glow::Context::from_loader_function_cstr(|s| eglGetProcAddress(s.as_ptr())) };
    let mut renderer = Renderer::new(gl, "monospace").unwrap();
    let mut slots = Slots::default();
    let mut scenes = ambient::Scenes::default();
    let fonts = patch_visual::fonts::Fonts::load();
    let pal = patch_visual::pack::PALETTES[0];
    // (world, title effect, title, artist, font)
    // The track's own title and artist, as Patch shows a playing track
    // (`PATCH_VISUAL_DEMO_TITLE=title|artist`).
    let names = std::env::var("PATCH_VISUAL_DEMO_TITLE").unwrap_or_else(|_| "Midnight Transit|Kasper Lind".into());
    let (song, artist) = names.split_once('|').unwrap_or((names.as_str(), ""));
    let names_b = std::env::var("PATCH_VISUAL_DEMO_TITLE_B").unwrap_or_default();
    let (song_b, artist_b) = names_b.split_once('|').unwrap_or((names_b.as_str(), ""));
    // A two-deck mix: `PATCH_VISUAL_DEMO_MIX=b_in:swap:a_out` (seconds).
    let mix: Vec<f64> = std::env::var("PATCH_VISUAL_DEMO_MIX").ok()
        .map(|g| g.split(':').filter_map(|v| v.parse().ok()).collect()).unwrap_or_default();
    let worlds: [(Backdrop, Act, u8); 12] = [
        (Backdrop::SunsetDrive, Act::Words, 1),
        (Backdrop::Jellyfish, Act::Neon, 2),
        (Backdrop::Ambient("neon-city"), Act::Zoom, 3),
        (Backdrop::EventHorizon, Act::Wave, 4),
        // No sand clocks: they show the wall-clock time.
        (Backdrop::Ambient("abyss"), Act::Shatter, 5),
        (Backdrop::Ambient("vector-highway"), Act::Bounce, 6),
        (Backdrop::Ambient("data-globe"), Act::Glitch, 2),
        (Backdrop::Ambient("tunnel-reactive"), Act::Zoom, 3),
        (Backdrop::Ambient("supernova"), Act::Mirror, 7),
        (Backdrop::Ambient("circuit-run"), Act::Words, 4),
        (Backdrop::Ambient("neon-avenue"), Act::Neon, 5),
        (Backdrop::Ambient("aquarium"), Act::Type, 1),
    ];
    let modern = std::env::var_os("PATCH_VISUAL_DEMO_MODERN").is_some();
    let bars: usize = std::env::var("PATCH_VISUAL_DEMO_BARS").ok().and_then(|b| b.parse().ok()).unwrap_or(32);
    // Bars a world holds (`PATCH_VISUAL_DEMO_WORLD_BARS`, 4 by default) and
    // the first world of the list (`PATCH_VISUAL_DEMO_WORLDS_FROM`).
    let world_bars: usize = std::env::var("PATCH_VISUAL_DEMO_WORLD_BARS").ok().and_then(|b| b.parse().ok()).unwrap_or(4).clamp(1, 64);
    let from: usize = std::env::var("PATCH_VISUAL_DEMO_WORLDS_FROM").ok().and_then(|b| b.parse().ok()).unwrap_or(0);
    let segments = bars.div_ceil(world_bars).clamp(1, 24);
    let seg0 = world_bars as f64 / 4.0 * if std::env::var_os("PATCH_VISUAL_DEMO_GRID").is_some() { 16.0 * 60.0 / std::env::var("PATCH_VISUAL_DEMO_GRID").ok().and_then(|g| g.split(':').next().and_then(|v| v.parse::<f64>().ok())).unwrap_or(124.0) } else { 7.5 };
    let reel: Vec<(Backdrop, Act, &str, &str, u8)> = (0..segments).map(|i| {
        // Short reels keep the original order; longer ones use every world.
        let (kind, act, font) = if segments <= 8 && from == 0 { [0, 1, 2, 3, 4, 5, 8, 11].map(|k| worlds[k])[i] } else { worlds[(i + from) % worlds.len()] };
        let at = i as f64 * seg0;
        // In a mix: A's title, then B's (alternating while both play).
        let b = mix.len() == 3 && (at >= mix[1] || (at >= mix[0] && i % 2 == 0));
        // The contemporary titles in clean type (sans, narrow, geometric).
        let (act, font) = if modern {
            let face = |name: &str| (0..fonts.len()).find(|&f| fonts.name(f) == name).map_or(1, |f| f as u8 + 1);
            ([Act::Decode, Act::Rise, Act::Sheen, Act::Slice][i % 4], face(["narrow", "sans", "gothic"][i % 3]))
        } else { (act, font) };
        if b { (kind, act, song_b, artist_b, font) } else { (kind, act, song, artist, font) }
    }).collect();

    // The track's grid: `PATCH_VISUAL_DEMO_GRID=bpm:start:offset` (start of
    // the excerpt and the grid's first beat, seconds in the track) and the
    // beatless stretches `PATCH_VISUAL_DEMO_QUIET=a-b,…`; else 124 BPM.
    let grid: Vec<f64> = std::env::var("PATCH_VISUAL_DEMO_GRID").ok()
        .map(|g| g.split(':').filter_map(|v| v.parse().ok()).collect()).unwrap_or_default();
    let (bpm, start, offset) = if grid.len() == 3 { (grid[0], grid[1], grid[2]) } else { (124.0, 0.0, 0.0) };
    let quiet: Vec<(f64, f64)> = std::env::var("PATCH_VISUAL_DEMO_QUIET").ok().map(|q| q.split(',')
        .filter_map(|r| r.split_once('-').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))).collect()).unwrap_or_default();
    let music = |t: f64| {
        let at = t + start;
        let beat = (at - offset) * bpm / 60.0;
        let silent = quiet.iter().any(|&(a, b)| at >= a && at < b);
        (beat, silent)
    };
    // Four bars a world: changes land on the phrase.
    let seg = world_bars as f64 / 4.0 * if grid.len() == 3 { 16.0 * 60.0 / bpm } else { 7.5f64 };
    let fade = 0.8f64;
    let booth = std::env::var_os("PATCH_VISUAL_DEMO_BOOTH").is_some();
    let titles = std::env::var_os("PATCH_VISUAL_DEMO_NO_TITLES").is_none();
    let compose = |scenes: &mut ambient::Scenes, i: usize, t: f64| {
        let (kind, act, title, artist, font) = reel[i];
        let local = t - i as f64 * seg;
        let (beat, silent) = music(t);
        // The kick as Patch hears it: a sharp hit on the beat, nothing where
        // no kick plays (the worlds' own beat moves follow it).
        let kick = if silent { 0.0 } else { (-(beat.rem_euclid(1.0) as f32) * 9.0).exp() };
        let c = Ctx {
            cols: 240, rows: 68, t, beat, music_beat: beat, age: ((local - 2.5) * bpm / 60.0).max(0.0), span: 16.0,
            ease: patch_visual::Easing::InOutSine, size: if modern { 0 } else { 1 }, place: if modern { 0 } else { 1 }, bright: 1.45,
            fonts: Some(&fonts), font: font.min(fonts.len() as u8), style: 0, gate: if silent { 0.0 } else { 1.0 }, energy: if silent { 0.4 } else { 0.8 },
            bass: kick, mid: 0.45, high: 0.3 + 0.5 * kick,
            bands: &[], history: &[], pal: &pal, title, artist, seed: 42 + i as u64,
        };
        let mut g = scenes.draw(kind, &c, None);
        // A visual passage first, then the title effect over the world.
        if titles && local > 2.5 && local < seg - 0.6 {
            let words = stage::act(act, &c);
            stage::over_readable(&mut g, &words);
        }
        g
    };
    let mut encoder = std::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", "1920x1080", "-r", "30", "-i", "-",
            "-vf", &format!("vflip,fade=t=in:st=0:d=1,fade=t=out:st={:.2}:d=2", seg0 * segments as f64 - 2.0), "-c:v", "libx264", "-crf", "17", "-preset", "slow",
            "-pix_fmt", "yuv420p", "-movflags", "+faststart"])
        .arg(&path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = encoder.stdin.take().unwrap();
    let frames = (seg * reel.len() as f64 * 30.0) as usize;
    for n in 0..frames {
        let t = n as f64 / 30.0;
        let i = ((t / seg) as usize).min(reel.len() - 1);
        let local = t - i as f64 * seg;
        let mut g = compose(&mut scenes, i, t);
        if i > 0 && local < fade {
            let before = compose(&mut scenes, i - 1, t);
            stage::transition_style(&mut g, &before, (local / fade) as f32, i as u64 % 4, 42 + i as u64);
        }
        let (beat, silent) = music(t);
        // An occasional accent, not a pulse on every beat: once per 16 beats,
        // only while the kick plays (the drop lands on one).
        let accent = if !silent && beat.rem_euclid(16.0) < 1.0 { 0.4 * (-(beat.rem_euclid(16.0) as f32) * 4.0).exp() } else { 0.0 };
        // Every kick visibly lands: the glow breathes and the frame gives a
        // small bump, both gone within the beat (the full-screen pulse stays
        // an occasional accent).
        let hit = if silent { 0.0 } else { (-(beat.rem_euclid(1.0) as f32) * 7.0).exp() };
        let look = Look { bg: [3, 4, 10], text: [235, 232, 238], pulse: accent, glow: 0.3 + 0.5 * hit, bump: 0.45 * hit, split: 0.0, shake: 0.0 };
        let mut frame = Frame::from_grid(&g, look, 1920.0, [0, 0, 1920, 1080], &mut slots);
        // Version with the booth: Patch's Decks & Signal layer, driven by the
        // same grid (deck A plays the track, B waits, the crossfader drifts).
        if booth {
            let kick = if silent { 0.0 } else { (-(beat.rem_euclid(1.0) as f32) * 6.0).exp() };
            let level = if silent { 0.45 } else { 0.62 + 0.3 * kick };
            let mut sources = [Source::default(); MAX_SOURCES];
            let crossfader;
            if mix.len() == 3 {
                // A alone, then both (B's fader rising), then B alone.
                let ramp = |a: f64, b: f64| ((t - a) / (b - a)).clamp(0.0, 1.0) as f32;
                let b_in = ramp(mix[0], mix[0] + 32.0 * 60.0 / bpm);
                let a_out = 1.0 - ramp(mix[1], mix[2]);
                crossfader = 0.5 * ramp(mix[0], mix[1]) + 0.5 * ramp(mix[1], mix[2]);
                let lb = level * b_in;
                let la = level * a_out;
                sources[0] = Source { active: a_out > 0.0, level: la, meter: [la, la * 0.97], beat: Some(beat as f32),
                    progress: ((t * 1.0116 + 111.6) / 353.0) as f32, weight: a_out * (1.0 - crossfader * 0.5), gain: 0.85 * a_out.max(0.2), ..Default::default() };
                sources[1] = Source { active: b_in > 0.0, level: lb, meter: [lb * 0.98, lb], beat: Some(beat as f32),
                    progress: (((t - mix[0]).max(0.0) * 0.9886 + 145.5) / 385.0) as f32, weight: b_in * (0.5 + crossfader * 0.5), gain: 0.85 * b_in.max(0.2), ..Default::default() };
            } else {
                crossfader = 0.5 - 0.42 * (1.0 - (t as f32 / 60.0)).clamp(0.0, 1.0);
                sources[0] = Source { active: true, level, meter: [level, level * 0.97], beat: Some(beat as f32),
                    progress: ((t + start) / 353.0) as f32, weight: 1.0 - crossfader * 0.6, gain: 0.85, ..Default::default() };
                sources[1] = Source { active: false, level: 0.0, meter: [0.0; 2], beat: Some(beat as f32), progress: 0.08,
                    weight: crossfader, gain: 0.7, ..Default::default() };
            }
            frame.performance = Some(Performance {
                scene: Scene { seconds: t as f32, beat: Some(beat as f32), bands: [0.3 + 0.5 * kick, 0.45, 0.35],
                    palette: patch_visual::performance::BRAND, kick, crossfader, ..Default::default() },
                sources, count: 2,
                settings: Settings { decks: true, performer: false, automatic: false,
                    ribbons: std::env::var_os("PATCH_VISUAL_DEMO_NO_RIBBONS").is_none(), ..Default::default() },
                appearance: 0.0, point_step: 1, body: Default::default(),
            });
        }
        renderer.draw(&frame);
        let p = pixels(&renderer.gl);
        if n % 225 == 150 { save(&format!("demo-{n}"), &p); }
        input.write_all(&p).unwrap();
    }
    drop(input);
    assert!(encoder.wait().unwrap().success());
    drop(renderer);
}
