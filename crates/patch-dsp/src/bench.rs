//! The benchmark harness: ns per frame for every implementation at the
//! period sizes live audio uses (16 … 4096 frames), after warm-up, over
//! repeated runs, reported as median and p95. Pins the thread to one CPU
//! when the OS allows it and reports the environment (CPU, governor,
//! build). Never changes system settings; says what it couldn't do.

use crate::abi::{MixRamp, Op};
use crate::registry::Impl;
use std::time::Instant;

pub const SIZES: [usize; 9] = [16, 32, 64, 128, 256, 512, 1024, 2048, 4096];

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Point {
    pub frames: usize,
    /// Nanoseconds per frame (all channels), median and 95th percentile.
    pub median_ns: f64,
    pub p95_ns: f64,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Result_ {
    pub op: Op,
    pub name: String,
    pub points: Vec<Point>,
}

impl Result_ {
    /// Geometric mean of median ns/frame over the live-audio sizes
    /// (32 … 1024): the figure selection compares.
    pub fn score(&self) -> f64 {
        let v: Vec<f64> = self
            .points
            .iter()
            .filter(|p| (32..=1024).contains(&p.frames))
            .map(|p| p.median_ns.max(1e-6))
            .collect();
        (v.iter().map(|x| x.ln()).sum::<f64>() / v.len().max(1) as f64).exp()
    }
}

/// The environment a benchmark ran in.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Env {
    pub cpu: String,
    pub pinned: bool,
    pub governor: String,
    pub build: String,
}

/// Pin this thread to the CPU it is on (no privileges needed); false when
/// the OS refuses.
pub fn pin() -> bool {
    #[cfg(target_os = "linux")]
    unsafe {
        let cpu = libc::sched_getcpu();
        if cpu < 0 {
            return false;
        }
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(cpu as usize, &mut set);
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) == 0
    }
    #[cfg(not(target_os = "linux"))]
    false
}

pub fn env(pinned: bool) -> Env {
    let governor = std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".into());
    let build = if cfg!(debug_assertions) {
        "debug (numbers not meaningful)".into()
    } else {
        "release".into()
    };
    Env {
        cpu: crate::cpu::this().model,
        pinned,
        governor,
        build,
    }
}

/// Measure one implementation at every size (stereo; `Peak`/`MixRamp`
/// per channel, `Interleave2` over the pair).
pub fn measure(imp: &Impl, rounds: usize) -> Result_ {
    let channels = imp.op.channels().unwrap_or(2) as usize;
    let mut points = vec![];
    for &frames in &SIZES {
        let ins: Vec<Vec<f32>> = (0..channels)
            .map(|c| {
                (0..frames)
                    .map(|i| ((i + c) as f32 * 0.01).sin() * 0.5)
                    .collect()
            })
            .collect();
        let mut outs: Vec<Vec<f32>> = (0..imp.op.out_channels(channels))
            .map(|_| vec![0.0f32; imp.op.out_len(frames)])
            .collect();
        let params = MixRamp { g0: 0.9, dg: 1e-5 };
        let ip: Vec<*const f32> = ins.iter().map(|v| v.as_ptr()).collect();
        let op_: Vec<*mut f32> = outs.iter_mut().map(|v| v.as_mut_ptr()).collect();
        // Enough calls per sample that timer resolution doesn't matter.
        let calls = (200_000 / frames).max(20);
        let run = || {
            let t0 = Instant::now();
            for _ in 0..calls {
                // SAFETY: buffers sized per the op's contract.
                unsafe {
                    (imp.f)(
                        ip.as_ptr(),
                        op_.as_ptr(),
                        frames as u32,
                        channels as u32,
                        &params as *const _ as *const _,
                        std::ptr::null_mut(),
                    )
                };
            }
            std::hint::black_box(&op_);
            t0.elapsed().as_nanos() as f64 / (calls * frames) as f64
        };
        for _ in 0..3 {
            run(); // warm-up: caches, frequency
        }
        let mut samples: Vec<f64> = (0..rounds).map(|_| run()).collect();
        samples.sort_by(f64::total_cmp);
        let median = samples[samples.len() / 2];
        let p95 = samples[((samples.len() as f64 * 0.95) as usize).min(samples.len() - 1)];
        points.push(Point {
            frames,
            median_ns: median,
            p95_ns: p95,
        });
    }
    Result_ {
        op: imp.op,
        name: imp.name.into(),
        points,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmarks_are_sane() {
        for imp in crate::registry::all().into_iter().filter(|i| i.supported()) {
            let r = measure(&imp, 3);
            assert_eq!(r.points.len(), SIZES.len());
            for p in &r.points {
                assert!(
                    p.median_ns > 0.0 && p.median_ns.is_finite() && p.p95_ns >= p.median_ns,
                    "{} {}: {p:?}",
                    imp.op.name(),
                    imp.name
                );
            }
            assert!(r.score() > 0.0);
        }
    }
}
