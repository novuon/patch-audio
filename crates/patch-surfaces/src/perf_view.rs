//! The Performance window: a worker samples once a second while one is
//! open (perf.rs); the UI thread only turns the newest snapshot into rows.

use crate::perf::{self, Side, Snapshot, Summary};
use crate::ui::{PerfNode, PerfProc, PerfState, PerfTile};
use slint::{Model, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Two minutes of history at one sample a second.
const HISTORY: usize = 120;
/// A baseline averages this many seconds.
const BASELINE_SECS: usize = 10;

pub struct Live {
    wanted: Arc<AtomicBool>,
    latest: Arc<Mutex<Option<Snapshot>>>,
    started: Cell<bool>,
    history: RefCell<VecDeque<Snapshot>>,
    recording: RefCell<Option<Vec<Snapshot>>>,
    baseline: RefCell<Option<Summary>>,
    /// Xrun counts when the window opened (new ones are shown).
    xruns_at_open: Cell<Option<(u64, u64)>>,
    others: RefCell<Vec<String>>,
    pub tiles: Rc<VecModel<PerfTile>>,
    pub procs: Rc<VecModel<PerfProc>>,
    pub nodes: Rc<VecModel<PerfNode>>,
}

impl Default for Live {
    fn default() -> Self {
        Live {
            wanted: Arc::new(AtomicBool::new(false)),
            latest: Arc::new(Mutex::new(None)),
            started: Cell::new(false),
            history: RefCell::new(VecDeque::with_capacity(HISTORY)),
            recording: RefCell::new(None),
            baseline: RefCell::new(perf::load_baseline()),
            xruns_at_open: Cell::new(None),
            others: RefCell::new(Vec::new()),
            tiles: Rc::new(VecModel::default()),
            procs: Rc::new(VecModel::default()),
            nodes: Rc::new(VecModel::default()),
        }
    }
}

pub(crate) fn in_place<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    if model.row_count() == rows.len() {
        for (i, r) in rows.into_iter().enumerate() {
            if model.row_data(i).as_ref() != Some(&r) {
                model.set_row_data(i, r);
            }
        }
    } else {
        model.set_vec(rows);
    }
}

fn nice_top(v: f64) -> f64 {
    for t in [5.0, 10.0, 25.0, 50.0, 100.0, 200.0, 400.0, 800.0, 1600.0] {
        if v <= t {
            return t;
        }
    }
    v
}

fn path(values: impl Iterator<Item = f64>, top: f64) -> String {
    let v: Vec<f64> = values.collect();
    if v.len() < 2 {
        return String::new();
    }
    let step = 1000.0 / (HISTORY - 1) as f64;
    let x0 = 1000.0 - step * (v.len() - 1) as f64;
    let mut s = String::with_capacity(v.len() * 14);
    for (i, y) in v.iter().enumerate() {
        let y = 100.0 - (y / top).clamp(0.0, 1.0) * 100.0;
        s.push_str(&format!(
            "{}{:.1} {:.1} ",
            if i == 0 { "M " } else { "L " },
            x0 + step * i as f64,
            y
        ));
    }
    s
}

fn delta(now: f64, base: Option<f64>, unit: &str, digits: usize) -> String {
    match base {
        Some(b) => format!("{:+.*}{unit} vs baseline", digits, now - b),
        None => String::new(),
    }
}

impl Live {
    /// Open windows tell the worker to measure; it starts on first use.
    pub fn set_open(&self, open: bool) {
        self.wanted.store(open, Ordering::Relaxed);
        if !open {
            self.xruns_at_open.set(None);
            return;
        }
        if !self.started.replace(true) {
            let (wanted, latest) = (self.wanted.clone(), self.latest.clone());
            let ui = std::process::id();
            let _ = std::thread::Builder::new()
                .name("patch-perf".into())
                .spawn(move || {
                    let mut sampler = perf::Sampler::new(Some(ui));
                    loop {
                        if wanted.load(Ordering::Relaxed) {
                            let s = sampler.sample();
                            *latest.lock().unwrap() = Some(s);
                            std::thread::sleep(std::time::Duration::from_secs(1));
                        } else {
                            std::thread::sleep(std::time::Duration::from_millis(400));
                        }
                    }
                });
        }
    }

    pub fn record_baseline(&self) {
        *self.recording.borrow_mut() = Some(Vec::new());
    }

    pub fn clear_baseline(&self) {
        *self.baseline.borrow_mut() = None;
        let _ = std::fs::remove_file(perf::baseline_path());
    }

    /// The report as text (Copy report), over the last ten seconds.
    pub fn report(&self) -> Option<String> {
        let h = self.history.borrow();
        let last = h.back()?;
        let recent: Vec<Snapshot> = h.iter().rev().take(BASELINE_SECS).cloned().collect();
        Some(perf::report(
            last,
            &perf::summarize(&recent),
            self.baseline.borrow().as_ref(),
        ))
    }

    /// Take the newest snapshot, if there is one: true when the window
    /// needs its scalars set again.
    /// `others`: Patch windows open besides this one.
    pub fn update(&self, ui_frame_ms: f64, others: Vec<String>) -> bool {
        *self.others.borrow_mut() = others;
        let Some(mut snap) = self.latest.lock().unwrap().take() else {
            return false;
        };
        snap.ui_frame_ms = ui_frame_ms;
        {
            let mut h = self.history.borrow_mut();
            if h.len() == HISTORY {
                h.pop_front();
            }
            h.push_back(snap.clone());
        }
        let done = {
            let mut rec = self.recording.borrow_mut();
            match rec.as_mut() {
                Some(v) => {
                    v.push(snap.clone());
                    v.len() >= BASELINE_SECS
                }
                None => false,
            }
        };
        if done {
            let v = self.recording.borrow_mut().take().unwrap_or_default();
            let sum = perf::summarize(&v);
            let _ = perf::save_baseline(&sum);
            *self.baseline.borrow_mut() = Some(sum);
        }
        let (p, n) = (snap.service(), snap.totals(Side::Native));
        if self.xruns_at_open.get().is_none() {
            self.xruns_at_open.set(Some((p.xruns, n.xruns)));
        }
        let (px0, nx0) = self.xruns_at_open.get().unwrap_or((0, 0));
        let (new_px, new_nx) = (p.xruns.saturating_sub(px0), n.xruns.saturating_sub(nx0));
        let base = self.baseline.borrow().clone();
        let share = |a: f64, b: f64| {
            if a + b > 0.0 {
                (a / (a + b)) as f32
            } else {
                -1.0
            }
        };
        let cycle_ms = snap.cycle_us / 1e3;
        let (ui, bar) = snap.ui();
        let pct = |x: Option<f64>| x.map(|v| format!("{v:.1} %")).unwrap_or_else(|| "—".into());
        in_place(
            &self.tiles,
            vec![
                PerfTile {
                    label: "CPU · service · % of a core".into(),
                    patch: format!("{:.1} %", p.cpu).into(),
                    native: format!("{:.1} %", n.cpu).into(),
                    share: share(p.cpu, n.cpu),
                    delta: delta(p.cpu, base.as_ref().map(|b| b.patch.cpu), " %", 1).into(),
                    warn: p.cpu > 10.0,
                },
                PerfTile {
                    label: "memory · service".into(),
                    patch: format!("{:.0} MB", p.rss_mb).into(),
                    native: format!("{:.0} MB", n.rss_mb).into(),
                    share: share(p.rss_mb, n.rss_mb),
                    delta: delta(p.rss_mb, base.as_ref().map(|b| b.patch.rss_mb), " MB", 0).into(),
                    warn: false,
                },
                PerfTile {
                    label: format!("DSP · per {cycle_ms:.1} ms cycle").into(),
                    patch: format!("{:.1} µs", p.dsp_us).into(),
                    native: format!("{:.1} µs", n.dsp_us).into(),
                    share: share(p.dsp_us, n.dsp_us),
                    delta: delta(p.dsp_us, base.as_ref().map(|b| b.patch.dsp_us), " µs", 1).into(),
                    warn: snap.cycle_us > 0.0 && p.dsp_us > snap.cycle_us * 0.25,
                },
                PerfTile {
                    label: "xruns · while open".into(),
                    patch: format!("{new_px}").into(),
                    native: format!("{new_nx}").into(),
                    share: -1.0,
                    delta: format!("since start {} · {}", p.xruns, n.xruns).into(),
                    warn: new_px > 0,
                },
                PerfTile {
                    label: "windows · CPU (native: the bar)".into(),
                    patch: pct(ui.map(|u| u.cpu)).into(),
                    native: pct(bar.map(|b| b.cpu)).into(),
                    share: -1.0,
                    delta: format!("worst frame {:.1} ms", snap.ui_frame_ms).into(),
                    warn: ui.is_some_and(|u| u.cpu > 20.0),
                },
                PerfTile {
                    label: "windows · GPU".into(),
                    patch: format!(
                        "{} · {}",
                        pct(ui.and_then(|u| u.gpu)),
                        ui.and_then(|u| u.vram_mb)
                            .map(|v| format!("{v:.0} MB"))
                            .unwrap_or_else(|| "—".into())
                    )
                    .into(),
                    native: format!(
                        "{} · {}",
                        pct(bar.and_then(|b| b.gpu)),
                        bar.and_then(|b| b.vram_mb)
                            .map(|v| format!("{v:.0} MB"))
                            .unwrap_or_else(|| "—".into())
                    )
                    .into(),
                    share: -1.0,
                    delta: String::new().into(),
                    warn: ui.and_then(|u| u.gpu).is_some_and(|g| g > 10.0),
                },
            ],
        );
        in_place(
            &self.procs,
            snap.procs
                .iter()
                .map(|x| PerfProc {
                    name: x.name.clone().into(),
                    role: x.role.clone().into(),
                    side: match x.side {
                        Side::Patch => "patch",
                        Side::Native => "native",
                        Side::Bar => "bar",
                    }
                    .into(),
                    cpu: format!("{:.1} %", x.cpu).into(),
                    mem: format!("{:.0} MB", x.rss_mb).into(),
                    threads: format!("{}", x.threads).into(),
                    gpu: match (x.gpu, x.vram_mb) {
                        (Some(g), Some(v)) => format!("{g:.0} % · {v:.0} MB"),
                        (None, Some(v)) => format!("{v:.0} MB"),
                        (Some(g), None) => format!("{g:.0} %"),
                        _ => "—".into(),
                    }
                    .into(),
                    warn: x.side == Side::Patch && x.cpu > 20.0,
                })
                .collect(),
        );
        in_place(
            &self.nodes,
            snap.nodes
                .iter()
                .map(|x| PerfNode {
                    name: x.name.clone().into(),
                    busy: format!("{:.1} µs", x.busy_us).into(),
                    load: if snap.cycle_us > 0.0 {
                        (x.busy_us / snap.cycle_us) as f32
                    } else {
                        0.0
                    },
                    err: format!("{}", x.err).into(),
                    patch: x.patch,
                    driver: x.driver,
                    warn: snap.cycle_us > 0.0 && x.busy_us > snap.cycle_us * 0.1,
                })
                .collect(),
        );
        true
    }

    /// The window's scalars (headline, lines, chart), from the history.
    pub fn apply(&self, s: &PerfState) {
        let h = self.history.borrow();
        let Some(snap) = h.back() else {
            s.set_headline("measuring…".into());
            return;
        };
        let (p, n) = (snap.service(), snap.totals(Side::Native));
        let e = &snap.engine;
        let ratio = if n.cpu > 0.05 {
            format!(" ({:.1}× native)", p.cpu / n.cpu)
        } else {
            String::new()
        };
        s.set_headline(
            format!(
                "Patch service {:.1} % of a core{ratio} · native {:.1} % · DSP {:.1} µs of a {:.1} ms cycle ({:.2} %)",
                p.cpu,
                n.cpu,
                p.dsp_us,
                snap.cycle_us / 1e3,
                if snap.cycle_us > 0.0 { p.dsp_us / snap.cycle_us * 100.0 } else { 0.0 }
            )
            .into(),
        );
        // Fair only when Patch does the native layer's job and no more.
        let mut why = vec![];
        let others = self.others.borrow();
        if !others.is_empty() {
            why.push(format!(
                "{} other Patch window{} open ({}): they ask the service for data",
                others.len(),
                if others.len() == 1 { "" } else { "s" },
                others.join(", ")
            ));
        }
        if !e.decks_playing.is_empty() {
            why.push(format!("Deck {} playing", e.decks_playing.join(" and ")));
        }
        if e.ai_acting {
            why.push("Auto Mix is mixing".into());
        }
        s.set_fair(why.is_empty());
        s.set_fairness(
            if why.is_empty() {
                "FAIR · the service does only what the native layer does: no other Patch window, no deck playing, Auto Mix not mixing".to_string()
            } else {
                format!("NOT A FAIR COMPARISON · {} · close them, or run: omarchy-patch perf fair", why.join(" · "))
            }
            .into(),
        );
        s.set_engine_line(
            if e.running {
                format!(
                    "engine callback {:.1} µs avg · {:.1} µs worst ({:.2} % of the cycle) · {} · ring drops {} · UI worst frame {:.2} ms · {}",
                    e.cost_avg_us,
                    e.cost_max_us,
                    e.load_max * 100.0,
                    if e.realtime { format!("real-time {}", e.sched) } else { "NOT real-time: the engine can be pre-empted".into() },
                    e.dropped,
                    snap.ui_frame_ms,
                    e.backend
                )
            } else {
                "the Patch service is not running: only the native layer is measured".into()
            }
            .into(),
        );
        s.set_engine_warn(e.running && (!e.realtime || e.dropped > 0 || e.load_max > 0.5));
        let sys = &snap.system;
        s.set_system_line(
            format!(
                "system CPU {:.1} % of {} cores · load {:.2}{} · the bar (Quickshell) is listed apart and counted on neither side · this window's own cost is in the Patch UI",
                sys.cpu,
                sys.cores,
                sys.load1,
                match (sys.gpu, sys.gpu_power_w) {
                    (Some(g), Some(w)) => format!(" · {} {g:.0} % · {w:.1} W", sys.gpu_name),
                    (Some(g), None) => format!(" · GPU {g:.0} %"),
                    _ => String::new(),
                }
            )
            .into(),
        );
        let top = nice_top(
            h.iter()
                .map(|x| {
                    x.service()
                        .cpu
                        .max(x.totals(Side::Native).cpu)
                        .max(x.ui().0.map(|u| u.cpu).unwrap_or(0.0))
                })
                .fold(1.0, f64::max),
        );
        s.set_chart_top(format!("top {top:.0} %").into());
        s.set_line_patch(path(h.iter().map(|x| x.service().cpu), top).into());
        s.set_line_native(path(h.iter().map(|x| x.totals(Side::Native).cpu), top).into());
        s.set_line_gpu(
            path(
                h.iter().map(|x| x.ui().0.map(|u| u.cpu).unwrap_or(0.0)),
                top,
            )
            .into(),
        );
        let rec = self.recording.borrow().as_ref().map(|v| v.len());
        s.set_recording(rec.is_some());
        let base = self.baseline.borrow();
        s.set_baseline(
            base.as_ref()
                .map(|b| b.when.clone())
                .unwrap_or_default()
                .into(),
        );
        s.set_note(
            match (rec, base.as_ref()) {
                (Some(n), _) => format!("baseline: {n}/{BASELINE_SECS} s"),
                (None, Some(b)) => format!("baseline {} · {}", b.when, b.version),
                _ => String::new(),
            }
            .into(),
        );
    }
}
