//! Meter-to-ear synchronisation.
//!
//! Meter taps read audio *before* it reaches your ears: after a tap come the
//! Patch loopback, graph buffering and the output device (a Bluetooth link
//! adds codec and radio latency, often 150–250 ms). To show exactly what you
//! hear, every metered point is displayed late by its own path latency:
//!
//! ```text
//!   output monitor   D + Q − T
//!   Patch bus        L + Q + D + Q − T
//!   routed stream    as the bus it feeds (or D + Q − T when direct)
//! ```
//!
//! D is the device latency PipeWire reports on the sink (`Latency` param,
//! re-read every second because Bluetooth latency drifts), Q one graph quantum,
//! L the loopback buffer, T the tap's own capture buffering. A per-device trim
//! (ms) covers anything PipeWire can't see and is saved to
//! `~/.config/omarchy-patch/sync`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Loopback buffer configured on the Patch bus (`latency_msec`).
pub const LOOPBACK_S: f64 = 0.020;
/// Capture buffering of each meter tap (`parec --latency-msec`).
pub const TAP_S: f64 = 0.020;

/// Device latency (seconds) per sink node name, kept fresh by a thread.
#[derive(Clone, Default)]
pub struct LatencyWatch {
    devices: Arc<Mutex<HashMap<String, DeviceLatency>>>,
    /// Measured engine-to-ear latency (seconds) per sink, from the
    /// Analyzer's acoustic loop (`~/.config/omarchy-patch/latency.json`).
    measured: Arc<Mutex<HashMap<String, f64>>>,
}

impl LatencyWatch {
    pub fn start() -> Self {
        let w = LatencyWatch::default();
        let d = w.devices.clone();
        let m = w.measured.clone();
        let _ = std::thread::Builder::new()
            .name("patch-latency".into())
            .spawn(move || {
                // One long-lived `pw-dump -m`: every change in the graph as
                // it happens (a Bluetooth output's drifting latency too), on
                // one connection. It read the whole graph once a second
                // while a Bluetooth output was there (every 10 s else): a
                // connection PipeWire set up and tore down each time.
                *m.lock().unwrap() = measured_now();
                loop {
                    watch_latencies(&d, &m);
                    std::thread::sleep(Duration::from_secs(1));
                }
            });
        w
    }

    /// Measured engine-to-ear latency of a sink, seconds.
    pub fn measured(&self, sink: &str) -> Option<f64> {
        self.measured.lock().unwrap().get(sink).copied()
    }

    /// What a sink reports beyond its buffer (seconds): frames and ns,
    /// such as a Bluetooth codec's delay. Its quanta are the buffer the
    /// driver's tracked delay already holds.
    pub fn beyond_buffer(&self, sink: &str, rate: u32) -> f64 {
        self.devices
            .lock()
            .unwrap()
            .get(sink)
            .map(|d| d.seconds(0, rate))
            .unwrap_or(0.0)
    }

    /// Reported device latency of a sink (seconds) at the graph's current
    /// quantum and rate.
    pub fn device(&self, sink: &str, quantum: u32, rate: u32) -> f64 {
        self.devices
            .lock()
            .unwrap()
            .get(sink)
            .map(|d| d.seconds(quantum, rate))
            .unwrap_or(0.0)
    }
}

/// A sink's `Latency` param (Input direction). PipeWire expresses it in
/// three parts that add up: graph quanta, frames and nanoseconds. An ALSA
/// sink typically reports one quantum and 0 ns; Bluetooth adds ns.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DeviceLatency {
    pub quanta: f64,
    pub frames: f64,
    pub ns: f64,
}

impl DeviceLatency {
    pub fn seconds(&self, quantum: u32, rate: u32) -> f64 {
        let rate = rate.max(1) as f64;
        self.quanta * quantum as f64 / rate + self.frames / rate + self.ns / 1e9
    }
}

fn measured_now() -> HashMap<String, f64> {
    patch_engine::latency::load_all()
        .into_iter()
        .map(|(k, v)| (k, v.output_ms / 1000.0))
        .collect()
}

/// The `Latency` param (Input direction) of a node object, if it has one.
fn node_latency(info: &serde_json::Value) -> Option<DeviceLatency> {
    let list = info["params"]["Latency"].as_array()?;
    Some(
        list.iter()
            .filter(|l| l["direction"].as_str() == Some("Input"))
            .map(|l| DeviceLatency {
                quanta: l["minQuantum"].as_f64().unwrap_or(0.0),
                frames: l["minRate"].as_f64().unwrap_or(0.0),
                ns: l["minNs"].as_f64().unwrap_or(0.0),
            })
            .fold(DeviceLatency::default(), |a, b| DeviceLatency {
                quanta: a.quanta.max(b.quanta),
                frames: a.frames.max(b.frames),
                ns: a.ns.max(b.ns),
            }),
    )
}

/// Follow `pw-dump -m` until it ends: every Audio/Sink's latency, by node
/// name, kept in `d`; the measured latencies re-read now and then.
fn watch_latencies(
    d: &Arc<Mutex<HashMap<String, DeviceLatency>>>,
    m: &Arc<Mutex<HashMap<String, f64>>>,
) {
    let mut cmd = Command::new("pw-dump");
    cmd.args(["-m", "-N"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: only an async-signal-safe call between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
    }
    let Ok(mut child) = cmd.spawn() else { return };
    let Some(out) = child.stdout.take() else {
        return;
    };
    // Node id → name (updates name the object by id only once it's known).
    let mut names: HashMap<u64, String> = HashMap::new();
    let mut read_at = std::time::Instant::now();
    for batch in serde_json::Deserializer::from_reader(std::io::BufReader::new(out))
        .into_iter::<serde_json::Value>()
    {
        let Ok(batch) = batch else { break };
        let mut map = d.lock().unwrap();
        for o in batch.as_array().into_iter().flatten() {
            let Some(id) = o["id"].as_u64() else { continue };
            let info = &o["info"];
            if info.is_null() {
                if let Some(name) = names.remove(&id) {
                    map.remove(&name);
                }
                continue;
            }
            let props = &info["props"];
            if let Some(name) = props["node.name"].as_str() {
                if props["media.class"].as_str() != Some("Audio/Sink") {
                    continue;
                }
                names.insert(id, name.to_string());
            }
            if let (Some(name), Some(lat)) = (names.get(&id), node_latency(info)) {
                map.insert(name.clone(), lat);
            }
        }
        if std::env::var_os("PATCH_TRACE_LATENCY").is_some() {
            for (k, v) in map.iter() {
                eprintln!(
                    "latency {k}: {:.1} ms (quanta {}, frames {}, ns {})",
                    v.seconds(1024, 48_000) * 1e3,
                    v.quanta,
                    v.frames,
                    v.ns
                );
            }
        }
        drop(map);
        if read_at.elapsed() >= Duration::from_secs(10) {
            *m.lock().unwrap() = measured_now();
            read_at = std::time::Instant::now();
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// `Latency` param (Input direction: quanta, frames, ns) of every Audio/Sink node.
#[allow(dead_code)]
fn read_device_latencies() -> Option<HashMap<String, DeviceLatency>> {
    let out = Command::new("pw-dump")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let mut map = HashMap::new();
    for o in v.as_array()? {
        let info = &o["info"];
        let props = &info["props"];
        if props["media.class"].as_str() != Some("Audio/Sink") {
            continue;
        }
        let Some(name) = props["node.name"].as_str() else {
            continue;
        };
        let lat = info["params"]["Latency"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|l| l["direction"].as_str() == Some("Input"))
            .map(|l| DeviceLatency {
                quanta: l["minQuantum"].as_f64().unwrap_or(0.0),
                frames: l["minRate"].as_f64().unwrap_or(0.0),
                ns: l["minNs"].as_f64().unwrap_or(0.0),
            })
            .fold(DeviceLatency::default(), |a, b| DeviceLatency {
                quanta: a.quanta.max(b.quanta),
                frames: a.frames.max(b.frames),
                ns: a.ns.max(b.ns),
            });
        map.insert(name.to_string(), lat);
    }
    Some(map)
}

fn trim_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("omarchy-patch/sync"))
}

/// Per-device trims in milliseconds (`<sink name> = <ms>` lines).
pub fn load_trims() -> HashMap<String, f64> {
    let Some(text) = trim_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return HashMap::new();
    };
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            Some((k.trim().to_string(), v.trim().parse::<f64>().ok()?))
        })
        .collect()
}

pub fn save_trims(trims: &HashMap<String, f64>) -> std::io::Result<()> {
    let Some(p) = trim_path() else { return Ok(()) };
    std::fs::create_dir_all(p.parent().unwrap())?;
    let mut lines: Vec<String> = trims.iter().map(|(k, v)| format!("{k} = {v}")).collect();
    lines.sort();
    std::fs::write(p, lines.join("\n") + "\n")
}

#[cfg(test)]
mod device_latency_tests {
    use super::DeviceLatency;

    #[test]
    fn an_alsa_sink_reports_one_quantum() {
        // The analog output here: minQuantum 1, minNs 0, at 1024 / 48 kHz.
        let d = DeviceLatency {
            quanta: 1.0,
            frames: 0.0,
            ns: 0.0,
        };
        assert!((d.seconds(1024, 48_000) - 0.021333).abs() < 1e-5);
        // Bluetooth: nanoseconds on top.
        let bt = DeviceLatency {
            quanta: 0.0,
            frames: 0.0,
            ns: 150e6,
        };
        assert!((bt.seconds(1024, 48_000) - 0.150).abs() < 1e-9);
    }
}
