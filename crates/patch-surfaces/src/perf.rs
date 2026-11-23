//! Performance Unit: what Patch costs, next to what the native Omarchy
//! audio layer (PipeWire, its Pulse server and WirePlumber) costs.
//!
//! Everything is measured, never estimated: CPU and memory from /proc, GPU
//! from NVML (`nvidia-smi pmon`) or the kernel's DRM fdinfo, per-node DSP
//! time and xruns from the PipeWire graph (`pw-top`), callback cost from the
//! engine. A [`Sampler`] runs off the UI thread, once a second, and only
//! while a Performance window is open (or `omarchy-patch perf` runs).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

/// Who a process works for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Patch,
    Native,
    /// The bar (Quickshell): shown for reference, counted on neither side.
    Bar,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proc {
    pub name: String,
    pub role: String,
    pub side: Side,
    pub pid: u32,
    /// % of one core.
    pub cpu: f64,
    pub rss_mb: f64,
    pub threads: u32,
    /// GPU busy %, when the driver tells.
    pub gpu: Option<f64>,
    pub vram_mb: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub name: String,
    pub patch: bool,
    pub driver: bool,
    pub busy_us: f64,
    pub wait_us: f64,
    /// Share of the cycle spent processing (B/Q).
    pub bq: f64,
    /// Xruns counted by PipeWire since the node started.
    pub err: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct System {
    pub cores: u32,
    /// All CPUs, % of the whole machine.
    pub cpu: f64,
    pub load1: f64,
    pub gpu_name: String,
    pub gpu: Option<f64>,
    pub gpu_power_w: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Engine {
    pub running: bool,
    pub rate: u32,
    pub quantum: u32,
    pub cost_avg_us: f64,
    pub cost_max_us: f64,
    pub load_max: f64,
    pub dropped: u64,
    pub realtime: bool,
    pub sched: String,
    pub backend: String,
    /// Doing more than the native layer does: decks playing, AI mixing.
    pub decks_playing: Vec<String>,
    pub ai_acting: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub procs: Vec<Proc>,
    pub nodes: Vec<Node>,
    pub system: System,
    pub engine: Engine,
    /// Worst UI frame in the last second (the UI fills this in).
    pub ui_frame_ms: f64,
    /// The graph's cycle (µs), from the driver.
    pub cycle_us: f64,
}

/// One side's total.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Totals {
    pub cpu: f64,
    pub rss_mb: f64,
    pub gpu: f64,
    pub vram_mb: f64,
    /// DSP time of this side's graph nodes per cycle.
    pub dsp_us: f64,
    pub xruns: u64,
}

impl Snapshot {
    /// The Patch service alone (the engine and its graph nodes): what does
    /// the native layer's job. Windows are the UI, like the bar.
    pub fn service(&self) -> Totals {
        let mut t = self.totals(Side::Patch);
        let ui: Vec<&Proc> = self
            .procs
            .iter()
            .filter(|p| p.side == Side::Patch && !p.role.contains("engine"))
            .collect();
        for p in ui {
            t.cpu -= p.cpu;
            t.rss_mb -= p.rss_mb;
            t.gpu -= p.gpu.unwrap_or(0.0);
            t.vram_mb -= p.vram_mb.unwrap_or(0.0);
        }
        t
    }

    /// The windows' own figures: (Patch UI, the bar).
    pub fn ui(&self) -> (Option<&Proc>, Option<&Proc>) {
        (
            self.procs
                .iter()
                .find(|p| p.side == Side::Patch && !p.role.contains("engine")),
            self.procs.iter().find(|p| p.side == Side::Bar),
        )
    }

    pub fn totals(&self, side: Side) -> Totals {
        let mut t = Totals::default();
        for p in self.procs.iter().filter(|p| p.side == side) {
            t.cpu += p.cpu;
            t.rss_mb += p.rss_mb;
            t.gpu += p.gpu.unwrap_or(0.0);
            t.vram_mb += p.vram_mb.unwrap_or(0.0);
        }
        for n in &self.nodes {
            if (side == Side::Patch) == n.patch && side != Side::Bar {
                t.dsp_us += n.busy_us;
                t.xruns += n.err;
            }
        }
        t
    }
}

// ---- /proc --------------------------------------------------------------

fn ticks_per_sec() -> f64 {
    // SAFETY: sysconf has no preconditions.
    let t = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if t > 0 { t as f64 } else { 100.0 }
}

/// (utime + stime ticks, rss pages, threads) of a process.
fn proc_stat(pid: u32) -> Option<(u64, u64, u32)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    let n = |i: usize| rest.get(i).and_then(|v| v.parse::<u64>().ok());
    Some((n(11)? + n(12)?, n(21)?, n(17)? as u32))
}

/// (busy, total) jiffies of all CPUs.
fn cpu_total() -> Option<(u64, u64)> {
    let s = std::fs::read_to_string("/proc/stat").ok()?;
    let line = s.lines().next()?;
    let v: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|x| x.parse().ok())
        .collect();
    let total: u64 = v.iter().take(8).sum();
    let idle = v.get(3)? + v.get(4).copied().unwrap_or(0);
    Some((total - idle, total))
}

fn comm(pid: u32) -> Option<String> {
    Some(
        std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .ok()?
            .trim()
            .to_string(),
    )
}

fn cmdline(pid: u32) -> String {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
        .unwrap_or_default()
}

/// Seconds since a process started.
fn age_secs(pid: u32) -> Option<f64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let start: f64 = stat
        .rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()?;
    let up: f64 = std::fs::read_to_string("/proc/uptime")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some(up - start / ticks_per_sec())
}

fn uid_of(pid: u32) -> Option<u32> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    s.lines()
        .find_map(|l| l.strip_prefix("Uid:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// The audio processes of this user: (pid, name, role, side).
fn discover(engine: Option<u32>, ui: Option<u32>) -> Vec<(u32, String, String, Side)> {
    // SAFETY: getuid has no preconditions.
    let me = unsafe { libc::getuid() };
    let mut out = Vec::new();
    if let Some(p) = engine {
        out.push((
            p,
            "omarchy-patch engine".into(),
            "real-time engine".into(),
            Side::Patch,
        ));
    }
    if let Some(p) = ui {
        out.push((
            p,
            "omarchy-patch".into(),
            "windows (this UI)".into(),
            Side::Patch,
        ));
    }
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return out;
    };
    for e in dir.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Some(name) = comm(pid) else { continue };
        let (role, side) = match name.as_str() {
            "pipewire" => ("audio graph server", Side::Native),
            "pipewire-pulse" => ("PulseAudio server for apps", Side::Native),
            "wireplumber" => ("session manager", Side::Native),
            "quickshell" if cmdline(pid).contains("omarchy/shell") => {
                ("the bar and its audio menu", Side::Bar)
            }
            // The Patch UI, seen from outside (the CLI): not the engine, and
            // older than a few seconds (not a passing CLI call).
            "omarchy-patch"
                if ui.is_none()
                    && Some(pid) != engine
                    && pid != std::process::id()
                    && !cmdline(pid).contains(" engine")
                    && age_secs(pid).is_some_and(|a| a > 5.0) =>
            {
                ("windows", Side::Patch)
            }
            _ => continue,
        };
        if uid_of(pid) != Some(me) {
            continue;
        }
        out.push((pid, name, role.into(), side));
    }
    out
}

// ---- GPU ----------------------------------------------------------------

/// Per-process GPU busy % and memory from NVML, and the GPU's own figures.
fn nvidia() -> Option<(
    HashMap<u32, (Option<f64>, Option<f64>)>,
    String,
    Option<f64>,
    Option<f64>,
)> {
    let out = Command::new("nvidia-smi")
        .args(["pmon", "-c", "1", "-s", "um"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut map = HashMap::new();
    for l in String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.starts_with('#'))
    {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 10 {
            continue;
        }
        let num = |s: &str| s.parse::<f64>().ok();
        if let Ok(pid) = f[1].parse::<u32>() {
            map.insert(pid, (num(f[3]).or(Some(0.0)), num(f[9])));
        }
    }
    let q = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,utilization.gpu,power.draw",
            "--format=csv,noheader,nounits",
        ])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let line = String::from_utf8_lossy(&q.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    let f: Vec<&str> = line.split(',').map(str::trim).collect();
    Some((
        map,
        f.first().unwrap_or(&"NVIDIA").to_string(),
        f.get(1).and_then(|v| v.parse().ok()),
        f.get(2).and_then(|v| v.parse().ok()),
    ))
}

/// Busy time (ns, all engines) and memory of a process from DRM fdinfo
/// (Intel, AMD and other kernel drivers), one entry per DRM client.
fn drm_fdinfo(pid: u32) -> Option<(u64, f64)> {
    let mut clients: HashMap<String, (u64, f64)> = HashMap::new();
    for e in std::fs::read_dir(format!("/proc/{pid}/fdinfo"))
        .ok()?
        .flatten()
    {
        let Ok(s) = std::fs::read_to_string(e.path()) else {
            continue;
        };
        if !s.contains("drm-client-id") {
            continue;
        }
        let mut id = String::new();
        let (mut ns, mut mem) = (0u64, 0.0f64);
        for l in s.lines() {
            if let Some(v) = l.strip_prefix("drm-client-id:") {
                id = v.trim().to_string();
            } else if let Some(v) = l.strip_prefix("drm-engine-") {
                ns += v
                    .split(':')
                    .nth(1)
                    .and_then(|x| x.split_whitespace().next())
                    .and_then(|x| x.parse::<u64>().ok())
                    .unwrap_or(0);
            } else if l.starts_with("drm-resident-") || l.starts_with("drm-total-") {
                let mut it = l.split(':').nth(1).unwrap_or("").split_whitespace();
                let n = it.next().and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
                let unit = match it.next() {
                    Some("KiB") => 1.0 / 1024.0,
                    Some("MiB") => 1.0,
                    Some("GiB") => 1024.0,
                    _ => 1.0 / 1_048_576.0,
                };
                if l.starts_with("drm-resident-") {
                    mem += n * unit;
                }
            }
        }
        clients.insert(id, (ns, mem));
    }
    (!clients.is_empty()).then(|| {
        clients
            .values()
            .fold((0, 0.0), |a, c| (a.0 + c.0, a.1 + c.1))
    })
}

// ---- PipeWire graph -----------------------------------------------------

fn micros(s: &str) -> Option<f64> {
    let (n, mul) = if let Some(v) = s.strip_suffix("us") {
        (v, 1.0)
    } else if let Some(v) = s.strip_suffix("ms") {
        (v, 1e3)
    } else if let Some(v) = s.strip_suffix('s') {
        (v, 1e6)
    } else {
        (s, 1.0)
    };
    n.parse::<f64>().ok().map(|v| v * mul)
}

/// The running nodes of the graph from `pw-top -b` output (its last frame).
pub fn parse_pw_top(text: &str) -> Vec<Node> {
    let frames: Vec<&str> = text.split("S   ID").collect();
    let last = frames.last().copied().unwrap_or("");
    let mut out = Vec::new();
    for l in last.lines().skip(1) {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 10 || f[0] != "R" {
            continue;
        }
        let mut rest = &f[9..];
        // FORMAT: e.g. "S32LE 2 48000" (absent on some nodes).
        if rest.len() > 3
            && rest[0]
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            && rest[1].parse::<u32>().is_ok()
            && rest[2].parse::<u32>().is_ok()
        {
            rest = &rest[3..];
        }
        let driver = rest.first() != Some(&"+");
        if !driver {
            rest = &rest[1..];
        }
        let name = rest.join(" ");
        let (Some(wait), Some(busy)) = (micros(f[4]), micros(f[5])) else {
            continue;
        };
        out.push(Node {
            patch: name.starts_with("patch."),
            driver,
            busy_us: busy,
            wait_us: wait,
            bq: f[7].parse().unwrap_or(0.0),
            err: f[8].parse().unwrap_or(0),
            name,
        });
    }
    out
}

fn pw_top() -> Vec<Node> {
    Command::new("pw-top")
        .args(["-b", "-n", "2"])
        .stderr(Stdio::null())
        .output()
        .map(|o| parse_pw_top(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

// ---- the sampler --------------------------------------------------------

pub struct Sampler {
    last: HashMap<u32, (u64, Instant)>,
    last_gpu_ns: HashMap<u32, (u64, Instant)>,
    last_cpu: Option<(u64, u64)>,
    procs: Vec<(u32, String, String, Side)>,
    found_at: Option<Instant>,
    ui: Option<u32>,
    tick: f64,
    page_mb: f64,
}

impl Sampler {
    /// `ui`: the Patch UI's pid, when the sampler runs inside it.
    pub fn new(ui: Option<u32>) -> Sampler {
        // SAFETY: sysconf has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as f64;
        Sampler {
            last: HashMap::new(),
            last_gpu_ns: HashMap::new(),
            last_cpu: None,
            procs: Vec::new(),
            found_at: None,
            ui,
            tick: ticks_per_sec(),
            page_mb: page / 1_048_576.0,
        }
    }

    /// One measurement. CPU and GPU figures are averages since the previous
    /// call (the first call has none).
    pub fn sample(&mut self) -> Snapshot {
        let status =
            patch_engine::client::call(&serde_json::json!({"op": "status"})).unwrap_or(Value::Null);
        let f = |v: &Value| v.as_f64().unwrap_or(0.0);
        let engine = Engine {
            running: !status.is_null(),
            rate: status["rate"].as_u64().unwrap_or(0) as u32,
            quantum: status["quantum"].as_u64().unwrap_or(0) as u32,
            cost_avg_us: f(&status["cost_avg_ns"]) / 1e3,
            cost_max_us: f(&status["cost_max_ns"]) / 1e3,
            load_max: f(&status["load_max"]),
            dropped: status["dropped"].as_u64().unwrap_or(0),
            realtime: status["realtime"].as_bool().unwrap_or(false),
            sched: status["sched"].as_str().unwrap_or("").to_string(),
            backend: status["backend"].as_str().unwrap_or("").to_string(),
            decks_playing: if status.is_null() {
                vec![]
            } else {
                ["a", "b"]
                    .iter()
                    .filter(|d| {
                        patch_engine::client::call(
                            &serde_json::json!({"op": "deck_status", "deck": d}),
                        )
                        .is_ok_and(|v| v["playing"] == true)
                    })
                    .map(|d| d.to_uppercase())
                    .collect()
            },
            ai_acting: !status.is_null()
                && patch_engine::client::call(&serde_json::json!({"op": "ai_state"}))
                    .is_ok_and(|v| v["mode"] == "act"),
        };
        let engine_pid = status["pid"].as_u64().map(|p| p as u32);
        // Processes come and go rarely: look again every 10 s.
        let stale = self.found_at.is_none_or(|t| t.elapsed().as_secs() >= 10)
            || engine_pid.is_some_and(|p| !self.procs.iter().any(|x| x.0 == p));
        if stale {
            self.procs = discover(engine_pid, self.ui);
            self.found_at = Some(Instant::now());
        }
        let nv = nvidia();
        let now = Instant::now();
        let mut procs = Vec::new();
        for (pid, name, role, side) in &self.procs {
            let Some((ticks, rss, threads)) = proc_stat(*pid) else {
                continue;
            };
            let cpu = self.last.insert(*pid, (ticks, now)).map(|(t0, at)| {
                let secs = now.duration_since(at).as_secs_f64().max(1e-3);
                ticks.saturating_sub(t0) as f64 / self.tick / secs * 100.0
            });
            let (mut gpu, mut vram) = nv
                .as_ref()
                .and_then(|n| n.0.get(pid).copied())
                .unwrap_or((None, None));
            if let Some((ns, mem)) = drm_fdinfo(*pid) {
                let prev = self.last_gpu_ns.insert(*pid, (ns, now));
                if gpu.is_none() {
                    gpu = prev.map(|(n0, at)| {
                        ns.saturating_sub(n0) as f64
                            / (now.duration_since(at).as_secs_f64().max(1e-3) * 1e9)
                            * 100.0
                    });
                }
                if vram.is_none() && mem > 0.0 {
                    vram = Some(mem);
                }
            }
            procs.push(Proc {
                name: name.clone(),
                role: role.clone(),
                side: *side,
                pid: *pid,
                cpu: cpu.unwrap_or(0.0),
                rss_mb: rss as f64 * self.page_mb,
                threads,
                gpu,
                vram_mb: vram,
            });
        }
        let total = cpu_total();
        let cpu = match (self.last_cpu, total) {
            (Some((b0, t0)), Some((b1, t1))) if t1 > t0 => {
                (b1 - b0) as f64 / (t1 - t0) as f64 * 100.0
            }
            _ => 0.0,
        };
        self.last_cpu = total;
        let nodes = pw_top();
        let cycle_us = if engine.rate > 0 {
            engine.quantum as f64 / engine.rate as f64 * 1e6
        } else {
            0.0
        };
        let load1 = std::fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse().ok()))
            .unwrap_or(0.0);
        let system = System {
            cores: std::thread::available_parallelism()
                .map(|n| n.get() as u32)
                .unwrap_or(1),
            cpu,
            load1,
            gpu_name: nv.as_ref().map(|n| n.1.clone()).unwrap_or_default(),
            gpu: nv.as_ref().and_then(|n| n.2),
            gpu_power_w: nv.as_ref().and_then(|n| n.3),
        };
        Snapshot {
            procs,
            nodes,
            system,
            engine,
            ui_frame_ms: 0.0,
            cycle_us,
        }
    }
}

// ---- averages, baseline, report ------------------------------------------

/// Averages over several snapshots (a baseline, or the CLI's report).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Summary {
    pub seconds: u32,
    pub when: String,
    pub version: String,
    pub patch: Totals,
    pub native: Totals,
    pub callback_avg_us: f64,
    pub callback_max_us: f64,
    pub ui_frame_ms: f64,
    pub cycle_us: f64,
}

pub fn summarize(snaps: &[Snapshot]) -> Summary {
    let n = snaps.len().max(1) as f64;
    let avg = |side: Side| {
        let mut t = Totals::default();
        for s in snaps {
            let x = s.totals(side);
            t.cpu += x.cpu / n;
            t.rss_mb += x.rss_mb / n;
            t.gpu += x.gpu / n;
            t.vram_mb += x.vram_mb / n;
            t.dsp_us += x.dsp_us / n;
        }
        // New xruns during the measurement (the graph counts since start).
        if let (Some(a), Some(b)) = (snaps.first(), snaps.last()) {
            t.xruns = b.totals(side).xruns.saturating_sub(a.totals(side).xruns);
        }
        t
    };
    Summary {
        seconds: snaps.len() as u32,
        when: now_text(),
        version: patch_core::version_string(),
        patch: avg(Side::Patch),
        native: avg(Side::Native),
        callback_avg_us: snaps.iter().map(|s| s.engine.cost_avg_us).sum::<f64>() / n,
        callback_max_us: snaps
            .iter()
            .map(|s| s.engine.cost_max_us)
            .fold(0.0, f64::max),
        ui_frame_ms: snaps.iter().map(|s| s.ui_frame_ms).fold(0.0, f64::max),
        cycle_us: snaps.last().map(|s| s.cycle_us).unwrap_or(0.0),
    }
}

fn now_text() -> String {
    Command::new("date")
        .arg("+%Y-%m-%d %H:%M")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

pub fn baseline_path() -> PathBuf {
    patch_engine::session::state_dir().join("perf-baseline.json")
}

pub fn load_baseline() -> Option<Summary> {
    serde_json::from_str(&std::fs::read_to_string(baseline_path()).ok()?).ok()
}

pub fn save_baseline(s: &Summary) -> std::io::Result<()> {
    let p = baseline_path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(p, serde_json::to_string_pretty(s).unwrap_or_default())
}

/// Plain text: the CLI's output and the window's "Copy report".
pub fn report(last: &Snapshot, sum: &Summary, base: Option<&Summary>) -> String {
    let mut o = String::new();
    let line = |o: &mut String, s: String| {
        o.push_str(&s);
        o.push('\n');
    };
    line(
        &mut o,
        format!(
            "Patch performance · {} · {} s · {}",
            sum.version, sum.seconds, sum.when
        ),
    );
    line(&mut o, String::new());
    line(
        &mut o,
        format!("{:<18} {:>12} {:>12}", "", "Patch", "Native"),
    );
    let row =
        |o: &mut String, k: &str, p: String, n: String| line(o, format!("{k:<18} {p:>12} {n:>12}"));
    row(
        &mut o,
        "CPU (% of a core)",
        format!("{:.2}", sum.patch.cpu),
        format!("{:.2}", sum.native.cpu),
    );
    row(
        &mut o,
        "memory",
        format!("{:.0} MB", sum.patch.rss_mb),
        format!("{:.0} MB", sum.native.rss_mb),
    );
    row(
        &mut o,
        "GPU",
        format!("{:.1} %", sum.patch.gpu),
        format!("{:.1} %", sum.native.gpu),
    );
    row(
        &mut o,
        "GPU memory",
        format!("{:.0} MB", sum.patch.vram_mb),
        format!("{:.0} MB", sum.native.vram_mb),
    );
    row(
        &mut o,
        "DSP per cycle",
        format!("{:.1} µs", sum.patch.dsp_us),
        format!("{:.1} µs", sum.native.dsp_us),
    );
    row(
        &mut o,
        "new xruns",
        format!("{}", sum.patch.xruns),
        format!("{}", sum.native.xruns),
    );
    line(&mut o, String::new());
    line(
        &mut o,
        format!(
            "engine callback    {:.1} µs avg · {:.1} µs worst · cycle {:.0} µs",
            sum.callback_avg_us, sum.callback_max_us, sum.cycle_us
        ),
    );
    if sum.ui_frame_ms > 0.0 {
        line(
            &mut o,
            format!("UI worst frame     {:.2} ms", sum.ui_frame_ms),
        );
    }
    line(
        &mut o,
        format!(
            "engine             {} · {}",
            if last.engine.realtime {
                last.engine.sched.as_str()
            } else {
                "NOT real-time"
            },
            last.engine.backend
        ),
    );
    line(
        &mut o,
        format!(
            "system             CPU {:.1} % of {} cores · load {:.2}{}",
            last.system.cpu,
            last.system.cores,
            last.system.load1,
            match (last.system.gpu, last.system.gpu_power_w) {
                (Some(g), Some(w)) => format!(" · {} {g:.0} % {w:.1} W", last.system.gpu_name),
                _ => String::new(),
            }
        ),
    );
    line(&mut o, String::new());
    line(&mut o, "processes".into());
    for p in &last.procs {
        line(
            &mut o,
            format!(
                "  {:<24} {:<8} {:>6.2} % {:>6.0} MB {:>3} thr {:>7} {:>8}",
                p.name,
                format!("{:?}", p.side).to_lowercase(),
                p.cpu,
                p.rss_mb,
                p.threads,
                p.gpu.map(|g| format!("{g:.0} % gpu")).unwrap_or_default(),
                p.vram_mb.map(|v| format!("{v:.0} MB")).unwrap_or_default()
            ),
        );
    }
    line(&mut o, "graph (running nodes)".into());
    for n in &last.nodes {
        line(
            &mut o,
            format!(
                "  {:<44} {:>8.1} µs {:>6.2} B/Q {:>4} err{}",
                n.name,
                n.busy_us,
                n.bq,
                n.err,
                if n.driver { "  driver" } else { "" }
            ),
        );
    }
    if let Some(b) = base {
        line(&mut o, String::new());
        line(
            &mut o,
            format!("against the baseline of {} ({}):", b.when, b.version),
        );
        line(
            &mut o,
            format!(
                "  Patch CPU {:+.2} · memory {:+.0} MB · GPU {:+.1} % · DSP {:+.1} µs · callback {:+.1} µs",
                sum.patch.cpu - b.patch.cpu,
                sum.patch.rss_mb - b.patch.rss_mb,
                sum.patch.gpu - b.patch.gpu,
                sum.patch.dsp_us - b.patch.dsp_us,
                sum.callback_avg_us - b.callback_avg_us
            ),
        );
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    const PW_TOP: &str = "S   ID  QUANT   RATE    WAIT    BUSY   W/Q   B/Q  ERR FORMAT           NAME \n\
R   56   1024  48000  90.0us  10.0us  0.00  0.00    1    S32LE 2 48000 old\n\
S   ID  QUANT   RATE    WAIT    BUSY   W/Q   B/Q  ERR FORMAT           NAME \n\
S   29      0      0    ---     ---   ---   ---     0                  Dummy-Driver\n\
R   56   1024  48000  82.8us  13.3us  0.00  0.01   11    S32LE 2 48000 alsa_output.pci-0000_80_1f.3.analog-stereo\n\
R   68      0      0   5.6us   4.4us  0.00  0.00    0     F32P 2 48000  + patch.program\n\
R  180      0      0   7.0us  1.2ms  0.00  0.03    2                   + patch.engine\n\
R  103      0      0   0.0us   0.0us  ???   ???     0                  PortAudio\n";

    #[test]
    fn the_graph_is_read_from_the_last_frame() {
        let n = parse_pw_top(PW_TOP);
        assert_eq!(n.len(), 4);
        assert_eq!(n[0].name, "alsa_output.pci-0000_80_1f.3.analog-stereo");
        assert!(n[0].driver && !n[0].patch);
        assert_eq!(n[0].err, 11);
        assert_eq!(n[1].name, "patch.program");
        assert!(n[1].patch && !n[1].driver);
        assert_eq!(n[2].busy_us, 1200.0);
        assert_eq!(n[3].name, "PortAudio");
    }

    #[test]
    fn totals_split_patch_from_native() {
        let s = Snapshot {
            procs: vec![
                Proc {
                    name: "e".into(),
                    role: String::new(),
                    side: Side::Patch,
                    pid: 1,
                    cpu: 1.0,
                    rss_mb: 10.0,
                    threads: 1,
                    gpu: None,
                    vram_mb: None,
                },
                Proc {
                    name: "u".into(),
                    role: String::new(),
                    side: Side::Patch,
                    pid: 2,
                    cpu: 2.0,
                    rss_mb: 20.0,
                    threads: 1,
                    gpu: Some(5.0),
                    vram_mb: Some(100.0),
                },
                Proc {
                    name: "pw".into(),
                    role: String::new(),
                    side: Side::Native,
                    pid: 3,
                    cpu: 0.5,
                    rss_mb: 15.0,
                    threads: 1,
                    gpu: None,
                    vram_mb: None,
                },
                Proc {
                    name: "qs".into(),
                    role: String::new(),
                    side: Side::Bar,
                    pid: 4,
                    cpu: 9.0,
                    rss_mb: 99.0,
                    threads: 1,
                    gpu: None,
                    vram_mb: None,
                },
            ],
            nodes: parse_pw_top(PW_TOP),
            ..Default::default()
        };
        let p = s.totals(Side::Patch);
        let n = s.totals(Side::Native);
        assert_eq!((p.cpu, p.rss_mb, p.gpu, p.vram_mb), (3.0, 30.0, 5.0, 100.0));
        assert_eq!(n.cpu, 0.5);
        assert!((p.dsp_us - 1204.4).abs() < 1e-9);
        assert!((n.dsp_us - 13.3).abs() < 1e-9);
        assert_eq!((p.xruns, n.xruns), (2, 11));
    }
}
