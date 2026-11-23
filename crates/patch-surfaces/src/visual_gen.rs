//! The Visualiser's generative layer: mode, scene, compositor, the signal
//! bus, and the shader compiler behind `patch_visual::generative`.
//!
//! The compiler (Hydra, out of process) runs on a worker thread: the UI
//! asks for a scene and keeps drawing what it has until the shader comes
//! back; a scene whose structure is unchanged is never recompiled (only its
//! uniforms move). Signals come from the music each frame, from the AI director's
//! posted intent (or the director's own, `Director::intent`), and from a
//! running pattern's visual events, placed on the graph clock
//! (`patch_visual::feed`). Mode, scene and compositor persist in
//! `~/.local/state/omarchy-patch/visualiser.json`; frame and sync figures
//! go to `$XDG_RUNTIME_DIR/omarchy-patch/visualiser-stats.json`.

use patch_core::visual_intent::Intent;
use patch_visual::director::Music;
use patch_visual::feed::Feed;
use patch_visual::generative::{Bindings, CompiledShader, Compositor, Mode, Signals, VisualBackend, VisualScene, presets};
use patch_visual::isf;
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};

/// What survives a restart.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Saved {
    pub mode: Mode,
    /// A preset's name, or `isf:<name>` (`patch_visual::isf`).
    pub scene: String,
    pub compositor: Compositor,
}

fn saved_path() -> std::path::PathBuf {
    patch_engine::session::state_dir().join("visualiser.json")
}

/// What a scene is made from: a Hydra scene, or an ISF file by name.
#[derive(Clone)]
enum Source {
    Hydra(VisualScene),
    Isf(String),
}

type Job = (u64, Source);
type Done = (u64, Result<CompiledShader, String>, f64);

struct Worker {
    tx: Sender<Job>,
    rx: Receiver<Done>,
}

fn worker() -> Option<Worker> {
    let (tx, jobs) = channel::<Job>();
    let (done, rx) = channel::<Done>();
    std::thread::Builder::new()
        .name("patch-visual-compile".into())
        .spawn(move || {
            let mut hydra: Option<Result<patch_visual::hydra::Hydra, String>> = None;
            while let Ok((key, source)) = jobs.recv() {
                let t0 = std::time::Instant::now();
                let scene = match source {
                    Source::Isf(name) => {
                        let r = isf::load(&name).and_then(|(src, mapping)| {
                            isf::compile(&isf::parse(&src).map_err(|e| format!("{name}: {e}"))?, &mapping)
                        });
                        if done.send((key, r, t0.elapsed().as_secs_f64() * 1000.0)).is_err() {
                            break;
                        }
                        continue;
                    }
                    Source::Hydra(scene) => scene,
                };
                let h = hydra.get_or_insert_with(|| match patch_visual::hydra::locate() {
                    Some(dir) => patch_visual::hydra::Hydra::start(&dir),
                    None => Err("the Hydra adapter isn't installed (cd adapters/hydra && npm install)".into()),
                });
                let r = match h {
                    Ok(h) => h.compile(&scene),
                    Err(e) => Err(e.clone()),
                };
                // A dead process is started again on the next scene.
                if r.as_ref().is_err_and(|e| e.contains("stopped") || e.contains("didn't answer")) {
                    hydra = None;
                }
                if done.send((key, r, t0.elapsed().as_secs_f64() * 1000.0)).is_err() {
                    break;
                }
            }
        })
        .ok()?;
    Some(Worker { tx, rx })
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub fps: f64,
    /// The generative layer's CPU work per frame (signals, polls,
    /// uniforms); the GPU's time isn't measured here.
    pub cpu_ms_mean: f64,
    pub cpu_ms_max: f64,
    /// Frames later than 1.5× the display's 60 Hz interval.
    pub dropped: u64,
    pub frames: u64,
    pub compile_ms_last: f64,
    pub compile_ms_max: f64,
    pub compiles: u64,
    /// From the AI director posting an intent to the first frame that used it.
    pub control_to_frame_ms_last: Option<f64>,
    pub pattern_sync_mean_ms: f64,
    pub pattern_sync_max_ms: f64,
    pub pattern_events: u64,
    pub pattern_late_arrivals: u64,
}

fn key_of(source: &Source) -> u64 {
    let text = match source {
        Source::Hydra(scene) => scene.structure_key(),
        Source::Isf(name) => format!("isf:{name}"),
    };
    text.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

/// Every scene by name: the Hydra presets, then the ISF scenes.
pub(crate) fn scene_names() -> Vec<String> {
    presets()
        .into_iter()
        .map(|(n, _)| n.to_string())
        .chain(isf::scenes().into_iter().map(|n| format!("isf:{n}")))
        .collect()
}

#[derive(Default)]
pub struct Gen {
    saved: RefCell<Saved>,
    loaded: Cell<bool>,
    worker: RefCell<Option<Worker>>,
    /// The shader in use: its key, source and bindings.
    compiled: RefCell<Option<(u64, Rc<String>, Bindings)>>,
    pending: Cell<Option<u64>>,
    error: RefCell<Option<String>>,
    signals: RefCell<Signals>,
    feed: RefCell<Feed>,
    polls: crate::poll::Poller,
    polled: Cell<f64>,
    /// The AI director's intent, when one applies: (intent, version, posted ms).
    posted: RefCell<Option<(Intent, u64, u64)>>,
    seen_version: Cell<u64>,
    kick: Cell<f32>,
    stats: RefCell<Stats>,
    stats_written: Cell<f64>,
}

fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Gen {
    fn load(&self) {
        if self.loaded.replace(true) {
            return;
        }
        if let Some(s) = std::fs::read(saved_path()).ok().and_then(|b| serde_json::from_slice::<Saved>(&b).ok()) {
            *self.saved.borrow_mut() = s;
        }
    }

    fn save(&self) {
        let path = saved_path();
        let tmp = path.with_extension("tmp");
        if let Ok(b) = serde_json::to_vec_pretty(&*self.saved.borrow())
            && std::fs::write(&tmp, b).is_ok()
        {
            let _ = std::fs::rename(tmp, path);
        }
    }

    pub fn mode(&self) -> Mode {
        self.load();
        self.saved.borrow().mode
    }

    pub fn apply(&self,mut saved:Saved) {
        self.load();
        if !scene_names().contains(&saved.scene) {saved.scene=scene_names().first().cloned().unwrap_or_default();}
        saved.compositor.generative_opacity=saved.compositor.generative_opacity.clamp(0.0,1.0);
        saved.compositor.cells_opacity=saved.compositor.cells_opacity.clamp(0.0,1.0);
        *self.saved.borrow_mut()=saved;*self.error.borrow_mut()=None;self.save();
    }
    pub fn select(&self,name:&str,mode:Mode) {
        if !scene_names().iter().any(|n|n==name){return;}
        self.load();let mut saved=self.saved.borrow().clone();saved.scene=name.into();saved.mode=mode;self.apply(saved);
    }
    pub fn set_mode(&self,mode:Mode) {self.load();self.saved.borrow_mut().mode=mode;self.save();}

    pub fn cycle_mode(&self) {
        self.load();
        let m = self.saved.borrow().mode.next();
        self.saved.borrow_mut().mode = m;
        self.save();
    }

    pub fn next_scene(&self) {
        self.load();
        let all = scene_names();
        let at = all.iter().position(|n| *n == self.saved.borrow().scene).map_or(0, |i| (i + 1) % all.len());
        self.saved.borrow_mut().scene = all[at].clone();
        // A scene that failed to compile doesn't stop the next one.
        *self.error.borrow_mut() = None;
        self.save();
    }

    fn scene(&self) -> (String, Source) {
        let name = self.saved.borrow().scene.clone();
        if let Some(file) = name.strip_prefix("isf:") {
            return (name.clone(), Source::Isf(file.to_string()));
        }
        presets()
            .into_iter()
            .find(|(n, _)| *n == name)
            .or_else(|| presets().into_iter().next())
            .map(|(n, s)| (n.to_string(), Source::Hydra(s)))
            .expect("presets exist")
    }

    /// " · hybrid tunnel" (and what's wrong, if anything), for the caption.
    pub fn caption(&self, gl_error: Option<&str>) -> String {
        let mode = self.mode();
        if mode == Mode::Choreography {
            return String::new();
        }
        let (name, _) = self.scene();
        let trouble = gl_error
            .map(|e| format!(" · shader failed: {}", e.lines().next().unwrap_or("")))
            .or_else(|| self.error.borrow().as_ref().map(|e| format!(" · {e}")))
            .unwrap_or_default();
        format!(" · {} {name}{trouble}", mode.name())
    }

    /// The engine's pattern timeline and the AI director's intent, a few times a
    /// second.
    fn poll(&self, t: f64) {
        for (key, reply) in [("timeline", self.polls.take("timeline")), ("intent", self.polls.take("intent"))] {
            let Some(Ok(v)) = reply.map(|r| r.result) else { continue };
            if key == "timeline" {
                self.feed.borrow_mut().ingest(&v, t);
            } else if v["active"] == true {
                let p = &v["posted"];
                if let Ok(i) = serde_json::from_value::<Intent>(p["intent"].clone()) {
                    *self.posted.borrow_mut() =
                        Some((i, p["version"].as_u64().unwrap_or(0), p["updated_ms"].as_u64().unwrap_or(0)));
                }
            } else {
                *self.posted.borrow_mut() = None;
            }
        }
        if t - self.polled.get() >= 0.2 || t < self.polled.get() {
            self.polled.set(t);
            let mut req = serde_json::json!({"op": "pattern", "action": "timeline", "visual": true});
            if let Some(since) = self.feed.borrow().since {
                req["since"] = since.into();
            }
            self.polls.request("timeline", req);
            self.polls.request("intent", serde_json::json!({"op": "visual_intent", "action": "get"}));
        }
    }

    /// Poll in every mode, including CPU choreography.
    pub fn posted_intent(&self, t: f64) -> Option<Intent> {
        self.poll(t);
        self.posted.borrow().as_ref().map(|(intent, _, _)| *intent)
    }

    /// The generative layer for this frame, or `None` (choreography, or no
    /// shader yet). `director` is what the director plays, as intent.
    pub fn frame(&self, t: f64, dt: f64, m: &Music, director: Intent) -> Option<crate::visual_gl::GenFrame> {
        let mode = self.mode();
        if mode == Mode::Choreography {
            return None;
        }
        let started = std::time::Instant::now();
        self.poll(t);
        // A pattern's scene change.
        if let Some(name) = self.feed.borrow_mut().scene.take()
            && scene_names().contains(&name)
        {
            *self.error.borrow_mut() = None;
            self.saved.borrow_mut().scene = name;
        }
        let (_, scene) = self.scene();
        let key = key_of(&scene);
        // Compile in the background; keep drawing the last shader meanwhile.
        if self.worker.borrow().is_none() {
            *self.worker.borrow_mut() = worker();
        }
        if let Some(w) = self.worker.borrow().as_ref() {
            while let Ok((k, r, ms)) = w.rx.try_recv() {
                if self.pending.get() == Some(k) {
                    self.pending.set(None);
                }
                let mut st = self.stats.borrow_mut();
                st.compiles += 1;
                st.compile_ms_last = ms;
                st.compile_ms_max = st.compile_ms_max.max(ms);
                match r {
                    Ok(c) if k == key => {
                        *self.error.borrow_mut() = None;
                        *self.compiled.borrow_mut() = Some((k, Rc::new(c.frag), Bindings::new(&c.uniforms)));
                    }
                    Ok(_) => {}
                    Err(e) => *self.error.borrow_mut() = Some(e),
                }
            }
            let have = self.compiled.borrow().as_ref().map(|c| c.0);
            if have != Some(key) && self.pending.get() != Some(key) && self.error.borrow().is_none() {
                self.pending.set(Some(key));
                let _ = w.tx.send((key, scene));
            }
        }
        // The bus.
        let mut s = self.signals.borrow_mut();
        s.set_clock(t, m.beat.max(0.0), m.bpm);
        let level = if m.playing { m.level } else { 0.0 };
        s.set("energy", level);
        let bands = &m.bands;
        // The mean of the spectrum's share a…b (0…1), at least one band.
        let part = |a: f32, b: f32| {
            let n = bands.len();
            if n == 0 {
                return level;
            }
            let i = ((a * n as f32) as usize).min(n - 1);
            let j = ((b * n as f32) as usize).clamp(i + 1, n);
            bands[i..j].iter().sum::<f32>() / (j - i) as f32
        };
        s.set("low", part(0.0, 0.2));
        s.set("mid", part(0.3, 0.65));
        s.set("high", part(0.7, 1.0));
        // The kick as an envelope: 1 on the hit, falling over ~150 ms.
        let kick = if m.kicking { 1.0 } else { self.kick.get() * (-dt as f32 / 0.15).exp() };
        self.kick.set(kick);
        s.set("kick", kick);
        s.set("transition", if m.transition || m.handover.is_some() { 1.0 } else { 0.0 });
        // Intent: the AI director's while it applies (hybrid), else the
        // Visualiser's own director's.
        let posted = *self.posted.borrow();
        let intent = match posted {
            Some((i, version, at_ms)) if mode == Mode::Hybrid => {
                if version != self.seen_version.replace(version) {
                    self.stats.borrow_mut().control_to_frame_ms_last = Some(wall_ms().saturating_sub(at_ms) as f64);
                }
                i
            }
            _ => director,
        };
        s.set_intent(&intent);
        self.feed.borrow_mut().apply(t, &mut s);
        s.pattern_over_intent();
        let frame = self.compiled.borrow_mut().as_mut().map(|(k, frag, b)| crate::visual_gl::GenFrame {
            key: *k,
            frag: frag.clone(),
            uniforms: b.values(&s, dt as f32).into_iter().map(|(n, v)| (n.to_string(), v)).collect(),
            time: t as f32,
            compositor: self.saved.borrow().compositor,
            cells: mode == Mode::Hybrid,
            points: None,
        });
        drop(s);
        self.measure(t, dt, started.elapsed().as_secs_f64() * 1000.0);
        frame
    }

    fn measure(&self, t: f64, dt: f64, ms: f64) {
        let mut st = self.stats.borrow_mut();
        st.frames += 1;
        if dt > 1.5 / 60.0 {
            st.dropped += 1;
        }
        if dt > 0.0 {
            st.fps += (1.0 / dt - st.fps) * 0.05;
        }
        st.cpu_ms_mean += (ms - st.cpu_ms_mean) / st.frames as f64;
        st.cpu_ms_max = st.cpu_ms_max.max(ms);
        let sync = self.feed.borrow().sync.clone();
        st.pattern_sync_mean_ms = sync.mean_ms;
        st.pattern_sync_max_ms = sync.max_ms;
        st.pattern_events = sync.applied;
        st.pattern_late_arrivals = sync.late_arrivals;
        if t - self.stats_written.get() >= 5.0 || t < self.stats_written.get() {
            self.stats_written.set(t);
            if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
                let p = std::path::Path::new(&dir).join("omarchy-patch/visualiser-stats.json");
                if let Ok(b) = serde_json::to_vec_pretty(&*st) {
                    let _ = std::fs::write(p, b);
                }
            }
        }
    }
}

pub(crate) fn read_saved()->Saved {std::fs::read(saved_path()).ok().and_then(|s|serde_json::from_slice(&s).ok()).unwrap_or_default()}
