//! DJ Booth diagnostics: the timing and health figures an engineer needs to
//! trust the booth, gathered only while the Diagnostics window is open.
//!
//! Everything shown comes from the engine (stamped cycles, measured callback
//! cost, per-deck positions and underruns) or the OS (CPU); nothing is
//! estimated by the UI. Levels of certainty are shown with the values
//! (e.g. latency REPORTED).

use serde_json::{Value, json};
use std::cell::Cell;
use std::time::Instant;

/// One row: section heading (key empty) or key/value; `warn` highlights.
pub struct Row {
    pub key: String,
    pub value: String,
    pub warn: bool,
}

fn row(k: &str, v: impl Into<String>) -> Row {
    Row {
        key: k.into(),
        value: v.into(),
        warn: false,
    }
}

fn warn(k: &str, v: impl Into<String>, w: bool) -> Row {
    Row {
        key: k.into(),
        value: v.into(),
        warn: w,
    }
}

fn heading(t: &str) -> Row {
    Row {
        key: String::new(),
        value: t.into(),
        warn: false,
    }
}

/// CPU time (user + system) of a process, in clock ticks.
fn cpu_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    Some(rest.get(11)?.parse::<u64>().ok()? + rest.get(12)?.parse::<u64>().ok()?)
}

/// CPU % of a process between two calls.
#[derive(Default)]
pub struct CpuMeter {
    last: Cell<Option<(u64, Instant)>>,
}

impl CpuMeter {
    pub fn percent(&self, pid: u32) -> Option<f64> {
        let now = (cpu_ticks(pid)?, Instant::now());
        let prev = self.last.replace(Some(now));
        let (t0, at) = prev?;
        let secs = at.elapsed().as_secs_f64();
        // Linux clock ticks are 100 Hz on all mainstream configs.
        (secs > 0.05).then(|| (now.0.saturating_sub(t0)) as f64 / 100.0 / secs * 100.0)
    }
}

pub struct Inputs<'a> {
    pub status: &'a Value,
    pub clock: &'a Value,
    pub decks: &'a [(String, Value)],
    pub latency_ms: Option<f32>,
    pub latency_level: &'a str,
    pub trim_ms: f32,
    pub reference: &'a str,
    pub analysis_queue: usize,
    pub engine_cpu: Option<f64>,
    pub ui_cpu: Option<f64>,
    pub ui_frame_ms: f64,
}

pub fn collect(i: &Inputs) -> Vec<Row> {
    let st = i.status;
    let f = |v: &Value| v.as_f64().unwrap_or(0.0);
    let rate = st["rate"].as_u64().unwrap_or(0);
    let quantum = st["quantum"].as_u64().unwrap_or(0);
    let budget_us = if rate > 0 {
        quantum as f64 / rate as f64 * 1e6
    } else {
        0.0
    };
    let mut out = vec![heading("Engine")];
    out.push(row(
        "backend",
        format!(
            "PipeWire filter · {}",
            st["backend"].as_str().unwrap_or("?")
        ),
    ));
    out.push(row(
        "rate · cycle",
        format!(
            "{} Hz · {} frames ({:.2} ms)",
            rate,
            quantum,
            budget_us / 1e3
        ),
    ));
    out.push(warn(
        "callback",
        format!(
            "{:.1} µs avg · {:.1} µs worst ({:.3} % of the cycle)",
            f(&st["cost_avg_ns"]) / 1e3,
            f(&st["cost_max_ns"]) / 1e3,
            f(&st["load_max"]) * 100.0
        ),
        f(&st["load_max"]) > 0.5,
    ));
    let rt = st["realtime"].as_bool().unwrap_or(false);
    // Arch has realtime-privileges; elsewhere install.sh sets up the same group.
    let fix = if std::path::Path::new("/usr/bin/pacman").exists() {
        "not real-time: install realtime-privileges, join the realtime group (usermod -aG realtime), log in again"
    } else {
        "not real-time: run Patch's installer again (it sets up the realtime group), log in again"
    };
    out.push(warn("scheduling", if rt { st["sched"].as_str().unwrap_or("real-time").to_string() } else { fix.into() }, !rt));
    out.push(warn(
        "ring drops",
        format!("{}", st["dropped"].as_u64().unwrap_or(0)),
        st["dropped"].as_u64().unwrap_or(0) > 0,
    ));
    out.push(row(
        "output",
        format!(
            "{} {}",
            st["output"].as_str().unwrap_or("—"),
            if st["linked"].as_bool().unwrap_or(false) {
                "· linked"
            } else {
                "· NOT linked"
            }
        ),
    ));
    out.push(row(
        "headphones",
        st["cue_output"].as_str().unwrap_or("off").to_string(),
    ));

    out.push(heading("Clock"));
    let c = i.clock;
    if let (Some(pos), Some(ns)) = (c["position"].as_u64(), c["nsec"].as_u64()) {
        let age_ms = (c["now_ns"].as_u64().unwrap_or(ns).saturating_sub(ns)) as f64 / 1e6;
        out.push(row(
            "graph position",
            format!("{pos} samples ({:.3} s)", pos as f64 / rate.max(1) as f64),
        ));
        out.push(row(
            "last cycle",
            format!("{:.3} ms ago (CLOCK_MONOTONIC {ns} ns)", age_ms),
        ));
        if let Some(lat) = i.latency_ms {
            out.push(row(
                "presentation",
                format!(
                    "{} ns (+{lat:.1} ms, {})",
                    ns + (lat as f64 * 1e6) as u64,
                    i.latency_level
                ),
            ));
        }
    } else {
        out.push(warn("graph", "no cycles yet", true));
    }
    out.push(row("meter reference", i.reference.to_string()));
    out.push(row(
        "output latency",
        match i.latency_ms {
            Some(l) => format!("{l:.1} ms {} · trim {:+.0} ms", i.latency_level, i.trim_ms),
            None => "—".into(),
        },
    ));

    for (name, d) in i.decks {
        out.push(heading(&format!("Deck {}", name.to_uppercase())));
        if !d["loaded"].as_bool().unwrap_or(false) {
            out.push(row("engine deck", "empty"));
            continue;
        }
        let pos = f(&d["position"]);
        out.push(row(
            "source",
            format!(
                "{} {} Hz / {}-bit → {} Hz",
                d["codec"].as_str().unwrap_or("").to_uppercase(),
                d["native_rate"],
                d["bits"],
                rate
            ),
        ));
        out.push(row(
            "position",
            format!(
                "{:.4} s · {} samples",
                pos,
                (pos * rate as f64).round() as i64
            ),
        ));
        out.push(row(
            "state",
            format!(
                "{} · step {:.4} · tempo {:+.2} % · bend {:+.3} % · {}",
                if d["playing"].as_bool().unwrap_or(false) {
                    "playing"
                } else if d["end"].as_bool().unwrap_or(false) {
                    "end of track"
                } else {
                    "stopped"
                },
                f(&d["step"]),
                f(&d["tempo"]) * 100.0,
                f(&d["bend"]) * 100.0,
                if d["keylock"].as_bool().unwrap_or(true) {
                    "keylock"
                } else {
                    "varispeed"
                },
            ),
        ));
        if d["loop_on"].as_bool().unwrap_or(false) {
            out.push(row(
                "loop",
                format!("{:.4} … {:.4} s", f(&d["loop_in"]), f(&d["loop_out"])),
            ));
        }
        let sy = &d["sync"];
        if sy["on"].as_bool().unwrap_or(false) {
            out.push(warn(
                "sync",
                format!(
                    "to {} · phase error {:+.3} ms{}",
                    sy["master"].as_str().unwrap_or("auto"),
                    f(&sy["error_ms"]),
                    if sy["phase_hold"].as_bool().unwrap_or(false) {
                        " · HELD (nudged)"
                    } else {
                        ""
                    }
                ),
                f(&sy["error_ms"]).abs() > 1.0,
            ));
        }
        let und = d["underruns"].as_u64().unwrap_or(0);
        out.push(warn("underruns", format!("{und}"), und > 0));
        let dec = f(&d["decoded"]);
        if dec < 1.0 {
            out.push(row("decoding", format!("{:.0} %", dec * 100.0)));
        }
    }

    out.push(heading("Workers"));
    out.push(row("analysis queue", format!("{} files", i.analysis_queue)));
    out.push(row(
        "engine CPU",
        i.engine_cpu
            .map(|c| format!("{c:.1} %"))
            .unwrap_or_else(|| "—".into()),
    ));
    out.push(row(
        "UI CPU",
        i.ui_cpu
            .map(|c| format!("{c:.1} %"))
            .unwrap_or_else(|| "—".into()),
    ));
    out.push(row(
        "UI worst frame",
        format!("{:.2} ms (budget 16.7 ms)", i.ui_frame_ms),
    ));
    out
}

/// Engine requests needed for a snapshot.
pub fn requests() -> [Value; 2] {
    [json!({"op": "status"}), json!({"op": "clock"})]
}
