//! Patch VU metering unit.
//!
//! The hot loops live in hand-written x86-64 assembly (`x86_64.s`):
//!
//! * [`stats`]: per-block sample peak and sum of squares (AVX2 + FMA),
//! * [`Vu::process`]: VU ballistics on every sample (SSE2, double precision).
//!
//! Every kernel has a Rust reference in [`reference`]. The ballistics kernel
//! is bit-identical to it; the stats kernel sums in a different order, so its
//! energy matches to within rounding (tested at 1e-9 relative).
//!
//! Input is interleaved stereo `f32` (L R L R …), as PipeWire delivers it.

pub mod analyzer;
pub mod loudness;
pub mod reference;
pub mod spectrum;

pub use loudness::{Loudness, TruePeak};

#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(include_str!("x86_64.s"), options(raw));

/// Result of [`stats`] over one block.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stats {
    /// Max |x| per channel (linear, 1.0 = 0 dBFS).
    pub peak: [f32; 2],
    /// Σ x² per channel, in double precision.
    pub sum_sq: [f64; 2],
}

impl Stats {
    pub fn peak_dbfs(&self) -> [f32; 2] {
        self.peak.map(|p| {
            if p > 0.0 {
                20.0 * p.log10()
            } else {
                f32::NEG_INFINITY
            }
        })
    }

    /// RMS in dBFS for a block of `frames` frames.
    pub fn rms_dbfs(&self, frames: usize) -> [f64; 2] {
        self.sum_sq.map(|s| {
            let ms = s / frames.max(1) as f64;
            if ms > 0.0 {
                10.0 * ms.log10()
            } else {
                f64::NEG_INFINITY
            }
        })
    }
}

#[cfg(target_arch = "x86_64")]
unsafe extern "sysv64" {
    fn patch_meter_stats_avx2(samples: *const f32, frames: usize, out: *mut Stats);
    fn patch_meter_ballistics_sse2(
        samples: *const f32,
        frames: usize,
        state: *mut [f64; 4],
        coeff: f64,
        max: *mut [f64; 2],
    );
    fn patch_meter_stereo_avx2(samples: *const f32, blocks: usize, out: *mut StereoSums);
}

/// Which implementation the kernels use (see `patch_dsp`).
pub fn backend() -> &'static str {
    patch_dsp::name()
}

pub(crate) fn has_avx2() -> bool {
    patch_dsp::allows(patch_dsp::Tier::Avx2)
}

pub(crate) fn has_sse2() -> bool {
    patch_dsp::allows(patch_dsp::Tier::Sse2)
}

/// Peak and energy of a block of interleaved stereo samples.
/// A trailing odd sample (half a frame) is ignored.
pub fn stats(samples: &[f32]) -> Stats {
    let frames = samples.len() / 2;
    #[cfg(target_arch = "x86_64")]
    if has_avx2() {
        let mut out = Stats::default();
        // SAFETY: the kernel reads exactly `frames * 2` floats from
        // `samples` and writes one `Stats`; AVX2/FMA presence was checked.
        unsafe { patch_meter_stats_avx2(samples.as_ptr(), frames, &mut out) };
        return out;
    }
    reference::stats(&samples[..frames * 2])
}

/// Double-precision sums over interleaved stereo: the basis of phase
/// correlation, balance, M/S width, RMS and DC offset.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StereoSums {
    /// ΣL, ΣR
    pub sum: [f64; 2],
    /// ΣL², ΣR²
    pub sq: [f64; 2],
    /// ΣL·R
    pub lr: f64,
}

impl std::ops::AddAssign for StereoSums {
    fn add_assign(&mut self, o: StereoSums) {
        self.sum[0] += o.sum[0];
        self.sum[1] += o.sum[1];
        self.sq[0] += o.sq[0];
        self.sq[1] += o.sq[1];
        self.lr += o.lr;
    }
}

/// [`StereoSums`] of a block (AVX2+FMA assembly on 4-frame blocks, the
/// reference for the tail; the reference alone without AVX2).
pub fn stereo_sums(samples: &[f32]) -> StereoSums {
    let frames = samples.len() / 2;
    let mut out = StereoSums::default();
    let mut done = 0;
    #[cfg(target_arch = "x86_64")]
    if has_avx2() {
        let blocks = frames / 4;
        // SAFETY: reads blocks·8 floats, writes one StereoSums (5 f64).
        unsafe { patch_meter_stereo_avx2(samples.as_ptr(), blocks, &mut out) };
        done = blocks * 4;
    }
    out += reference::stereo_sums(&samples[done * 2..frames * 2]);
    out
}

/// Level a VU meter reads for a steady sine, relative to its RMS: VU meters
/// are average-responding but calibrated in RMS for sine waves.
pub const SINE_FORM_FACTOR: f64 = std::f64::consts::PI / (2.0 * std::f64::consts::SQRT_2);

/// Default digital alignment (EBU R68): 0 VU for a 1 kHz sine at −18 dBFS.
/// Sine levels in dBFS follow AES17: a full-scale sine is 0 dBFS, so the
/// reference sine peaks at −18 dBFS (its RMS is −21 dB re full scale).
pub const DEFAULT_REFERENCE_DBFS: f64 = -18.0;

/// A stereo VU meter.
#[derive(Debug, Clone)]
pub struct Vu {
    state: [f64; 4],
    coeff: f64,
    reference_dbfs: f64,
}

impl Vu {
    /// Time constant of each of the two cascaded poles. Step response
    /// 1 − e^(−t/τ)(1 + t/τ) reaches 99 % at t ≈ 6.64 τ, i.e. 300 ms.
    pub const TAU_S: f64 = 0.300 / 6.638_352;

    pub fn new(sample_rate: u32) -> Self {
        Self::with_reference(sample_rate, DEFAULT_REFERENCE_DBFS)
    }

    pub fn with_reference(sample_rate: u32, reference_dbfs: f64) -> Self {
        let coeff = 1.0 - (-1.0 / (Self::TAU_S * sample_rate as f64)).exp();
        Vu {
            state: [0.0; 4],
            coeff,
            reference_dbfs,
        }
    }

    pub fn coeff(&self) -> f64 {
        self.coeff
    }

    /// Run the ballistics over a block. Returns the highest VU reading
    /// (in VU, 0 = reference level) reached in the block, per channel.
    pub fn process(&mut self, samples: &[f32]) -> [f64; 2] {
        let frames = samples.len() / 2;
        let mut max = [0.0f64; 2];
        #[cfg(target_arch = "x86_64")]
        if has_sse2() {
            // SAFETY: reads `frames * 2` floats, updates the 4-element state
            // and writes 2 doubles.
            unsafe {
                patch_meter_ballistics_sse2(
                    samples.as_ptr(),
                    frames,
                    &mut self.state,
                    self.coeff,
                    &mut max,
                )
            };
            return max.map(|m| self.to_vu(m));
        }
        max = reference::ballistics(&samples[..frames * 2], &mut self.state, self.coeff);
        max.map(|m| self.to_vu(m))
    }

    /// Current needle position in VU per channel.
    pub fn reading(&self) -> [f64; 2] {
        [self.to_vu(self.state[2]), self.to_vu(self.state[3])]
    }

    fn to_vu(&self, rectified_avg: f64) -> f64 {
        // Sine-equivalent level in dBFS (AES17): RMS + 3.01 dB.
        let level = rectified_avg * SINE_FORM_FACTOR * std::f64::consts::SQRT_2;
        if level <= 1e-12 {
            -120.0
        } else {
            20.0 * level.log10() - self.reference_dbfs
        }
    }

    pub fn reset(&mut self) {
        self.state = [0.0; 4];
    }
}

/// The same VU ballistics as [`Vu`], driven by block measurements instead
/// of samples: each block's mean rectified level (mean |x|) held for its
/// duration. The two poles are integrated in ≤ 1 ms steps, so a meter fed
/// with 5–43 ms engine blocks reads what the sample-rate [`Vu`] reads.
#[derive(Debug, Clone, Default)]
pub struct BlockVu {
    state: [f64; 2],
}

impl BlockVu {
    /// Feed one block: its mean |x| (linear) and duration (s).
    pub fn step(&mut self, mean_abs: f64, dt: f64) {
        // Also NaN.
        if dt.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
            return;
        }
        let n = (dt / 0.001).ceil().max(1.0);
        let k = 1.0 - (-(dt / n) / Vu::TAU_S).exp();
        for _ in 0..n as usize {
            self.state[0] += (mean_abs.max(0.0) - self.state[0]) * k;
            self.state[1] += (self.state[0] - self.state[1]) * k;
        }
    }

    /// Needle position in VU (0 = `DEFAULT_REFERENCE_DBFS`).
    pub fn reading(&self) -> f64 {
        let level = self.state[1] * SINE_FORM_FACTOR * std::f64::consts::SQRT_2;
        if level <= 1e-12 {
            -120.0
        } else {
            20.0 * level.log10() - DEFAULT_REFERENCE_DBFS
        }
    }

    pub fn reset(&mut self) {
        self.state = [0.0; 2];
    }
}

#[cfg(test)]
mod block_vu_tests {
    use super::*;

    #[test]
    fn block_vu_reads_like_the_sample_vu() {
        // A 1 kHz sine peaking at −18 dBFS reads 0 VU on both, block-fed
        // with 1024-frame engine blocks at 48 kHz.
        let rate = 48_000;
        let amp = 10f64.powf(-18.0 / 20.0);
        let x: Vec<f32> = (0..rate * 2)
            .flat_map(|i| {
                let v = (amp * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / rate as f64).sin())
                    as f32;
                [v, v]
            })
            .collect();
        let mut vu = Vu::new(rate as u32);
        vu.process(&x);
        let mut b = BlockVu::default();
        for block in x.chunks(2048) {
            let mean = block.iter().step_by(2).map(|v| v.abs() as f64).sum::<f64>()
                / (block.len() / 2) as f64;
            b.step(mean, (block.len() / 2) as f64 / rate as f64);
        }
        assert!(vu.reading()[0].abs() < 0.1, "sample VU {}", vu.reading()[0]);
        assert!(b.reading().abs() < 0.1, "block VU {}", b.reading());
    }

    #[test]
    fn block_vu_reaches_99_percent_in_300_ms() {
        let mut b = BlockVu::default();
        let mean = 10f64.powf(-18.0 / 20.0) * 2.0 / std::f64::consts::PI; // sine at −18 dBFS
        for _ in 0..(300 / 5) {
            b.step(mean, 0.005);
        }
        // 99 % of the needle's travel ≈ −0.09 dB below the steady reading.
        assert!(
            (b.reading() - 20.0 * 0.99f64.log10()).abs() < 0.05,
            "{}",
            b.reading()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic noise with occasional full-scale spikes.
    fn signal(frames: usize, seed: u32) -> Vec<f32> {
        let mut x = seed.wrapping_mul(2_654_435_761).max(1);
        (0..frames * 2)
            .map(|i| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                let v = (x as f32 / u32::MAX as f32) * 2.0 - 1.0;
                if i % 977 == 0 { -1.0 } else { v * 0.7 }
            })
            .collect()
    }

    /// Sine at `dbfs` per AES17 (peak level; full-scale sine = 0 dBFS).
    fn sine(frames: usize, rate: f64, freq: f64, dbfs: f64) -> Vec<f32> {
        let amp = 10f64.powf(dbfs / 20.0);
        (0..frames)
            .flat_map(|n| {
                let v = (amp * (std::f64::consts::TAU * freq * n as f64 / rate).sin()) as f32;
                [v, v]
            })
            .collect()
    }

    #[test]
    fn stats_matches_reference_for_every_tail_length() {
        for frames in (0..70).chain([1023, 4096, 48_001]) {
            let s = signal(frames, frames as u32 + 3);
            let (fast, slow) = (stats(&s), reference::stats(&s));
            assert_eq!(fast.peak, slow.peak, "peak, {frames} frames");
            for ch in 0..2 {
                let (a, b) = (fast.sum_sq[ch], slow.sum_sq[ch]);
                assert!(
                    (a - b).abs() <= 1e-9 * b.max(1e-30),
                    "sum_sq ch{ch}, {frames} frames: {a} vs {b}"
                );
            }
        }
    }

    #[test]
    fn channels_never_mix() {
        let mut s = vec![0.0f32; 2 * 37];
        s[2 * 20] = 0.5; // left only
        s[2 * 33 + 1] = -0.25; // right only, in the tail
        let st = stats(&s);
        assert_eq!(st.peak, [0.5, 0.25]);
        assert_eq!(st.sum_sq, [0.25, 0.0625]);
    }

    #[test]
    fn ballistics_are_bit_identical_to_reference() {
        let s = signal(10_007, 11);
        let mut vu = Vu::new(48_000);
        let mut state = [0.0; 4];
        for chunk in s.chunks(2 * 333) {
            let fast = vu.process(chunk);
            let slow = reference::ballistics(chunk, &mut state, vu.coeff());
            assert_eq!(vu.state, state);
            assert_eq!(fast, slow.map(|m| vu.to_vu(m)));
        }
    }

    #[test]
    fn reference_tone_reads_zero_vu() {
        let rate = 48_000.0;
        let mut vu = Vu::new(rate as u32);
        vu.process(&sine(48_000, rate, 1000.0, -18.0));
        let [l, r] = vu.reading();
        assert!(l.abs() < 0.05 && r.abs() < 0.05, "{l} {r}");
        // And a −60 dBFS sine reads −42 VU.
        let mut vu = Vu::new(rate as u32);
        vu.process(&sine(48_000, rate, 1000.0, -60.0));
        assert!((vu.reading()[0] + 42.0).abs() < 0.05, "{:?}", vu.reading());
    }

    #[test]
    fn rise_time_is_300_ms_to_99_percent() {
        let rate = 48_000.0;
        let tone = sine(48_000 * 2, rate, 1000.0, -18.0);
        let mut settled = Vu::new(48_000);
        settled.process(&tone);
        let target = settled.state[2];
        let mut vu = Vu::new(48_000);
        vu.process(&tone[..2 * 14_400]); // 300 ms
        let ratio = vu.state[2] / target;
        assert!((0.985..=0.995).contains(&ratio), "{ratio}");
    }
}
