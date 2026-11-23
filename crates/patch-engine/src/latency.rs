//! Measured output latency: an acoustic loop through the Analyzer.
//!
//! The engine plays a few log sweeps on the program output and records a
//! microphone on its `in_measure` port in the same graph cycles, so probe
//! sample `k` and microphone sample `k` share one graph position. The lag
//! of the sweep in the recording is then the exact round trip in samples:
//!
//! ```text
//!   engine → output device → air → microphone → engine
//! ```
//!
//! Output latency (engine block to your ear) = round trip − the
//! microphone's own capture latency (reported by PipeWire; small and
//! stable on USB and built-in microphones). Each sweep gives its own
//! estimate; a result needs at least three that agree and a clear
//! correlation peak, otherwise the measurement fails and says why — no
//! number is ever invented.
//!
//! The RT side only copies samples into a preallocated buffer; the
//! analysis (FFT cross-correlation) runs on a control thread.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering};

/// Sweeps played per measurement.
pub const REPS: usize = 4;
/// One sweep, seconds; the band suits Bluetooth codecs and small mics.
const SWEEP_S: f64 = 0.2;
const F0: f64 = 200.0;
const F1: f64 = 6000.0;
/// Probe level (−12 dBFS).
const LEVEL: f32 = 0.25;
/// Sweep spacing: also the longest latency a sweep can be matched to.
pub const SPACING_S: f64 = 1.0;
/// Silence before the first sweep (the microphone settles).
const LEAD_S: f64 = 0.25;
/// Cycles to wait after arming (the microphone link and device wake up).
const SETTLE_S: f64 = 0.6;

pub const IDLE: u8 = 0;
pub const ARMED: u8 = 1;
pub const RUNNING: u8 = 2;
pub const DONE: u8 = 3;

/// Geometry of one measurement at a sample rate.
#[derive(Clone, Copy, Debug)]
pub struct Plan {
    pub rate: u32,
    pub sweep: usize,
    pub lead: usize,
    pub spacing: usize,
    /// Samples recorded: every sweep plus a full spacing after the last.
    pub len: usize,
}

impl Plan {
    pub fn new(rate: u32) -> Plan {
        let r = rate.max(8000) as f64;
        let (sweep, lead, spacing) = (
            (SWEEP_S * r) as usize,
            (LEAD_S * r) as usize,
            (SPACING_S * r) as usize,
        );
        Plan {
            rate,
            sweep,
            lead,
            spacing,
            len: lead + REPS * spacing,
        }
    }

    pub fn start(&self, rep: usize) -> usize {
        self.lead + rep * self.spacing
    }
}

/// The probe sweep (exponential, raised-cosine edges) at a rate.
pub fn sweep(rate: u32) -> Vec<f32> {
    let p = Plan::new(rate);
    let n = p.sweep;
    let r = rate as f64;
    let t_len = n as f64 / r;
    let k = (F1 / F0).ln();
    let edge = (0.01 * r) as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / r;
            let phase = 2.0 * std::f64::consts::PI * F0 * t_len / k * ((t / t_len * k).exp() - 1.0);
            let w = if i < edge {
                0.5 - 0.5 * (std::f64::consts::PI * i as f64 / edge as f64).cos()
            } else if i + edge > n {
                0.5 - 0.5 * (std::f64::consts::PI * (n - i) as f64 / edge as f64).cos()
            } else {
                1.0
            };
            (phase.sin() * w) as f32 * LEVEL
        })
        .collect()
}

/// What the RT thread and the control thread share for one measurement.
/// The recording is written only by the RT thread while RUNNING and read
/// only by the control thread once DONE (Release/Acquire on `state`).
pub struct Shared {
    pub state: AtomicU8,
    /// Rate the probe was built for; the RT thread refuses another rate.
    pub rate: AtomicU32,
    settle: AtomicU32,
    written: AtomicUsize,
    probe: UnsafeCell<Box<[f32]>>,
    rec: UnsafeCell<Box<[f32]>>,
}

// SAFETY: access to the cells is handed over through `state` (see above).
unsafe impl Sync for Shared {}

impl Default for Shared {
    fn default() -> Self {
        Shared {
            state: AtomicU8::new(IDLE),
            rate: AtomicU32::new(0),
            settle: AtomicU32::new(0),
            written: AtomicUsize::new(0),
            probe: UnsafeCell::new(Box::new([])),
            rec: UnsafeCell::new(Box::new([])),
        }
    }
}

impl Shared {
    /// Prepare buffers for `rate` and arm: the RT thread starts after the
    /// settle time. Control thread only, while IDLE or DONE.
    pub fn arm(&self, rate: u32) -> Result<(), String> {
        let s = self.state.load(Ordering::Acquire);
        if s == ARMED || s == RUNNING {
            return Err("a measurement is running".into());
        }
        let plan = Plan::new(rate);
        let one = sweep(rate);
        let mut probe = vec![0.0f32; plan.len];
        for r in 0..REPS {
            let at = plan.start(r);
            probe[at..at + one.len()].copy_from_slice(&one);
        }
        // SAFETY: IDLE/DONE: the RT thread does not touch the cells.
        unsafe {
            *self.probe.get() = probe.into_boxed_slice();
            *self.rec.get() = vec![0.0f32; plan.len].into_boxed_slice();
        }
        self.rate.store(rate, Ordering::Relaxed);
        self.written.store(0, Ordering::Relaxed);
        self.settle
            .store((SETTLE_S * rate as f64) as u32, Ordering::Relaxed);
        self.state.store(ARMED, Ordering::Release);
        Ok(())
    }

    pub fn cancel(&self) {
        self.state.store(IDLE, Ordering::Release);
    }

    /// The recording, once DONE.
    pub fn recording(&self) -> Option<&[f32]> {
        // SAFETY: DONE: the RT thread no longer writes.
        (self.state.load(Ordering::Acquire) == DONE).then(|| unsafe { &**self.rec.get() })
    }

    /// RT: during a measurement, replace the program with the probe and
    /// record the microphone. Returns true while the program is replaced.
    pub fn process(
        &self,
        rate: u32,
        mic: Option<&[f32]>,
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) -> bool {
        match self.state.load(Ordering::Acquire) {
            ARMED => {
                if rate != self.rate.load(Ordering::Relaxed) {
                    self.state.store(IDLE, Ordering::Release);
                    return false;
                }
                // Program already silent while the microphone settles.
                out_l.fill(0.0);
                out_r.fill(0.0);
                let left = self.settle.load(Ordering::Relaxed);
                let n = out_l.len() as u32;
                if left > n {
                    self.settle.store(left - n, Ordering::Relaxed);
                } else {
                    self.state.store(RUNNING, Ordering::Release);
                }
                true
            }
            RUNNING => {
                // SAFETY: RUNNING: only this thread touches the cells.
                let (probe, rec) = unsafe { (&**self.probe.get(), &mut **self.rec.get()) };
                let at = self.written.load(Ordering::Relaxed);
                let n = out_l.len().min(out_r.len()).min(rec.len() - at);
                out_l[..n].copy_from_slice(&probe[at..at + n]);
                out_r[..n].copy_from_slice(&probe[at..at + n]);
                out_l[n..].fill(0.0);
                out_r[n..].fill(0.0);
                match mic {
                    Some(m) => {
                        let k = n.min(m.len());
                        rec[at..at + k].copy_from_slice(&m[..k]);
                    }
                    None => rec[at..at + n].fill(0.0),
                }
                self.written.store(at + n, Ordering::Relaxed);
                if at + n >= rec.len() {
                    self.state.store(DONE, Ordering::Release);
                }
                true
            }
            _ => false,
        }
    }
}

/// The result of one measurement.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Measured {
    /// Round trip per sweep, ms (None: that sweep wasn't found).
    pub sweeps_ms: Vec<Option<f64>>,
    /// Median round trip of the agreeing sweeps, ms.
    pub round_trip_ms: f64,
    /// The microphone's reported capture latency, ms.
    pub input_ms: f64,
    /// Engine block to ear: round trip − input.
    pub output_ms: f64,
    /// Spread of the agreeing sweeps, ms.
    pub spread_ms: f64,
    /// Correlation peak over its window's median (higher is clearer).
    pub clarity: f64,
}

/// Find the probe in the recording. `input_ms`: the microphone's capture
/// latency to subtract.
pub fn analyse(rec: &[f32], rate: u32, input_ms: f64) -> Result<Measured, String> {
    let plan = Plan::new(rate);
    if rec.len() < plan.len {
        return Err("recording too short".into());
    }
    let peak = rec.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    if peak < 1e-4 {
        return Err("the microphone heard nothing (is it muted, or the output too quiet?)".into());
    }
    let tpl = sweep(rate);
    let corr = xcorr(rec, &tpl);
    let ms = |s: f64| s * 1000.0 / rate as f64;
    let mut sweeps = vec![];
    let mut clarity = f64::MAX;
    for r in 0..REPS {
        let from = plan.start(r);
        let to = (from + plan.spacing).min(corr.len());
        let w = &corr[from..to];
        let (i, p) =
            w.iter().enumerate().fold(
                (0, 0.0f64),
                |b, (i, v)| if v.abs() > b.1 { (i, v.abs()) } else { b },
            );
        let mut mags: Vec<f64> = w.iter().map(|v| v.abs()).collect();
        mags.sort_by(f64::total_cmp);
        let median = mags[mags.len() / 2].max(1e-12);
        let c = p / median;
        if c < MIN_CLARITY {
            sweeps.push(None);
            continue;
        }
        clarity = clarity.min(c);
        // Sub-sample peak (parabola through the neighbours).
        let at = from + i;
        let frac = if at > 0 && at + 1 < corr.len() {
            let (a, b, c) = (corr[at - 1].abs(), corr[at].abs(), corr[at + 1].abs());
            let d = a - 2.0 * b + c;
            if d.abs() > 1e-12 {
                (0.5 * (a - c) / d).clamp(-0.5, 0.5)
            } else {
                0.0
            }
        } else {
            0.0
        };
        sweeps.push(Some(ms(i as f64 + frac)));
    }
    let found: Vec<f64> = sweeps.iter().flatten().copied().collect();
    if found.len() < 3 {
        return Err(format!(
            "found {} of {REPS} sweeps: hold the headphones or speaker close to the microphone and raise the volume",
            found.len()
        ));
    }
    let mut sorted = found.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    let agree: Vec<f64> = found
        .iter()
        .copied()
        .filter(|v| (v - median).abs() <= AGREE_MS)
        .collect();
    if agree.len() < 3 {
        return Err(format!(
            "the sweeps disagree ({}): too much noise or echo",
            found
                .iter()
                .map(|v| format!("{v:.1}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let lo = agree.iter().copied().fold(f64::MAX, f64::min);
    let hi = agree.iter().copied().fold(f64::MIN, f64::max);
    let mut a = agree.clone();
    a.sort_by(f64::total_cmp);
    let rt = a[a.len() / 2];
    Ok(Measured {
        sweeps_ms: sweeps,
        round_trip_ms: rt,
        input_ms,
        output_ms: (rt - input_ms).max(0.0),
        spread_ms: hi - lo,
        clarity,
    })
}

/// Measure now (control thread): link the microphone, wait for the
/// armed measurement to finish, unlink, analyse and save for `output`.
pub fn run(params: &crate::Params, mic: &str, output: &str, rate: u32) -> Result<Measured, String> {
    let port = mic_port(mic).ok_or_else(|| format!("{mic} has no capture port"))?;
    let _ = pw(&["pw-link", &port, MEASURE_PORT]);
    let s = &params.latency;
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs_f64(Plan::new(rate).len as f64 / rate as f64 + 4.0);
    while s.state.load(Ordering::Acquire) != DONE {
        if std::time::Instant::now() > deadline || s.state.load(Ordering::Acquire) == IDLE {
            s.cancel();
            let _ = pw(&["pw-link", "-d", &port, MEASURE_PORT]);
            return Err("the measurement didn't run (graph stopped or rate changed)".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = pw(&["pw-link", "-d", &port, MEASURE_PORT]);
    let quantum = params.last_frames.load(Ordering::Relaxed);
    let input_ms = capture_latency_ms(mic, quantum, rate).unwrap_or(0.0);
    let r = analyse(s.recording().unwrap_or(&[]), rate, input_ms);
    s.cancel();
    let m = r?;
    save(output, mic, &m)?;
    Ok(m)
}

const MEASURE_PORT: &str = "patch.engine:in_measure";

fn pw(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(args[0])
        .args(&args[1..])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A microphone that hears the room: the default source unless that is a
/// monitor (never through the air), Patch's own, or Bluetooth (opening a
/// headset microphone drops the headphones to call quality); else the
/// first hardware input.
pub fn choose_mic() -> Result<String, String> {
    let usable =
        |s: &str| !s.ends_with(".monitor") && !s.starts_with("bluez_") && !s.starts_with("patch");
    if let Some(d) = pw(&["pactl", "get-default-source"])
        .map(|s| s.trim().to_string())
        .filter(|s| usable(s))
    {
        return Ok(d);
    }
    pw(&["pactl", "list", "short", "sources"])
        .and_then(|t| {
            t.lines()
                .filter_map(|l| l.split('\t').nth(1))
                .find(|s| usable(s) && s.starts_with("alsa_input"))
                .map(String::from)
        })
        .ok_or_else(|| {
            "no microphone to measure with (Bluetooth headset microphones are skipped)".into()
        })
}

/// The first capture port of a source node (mono, or the left side).
fn mic_port(mic: &str) -> Option<String> {
    let prefix = format!("{mic}:");
    let ports: Vec<String> = pw(&["pw-link", "-o"])?
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with(&prefix))
        .map(String::from)
        .collect();
    ports
        .iter()
        .find(|p| p.ends_with("MONO") || p.ends_with("_FL"))
        .or(ports.first())
        .cloned()
}

/// The source's reported capture latency (PipeWire `Latency`, Output) at
/// the graph's `quantum` and `rate`.
fn capture_latency_ms(mic: &str, quantum: u32, rate: u32) -> Option<f64> {
    let v: serde_json::Value = serde_json::from_str(&pw(&["pw-dump"])?).ok()?;
    let node = v
        .as_array()?
        .iter()
        .find(|o| o["info"]["props"]["node.name"].as_str() == Some(mic))?;
    reported_ms(&node["info"]["params"]["Latency"], quantum, rate)
}

/// A `Latency` param list's Output direction in ms. PipeWire states it in
/// three parts that add up: graph quanta, frames and nanoseconds. An ALSA
/// source reports one quantum and 0 ns: reading the nanoseconds alone took
/// a USB microphone's capture for none, and the whole round trip went to
/// the output (37 ms measured where the card holds about 9).
fn reported_ms(latency: &serde_json::Value, quantum: u32, rate: u32) -> Option<f64> {
    let rate = rate.max(1) as f64;
    latency
        .as_array()?
        .iter()
        .filter(|l| l["direction"].as_str() == Some("Output"))
        .map(|l| {
            let part = |k: &str| l[k].as_f64().unwrap_or(0.0);
            (part("minQuantum") * quantum as f64 / rate + part("minRate") / rate) * 1000.0
                + part("minNs") / 1e6
        })
        .reduce(f64::max)
}

/// A saved measurement for one output device.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Saved {
    pub output_ms: f64,
    pub round_trip_ms: f64,
    pub input_ms: f64,
    pub spread_ms: f64,
    pub mic: String,
    /// Unix seconds.
    pub at: u64,
    /// The format: measurements from before `SAVED_VERSION` counted the
    /// microphone's capture as output latency and are not used.
    #[serde(default)]
    pub version: u32,
}

/// Measurements saved with the microphone's capture subtracted in full.
pub const SAVED_VERSION: u32 = 2;

pub fn store_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    base.join("omarchy-patch").join("latency.json")
}

/// Every output's saved measurement (output device node name → result).
pub fn load_all() -> std::collections::BTreeMap<String, Saved> {
    std::fs::read_to_string(store_path())
        .ok()
        .and_then(|t| serde_json::from_str::<std::collections::BTreeMap<String, Saved>>(&t).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, m)| m.version >= SAVED_VERSION)
        .collect()
}

fn write_all(all: &std::collections::BTreeMap<String, Saved>) -> Result<(), String> {
    let path = store_path();
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(all).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(tmp, path).map_err(|e| e.to_string())
}

fn save(output: &str, mic: &str, m: &Measured) -> Result<(), String> {
    let mut all = load_all();
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    all.insert(
        output.to_string(),
        Saved {
            output_ms: m.output_ms,
            round_trip_ms: m.round_trip_ms,
            input_ms: m.input_ms,
            spread_ms: m.spread_ms,
            mic: mic.to_string(),
            at,
            version: SAVED_VERSION,
        },
    );
    write_all(&all)
}

pub fn forget(output: &str) -> Result<(), String> {
    let mut all = load_all();
    all.remove(output);
    write_all(&all)
}

/// A sweep counts when its peak stands this far above its window's median.
const MIN_CLARITY: f64 = 8.0;
/// Sweeps agree within this much (ms).
const AGREE_MS: f64 = 2.0;

/// Cross-correlation r[m] = Σ rec[m + k] · tpl[k] for m in 0..rec.len().
pub fn xcorr(rec: &[f32], tpl: &[f32]) -> Vec<f64> {
    let n = (rec.len() + tpl.len()).next_power_of_two();
    let mut a: Vec<(f64, f64)> = rec
        .iter()
        .map(|v| (*v as f64, 0.0))
        .chain(std::iter::repeat((0.0, 0.0)))
        .take(n)
        .collect();
    let mut b: Vec<(f64, f64)> = tpl
        .iter()
        .map(|v| (*v as f64, 0.0))
        .chain(std::iter::repeat((0.0, 0.0)))
        .take(n)
        .collect();
    fft(&mut a, false);
    fft(&mut b, false);
    // A · conj(B)
    for (x, y) in a.iter_mut().zip(&b) {
        *x = (x.0 * y.0 + x.1 * y.1, x.1 * y.0 - x.0 * y.1);
    }
    fft(&mut a, true);
    a.truncate(rec.len());
    a.into_iter().map(|v| v.0 / n as f64).collect()
}

/// In-place iterative radix-2 FFT (unnormalised); `inverse` flips the sign.
pub fn fft(x: &mut [(f64, f64)], inverse: bool) {
    let n = x.len();
    assert!(n.is_power_of_two());
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            x.swap(i, j);
        }
    }
    let sign = if inverse { 1.0 } else { -1.0 };
    let mut len = 2;
    while len <= n {
        let ang = sign * 2.0 * std::f64::consts::PI / len as f64;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0, 0.0);
            for k in 0..len / 2 {
                let (ar, ai) = x[start + k];
                let (br, bi) = x[start + k + len / 2];
                let (tr, ti) = (br * cr - bi * ci, br * ci + bi * cr);
                x[start + k] = (ar + tr, ai + ti);
                x[start + k + len / 2] = (ar - tr, ai - ti);
                (cr, ci) = (cr * wr - ci * wi, cr * wi + ci * wr);
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// What a microphone hears: the probe `delay` samples late, quieter,
    /// low-passed (a small speaker), with noise and an echo.
    fn heard(delay: usize, gain: f32, noise: f32) -> Vec<f32> {
        let s = Shared::default();
        s.arm(RATE).unwrap();
        let probe = unsafe { (*s.probe.get()).to_vec() };
        let mut rec = vec![0.0f32; probe.len()];
        let mut lp = 0.0f32;
        let mut seed = 0x1234_5678u32;
        for i in 0..rec.len() {
            let src = if i >= delay { probe[i - delay] } else { 0.0 };
            lp += 0.35 * (src - lp);
            let echo = if i >= delay + 700 {
                probe[i - delay - 700] * 0.3
            } else {
                0.0
            };
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let n = (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
            rec[i] = gain * (lp + echo) + noise * n;
        }
        rec
    }

    #[test]
    fn fft_matches_a_plain_dft() {
        let x: Vec<(f64, f64)> = (0..16)
            .map(|i| ((i as f64 * 0.7).sin(), (i as f64 * 0.3).cos()))
            .collect();
        let mut y = x.clone();
        fft(&mut y, false);
        for (k, v) in y.iter().enumerate() {
            let (mut re, mut im) = (0.0, 0.0);
            for (n, s) in x.iter().enumerate() {
                let a = -2.0 * std::f64::consts::PI * (k * n) as f64 / 16.0;
                re += s.0 * a.cos() - s.1 * a.sin();
                im += s.0 * a.sin() + s.1 * a.cos();
            }
            assert!(
                (v.0 - re).abs() < 1e-9 && (v.1 - im).abs() < 1e-9,
                "bin {k}"
            );
        }
    }

    #[test]
    fn finds_a_bluetooth_sized_delay_to_the_sample() {
        // 212.5 ms round trip = 10 200 samples; a 4 ms microphone.
        let rec = heard(10_200, 0.05, 0.01);
        let m = analyse(&rec, RATE, 4.0).unwrap();
        assert!((m.round_trip_ms - 212.5).abs() < 0.05, "{m:?}");
        assert!((m.output_ms - 208.5).abs() < 0.05);
        assert!(m.spread_ms < 0.05);
        assert_eq!(m.sweeps_ms.iter().flatten().count(), REPS);
    }

    #[test]
    fn short_wired_delays_too() {
        let m = analyse(&heard(480, 0.2, 0.002), RATE, 0.0).unwrap();
        assert!((m.round_trip_ms - 10.0).abs() < 0.05, "{m:?}");
    }

    #[test]
    fn silence_and_noise_fail_without_a_number() {
        let plan = Plan::new(RATE);
        assert!(
            analyse(&vec![0.0; plan.len], RATE, 0.0)
                .unwrap_err()
                .contains("heard nothing")
        );
        let only_noise = heard(1000, 0.0, 0.05);
        assert!(analyse(&only_noise, RATE, 0.0).is_err());
    }

    #[test]
    fn rt_side_plays_the_probe_and_records_in_step() {
        let s = Shared::default();
        s.arm(RATE).unwrap();
        let (mut l, mut r) = (vec![1.0f32; 1024], vec![1.0f32; 1024]);
        // Settle: program silenced, nothing recorded.
        while s.state.load(Ordering::Acquire) == ARMED {
            assert!(s.process(RATE, None, &mut l, &mut r));
            assert!(l.iter().all(|v| *v == 0.0));
        }
        // Running: the "microphone" is the output one block late.
        let mut last = vec![0.0f32; 1024];
        let mut played = vec![];
        while s.state.load(Ordering::Acquire) == RUNNING {
            s.process(RATE, Some(&last), &mut l, &mut r);
            played.extend_from_slice(&l);
            last.copy_from_slice(&l);
        }
        let m = analyse(s.recording().unwrap(), RATE, 0.0).unwrap();
        assert!(
            (m.round_trip_ms - 1024.0 * 1000.0 / RATE as f64).abs() < 0.05,
            "one block: {m:?}"
        );
        assert!(
            !s.process(RATE, None, &mut l, &mut r),
            "done: the program is back"
        );
    }

    /// A USB microphone reports its capture as one quantum, no ns (the
    /// Logitech webcam's param, as pw-dump shows it): that cycle is the
    /// input latency, and frames and ns add on top when a device has them.
    #[test]
    fn a_microphone_reporting_quanta_has_a_capture_latency() {
        let webcam = serde_json::json!([
            {"direction": "Input", "minQuantum": 0.0, "minRate": 0, "minNs": 0},
            {"direction": "Output", "minQuantum": 1.0, "minRate": 0, "minNs": 0}
        ]);
        let ms = reported_ms(&webcam, 256, 48_000).unwrap();
        assert!((ms - 256.0 * 1000.0 / 48_000.0).abs() < 1e-9, "{ms}");
        let both = serde_json::json!([
            {"direction": "Output", "minQuantum": 1.0, "minRate": 480, "minNs": 2_000_000}
        ]);
        let ms = reported_ms(&both, 1024, 48_000).unwrap();
        assert!((ms - (1024.0 / 48.0 + 10.0 + 2.0)).abs() < 1e-9, "{ms}");
        assert_eq!(reported_ms(&serde_json::json!(null), 256, 48_000), None);
    }
}
