//! ITU-R BS.1770-4 / EBU R128 loudness and true peak.
//!
//! * K-weighting: two biquads derived for any sample rate (at 48 kHz they
//!   equal the coefficients printed in BS.1770-4), run by the SSE2 assembly
//!   kernel `patch_kweight_sse2`.
//! * Momentary (400 ms), short-term (3 s), integrated (gated: −70 LUFS
//!   absolute, −10 LU relative, 400 ms blocks with 75 % overlap) and loudness
//!   range (EBU Tech 3342: 3 s windows, −70 LUFS / −20 LU gates,
//!   10th–95th percentile).
//! * True peak: 4× oversampling with a 48-tap polyphase interpolator, run by
//!   the AVX2 assembly kernel `patch_truepeak_avx2`.

use std::collections::VecDeque;

/// Biquad coefficients { b0, b1, b2, a1, a2 } for both K-weighting stages.
pub fn k_weighting(rate: f64) -> [f64; 10] {
    use std::f64::consts::PI;
    // Stage 1: high shelf (head acoustic effects).
    let f0 = 1_681.974_450_955_533;
    let g = 3.999_843_853_973_347;
    let q = 0.707_175_236_955_419_6;
    let k = (PI * f0 / rate).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6);
    let a0 = 1.0 + k / q + k * k;
    let s1 = [
        (vh + vb * k / q + k * k) / a0,
        2.0 * (k * k - vh) / a0,
        (vh - vb * k / q + k * k) / a0,
        2.0 * (k * k - 1.0) / a0,
        (1.0 - k / q + k * k) / a0,
    ];
    // Stage 2: RLB high-pass.
    let f0 = 38.135_470_876_024_44;
    let q = 0.500_327_037_323_877_3;
    let k = (PI * f0 / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let s2 = [
        1.0,
        -2.0,
        1.0,
        2.0 * (k * k - 1.0) / a0,
        (1.0 - k / q + k * k) / a0,
    ];
    [
        s1[0], s1[1], s1[2], s1[3], s1[4], s2[0], s2[1], s2[2], s2[3], s2[4],
    ]
}

#[cfg(target_arch = "x86_64")]
unsafe extern "sysv64" {
    fn patch_kweight_sse2(
        samples: *const f32,
        frames: usize,
        state: *mut [f64; 8],
        coeffs: *const [f64; 10],
        sum_sq: *mut [f64; 2],
    );
    fn patch_truepeak_avx2(x: *const f32, groups: usize, coeffs: *const [f32; 48]) -> f32;
}

/// Reference K-weighting: same operations, same order as the kernel.
pub fn kweight_reference(
    samples: &[f32],
    state: &mut [f64; 8],
    c: &[f64; 10],
    sum_sq: &mut [f64; 2],
) {
    for frame in samples.as_chunks::<2>().0 {
        for ch in 0..2 {
            let x = frame[ch] as f64;
            let (z1a, z2a, z1b, z2b) = (state[ch], state[2 + ch], state[4 + ch], state[6 + ch]);
            let y1 = c[0] * x + z1a;
            let n1a = c[1] * x - c[3] * y1 + z2a;
            let n2a = c[2] * x - c[4] * y1;
            let y2 = c[5] * y1 + z1b;
            let n1b = c[6] * y1 - c[8] * y2 + z2b;
            let n2b = c[7] * y1 - c[9] * y2;
            state[ch] = n1a;
            state[2 + ch] = n2a;
            state[4 + ch] = n1b;
            state[6 + ch] = n2b;
            sum_sq[ch] += y2 * y2;
        }
    }
}

fn kweight(samples: &[f32], state: &mut [f64; 8], c: &[f64; 10], sum_sq: &mut [f64; 2]) {
    #[cfg(target_arch = "x86_64")]
    if crate::has_sse2() {
        // SAFETY: reads samples.len()/2 frames, updates 8 state doubles,
        // reads 10 coefficients, adds into 2 sums.
        unsafe { patch_kweight_sse2(samples.as_ptr(), samples.len() / 2, state, c, sum_sq) };
        return;
    }
    kweight_reference(&samples[..samples.len() / 2 * 2], state, c, sum_sq);
}

/// Polyphase interpolator for 4× oversampling: Hann-windowed sinc, 48 taps,
/// laid out as coeffs[phase·12 + k] against x[t + k] (x[t+5] is "now" for
/// phase 0 through the centre of the window). Each phase has unity DC gain.
pub fn truepeak_coeffs() -> [f32; 48] {
    const TAPS: usize = 48;
    const UP: usize = 4;
    let mut h = [0f64; TAPS];
    let centre = (TAPS - 1) as f64 / 2.0;
    for (n, v) in h.iter_mut().enumerate() {
        let m = n as f64 - centre;
        let x = m / UP as f64;
        let sinc = if x.abs() < 1e-12 {
            1.0
        } else {
            (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
        };
        let w = 0.5 - 0.5 * (std::f64::consts::TAU * (n as f64 + 0.5) / TAPS as f64).cos();
        *v = sinc * w;
    }
    // Phase p uses taps n = p + 4k; reverse k so the kernel's x[t+k] order is a convolution.
    let mut out = [0f32; 48];
    for p in 0..UP {
        let taps: Vec<f64> = (0..12).map(|k| h[p + UP * k]).collect();
        let sum: f64 = taps.iter().sum();
        for k in 0..12 {
            out[p * 12 + k] = (taps[11 - k] / sum) as f32;
        }
    }
    out
}

/// Reference true-peak interpolation (same arithmetic as the kernel, f32).
pub fn truepeak_reference(x: &[f32], groups: usize, c: &[f32; 48]) -> f32 {
    let mut max = 0f32;
    for t in 0..groups * 8 {
        for p in 0..4 {
            let mut acc = 0f32;
            for k in 0..12 {
                acc = x[t + k].mul_add(c[p * 12 + k], acc);
            }
            max = max.max(acc.abs());
        }
    }
    max
}

fn truepeak(x: &[f32], groups: usize, c: &[f32; 48]) -> f32 {
    debug_assert!(x.len() >= groups * 8 + 11);
    #[cfg(target_arch = "x86_64")]
    if crate::has_avx2() {
        // SAFETY: reads groups·8 + 11 floats (asserted by callers) and 48 coefficients.
        return unsafe { patch_truepeak_avx2(x.as_ptr(), groups, c) };
    }
    truepeak_reference(x, groups, c)
}

/// Streaming true-peak meter for one stereo stream.
#[derive(Debug, Clone)]
pub struct TruePeak {
    coeffs: [f32; 48],
    /// Per channel: carried history + pending samples.
    buf: [Vec<f32>; 2],
    max: [f32; 2],
}

impl Default for TruePeak {
    fn default() -> Self {
        TruePeak {
            coeffs: truepeak_coeffs(),
            buf: [vec![0.0; 11], vec![0.0; 11]],
            max: [0.0; 2],
        }
    }
}

impl TruePeak {
    pub fn push(&mut self, samples: &[f32]) {
        for frame in samples.as_chunks::<2>().0 {
            self.buf[0].push(frame[0]);
            self.buf[1].push(frame[1]);
        }
        for ch in 0..2 {
            let b = &mut self.buf[ch];
            let groups = (b.len() - 11) / 8;
            if groups > 0 {
                let m = truepeak(b, groups, &self.coeffs);
                self.max[ch] = self.max[ch].max(m);
                b.drain(..groups * 8);
            }
        }
    }

    /// Max true peak per channel since the last call, linear.
    pub fn take(&mut self) -> [f32; 2] {
        std::mem::take(&mut self.max)
    }
}

pub fn to_lufs(power: f64) -> f64 {
    if power <= 0.0 {
        f64::NEG_INFINITY
    } else {
        -0.691 + 10.0 * power.log10()
    }
}

/// EBU R128 loudness meter for a stereo stream (channel weights 1.0).
#[derive(Debug, Clone)]
pub struct Loudness {
    rate: u32,
    coeffs: [f64; 10],
    state: [f64; 8],
    /// Samples per 100 ms step.
    step: usize,
    acc: [f64; 2],
    acc_frames: usize,
    /// Power (L + R mean square) of each completed 100 ms step, newest last.
    steps: VecDeque<f64>,
    /// Gating block powers (400 ms, every 100 ms) since reset, and the
    /// short-term (3 s) loudness values for LRA: as histograms, so the
    /// integrated loudness and LRA cost the same after hours as after a
    /// minute. (Kept as lists, both were recomputed over every value since
    /// reset 50 times a second: a cost growing without end.)
    blocks: Histogram,
    short: Histogram,
    /// The exact values, in tests only (the oracle for the histograms).
    #[cfg(test)]
    raw: (Vec<f64>, Vec<f64>),
    pub true_peak: TruePeak,
}

impl Loudness {
    pub fn new(rate: u32) -> Self {
        Loudness {
            rate,
            coeffs: k_weighting(rate as f64),
            state: [0.0; 8],
            step: (rate as usize / 10).max(1),
            acc: [0.0; 2],
            acc_frames: 0,
            steps: VecDeque::with_capacity(31),
            blocks: Histogram::default(),
            short: Histogram::default(),
            #[cfg(test)]
            raw: (vec![], vec![]),
            true_peak: TruePeak::default(),
        }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn reset(&mut self) {
        *self = Loudness::new(self.rate);
    }

    /// Feed interleaved stereo samples.
    pub fn push(&mut self, mut samples: &[f32]) {
        self.true_peak.push(samples);
        while samples.len() >= 2 {
            let room = self.step - self.acc_frames;
            let take = room.min(samples.len() / 2);
            let (now, rest) = samples.split_at(take * 2);
            kweight(now, &mut self.state, &self.coeffs, &mut self.acc);
            self.acc_frames += take;
            samples = rest;
            if self.acc_frames == self.step {
                self.finish_step();
            }
        }
    }

    fn finish_step(&mut self) {
        let n = self.step as f64;
        let power = self.acc[0] / n + self.acc[1] / n;
        self.acc = [0.0; 2];
        self.acc_frames = 0;
        self.steps.push_back(power);
        while self.steps.len() > 30 {
            self.steps.pop_front();
        }
        if self.steps.len() >= 4 {
            let p = self.window(4);
            self.blocks.add(to_lufs(p), p);
            #[cfg(test)]
            self.raw.0.push(p);
        }
        if self.steps.len() >= 30 {
            let p = self.window(30);
            self.short.add(to_lufs(p), p);
            #[cfg(test)]
            self.raw.1.push(to_lufs(p));
        }
    }

    fn window(&self, n: usize) -> f64 {
        let n = n.min(self.steps.len());
        if n == 0 {
            return 0.0;
        }
        self.steps.iter().rev().take(n).sum::<f64>() / n as f64
    }

    /// Momentary loudness (400 ms), LUFS.
    pub fn momentary(&self) -> f64 {
        to_lufs(self.window(4))
    }

    /// Short-term loudness (3 s), LUFS.
    pub fn short_term(&self) -> f64 {
        to_lufs(self.window(30))
    }

    /// Integrated (gated) loudness since reset, LUFS.
    pub fn integrated(&self) -> f64 {
        // Absolute gate (−70 LUFS) by construction; then −10 LU relative.
        let (n, sum) = self.blocks.above(f64::NEG_INFINITY);
        if n == 0 {
            return f64::NEG_INFINITY;
        }
        let rel_gate = to_lufs(sum / n as f64) - 10.0;
        let (n, sum) = self.blocks.above(rel_gate);
        if n == 0 {
            return f64::NEG_INFINITY;
        }
        to_lufs(sum / n as f64)
    }

    /// Loudness range (EBU Tech 3342), LU.
    pub fn loudness_range(&self) -> f64 {
        let (n, sum) = self.short.above(f64::NEG_INFINITY);
        if n < 2 {
            return 0.0;
        }
        let gate = to_lufs(sum / n as f64) - 20.0;
        match (
            self.short.percentile(gate, 0.95),
            self.short.percentile(gate, 0.10),
        ) {
            (Some(hi), Some(lo)) => hi - lo,
            _ => 0.0,
        }
    }
}

/// Loudness values in 0.01 LU bins from −70 to +10 LUFS, each with its
/// count and summed power: gates and percentiles in a fixed number of
/// steps, within 0.005 LU of the exact values.
#[derive(Clone, Debug)]
struct Histogram {
    count: Vec<u64>,
    power: Vec<f64>,
}

const H_LO: f64 = -70.0;
const H_STEP: f64 = 0.01;
const H_BINS: usize = 8000;

impl Default for Histogram {
    fn default() -> Self {
        Histogram {
            count: vec![0; H_BINS],
            power: vec![0.0; H_BINS],
        }
    }
}

impl Histogram {
    /// A value `lufs` (power `p`); at or below −70 LUFS it's gated out.
    fn add(&mut self, lufs: f64, p: f64) {
        // Also NaN.
        if lufs.partial_cmp(&H_LO) != Some(std::cmp::Ordering::Greater) {
            return;
        }
        let i = (((lufs - H_LO) / H_STEP) as usize).min(H_BINS - 1);
        self.count[i] += 1;
        self.power[i] += p;
    }

    /// The bin a loudness falls in's lower edge, and a value's bin centre.
    fn centre(i: usize) -> f64 {
        H_LO + (i as f64 + 0.5) * H_STEP
    }

    /// Count and summed power of the values above `gate` (by bin centre).
    fn above(&self, gate: f64) -> (u64, f64) {
        let mut n = 0;
        let mut sum = 0.0;
        for i in 0..H_BINS {
            if self.count[i] > 0 && Self::centre(i) > gate {
                n += self.count[i];
                sum += self.power[i];
            }
        }
        (n, sum)
    }

    /// The `p` percentile (nearest rank, as the sorted list gave it) of the
    /// values above `gate`, None under two values.
    fn percentile(&self, gate: f64, p: f64) -> Option<f64> {
        let first = (0..H_BINS).find(|i| Self::centre(*i) > gate)?;
        let total: u64 = self.count[first..].iter().sum();
        if total < 2 {
            return None;
        }
        let rank = ((total - 1) as f64 * p).round() as u64;
        let mut seen = 0;
        for i in first..H_BINS {
            seen += self.count[i];
            if seen > rank {
                return Some(Self::centre(i));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(frames: usize, rate: f64, freq: f64, dbfs_peak: f64, phase: f64) -> Vec<f32> {
        let a = 10f64.powf(dbfs_peak / 20.0);
        (0..frames)
            .flat_map(|n| {
                let v = (a * (std::f64::consts::TAU * freq * n as f64 / rate + phase).sin()) as f32;
                [v, v]
            })
            .collect()
    }

    #[test]
    fn coefficients_match_bs1770_at_48k() {
        let c = k_weighting(48_000.0);
        let expected = [
            1.535_124_859_586_97,
            -2.691_696_189_406_38,
            1.198_392_810_852_85,
            -1.690_659_293_182_41,
            0.732_480_774_215_85,
            1.0,
            -2.0,
            1.0,
            -1.990_047_454_833_98,
            0.990_072_250_366_21,
        ];
        for (a, b) in c.iter().zip(expected) {
            assert!((a - b).abs() < 1e-9, "{a} vs {b}");
        }
    }

    #[test]
    fn kweight_kernel_is_bit_identical() {
        let s = sine(9_601, 48_000.0, 997.0, -6.0, 0.3);
        let c = k_weighting(48_000.0);
        let (mut st_a, mut st_b) = ([0.0; 8], [0.0; 8]);
        let (mut sum_a, mut sum_b) = ([0.0; 2], [0.0; 2]);
        for chunk in s.chunks(2 * 1_111) {
            kweight(chunk, &mut st_a, &c, &mut sum_a);
            kweight_reference(chunk, &mut st_b, &c, &mut sum_b);
        }
        assert_eq!(st_a, st_b);
        assert_eq!(sum_a, sum_b);
    }

    /// EBU Tech 3341 case 1: stereo 1 kHz sine at −23 dBFS (peak) reads
    /// −23.0 LUFS momentary, short-term and integrated (±0.1).
    #[test]
    fn tech3341_case1_minus_23() {
        let mut m = Loudness::new(48_000);
        m.push(&sine(48_000 * 20, 48_000.0, 1000.0, -23.0, 0.0));
        for (name, v) in [
            ("M", m.momentary()),
            ("S", m.short_term()),
            ("I", m.integrated()),
        ] {
            assert!((v + 23.0).abs() < 0.1, "{name} = {v}");
        }
    }

    /// EBU Tech 3341 case 2: −33 dBFS reads −33.0 LUFS.
    #[test]
    fn tech3341_case2_minus_33() {
        let mut m = Loudness::new(48_000);
        m.push(&sine(48_000 * 20, 48_000.0, 1000.0, -33.0, 0.0));
        assert!((m.integrated() + 33.0).abs() < 0.1, "{}", m.integrated());
    }

    /// EBU Tech 3341 case 3-style gating: 10 s at −36, 60 s at −23, 10 s at
    /// −36 → integrated −23.0 (the quiet parts fall under the relative gate).
    #[test]
    fn relative_gate() {
        let mut m = Loudness::new(48_000);
        m.push(&sine(48_000 * 10, 48_000.0, 1000.0, -36.0, 0.0));
        m.push(&sine(48_000 * 60, 48_000.0, 1000.0, -23.0, 0.0));
        m.push(&sine(48_000 * 10, 48_000.0, 1000.0, -36.0, 0.0));
        assert!((m.integrated() + 23.0).abs() < 0.1, "{}", m.integrated());
    }

    /// Absolute gate: silence doesn't pull the integrated value down.
    #[test]
    fn absolute_gate() {
        let mut m = Loudness::new(48_000);
        m.push(&vec![0.0; 48_000 * 2 * 10]);
        m.push(&sine(48_000 * 20, 48_000.0, 1000.0, -23.0, 0.0));
        assert!((m.integrated() + 23.0).abs() < 0.1, "{}", m.integrated());
    }

    /// Loudness range of two alternating levels 10 LU apart ≈ 10 LU.
    #[test]
    fn histogram_matches_the_exact_calculation() {
        // A level that moves (quiet and loud parts, a few silences): the
        // exact gates over every value against the histograms.
        let rate = 48_000.0;
        let mut m = Loudness::new(48_000);
        for sec in 0..240 {
            let db =
                -30.0 + 18.0 * ((sec as f64) / 17.0).sin() + if sec % 50 < 5 { -60.0 } else { 0.0 };
            m.push(&sine(48_000, rate, 997.0, db, sec as f64));
        }
        let (blocks, short) = &m.raw;
        let exact_i = {
            let abs: Vec<f64> = blocks
                .iter()
                .copied()
                .filter(|p| to_lufs(*p) > -70.0)
                .collect();
            let gate = to_lufs(abs.iter().sum::<f64>() / abs.len() as f64) - 10.0;
            let g: Vec<f64> = abs.into_iter().filter(|p| to_lufs(*p) > gate).collect();
            to_lufs(g.iter().sum::<f64>() / g.len() as f64)
        };
        let exact_lra = {
            let abs: Vec<f64> = short.iter().copied().filter(|l| *l > -70.0).collect();
            let mean = abs
                .iter()
                .map(|l| 10f64.powf((l + 0.691) / 10.0))
                .sum::<f64>()
                / abs.len() as f64;
            let gate = to_lufs(mean) - 20.0;
            let mut g: Vec<f64> = abs.into_iter().filter(|l| *l > gate).collect();
            g.sort_by(f64::total_cmp);
            let pct = |p: f64| g[((g.len() - 1) as f64 * p).round() as usize];
            pct(0.95) - pct(0.10)
        };
        assert!(
            (m.integrated() - exact_i).abs() < 0.01,
            "I {} vs {exact_i}",
            m.integrated()
        );
        assert!(
            (m.loudness_range() - exact_lra).abs() < 0.02,
            "LRA {} vs {exact_lra}",
            m.loudness_range()
        );
    }

    #[test]
    fn loudness_range() {
        let mut m = Loudness::new(48_000);
        for _ in 0..3 {
            m.push(&sine(48_000 * 20, 48_000.0, 1000.0, -30.0, 0.0));
            m.push(&sine(48_000 * 20, 48_000.0, 1000.0, -20.0, 0.0));
        }
        let lra = m.loudness_range();
        assert!((lra - 10.0).abs() < 0.5, "{lra}");
    }

    #[test]
    fn truepeak_kernel_matches_reference() {
        let s: Vec<f32> = (0..(64 * 8 + 11))
            .map(|i| ((i as f32) * 0.37).sin() * 0.9)
            .collect();
        let c = truepeak_coeffs();
        let a = truepeak(&s, 64, &c);
        let b = truepeak_reference(&s, 64, &c);
        assert!((a - b).abs() <= 1e-6, "{a} vs {b}");
    }

    /// A sine at fs/4 sampled 45° off its peaks has sample peaks 3.01 dB below
    /// its true peak. The 4× meter must find the true peak (±0.3 dB).
    #[test]
    fn true_peak_finds_intersample_peak() {
        let s = sine(
            48_000,
            48_000.0,
            12_000.0,
            -6.0,
            std::f64::consts::FRAC_PI_4,
        );
        let sample_peak = s.iter().fold(0f32, |m, v| m.max(v.abs()));
        let mut tp = TruePeak::default();
        tp.push(&s);
        let [l, _] = tp.take();
        let (sp_db, tp_db) = (20.0 * sample_peak.log10(), 20.0 * l.log10());
        assert!((sp_db + 9.01).abs() < 0.05, "sample peak {sp_db}");
        assert!((tp_db + 6.0).abs() < 0.3, "true peak {tp_db}");
    }

    /// A 1 kHz sine's true peak equals its sample peak (within 0.1 dB).
    #[test]
    fn true_peak_of_low_sine() {
        let s = sine(48_000, 48_000.0, 1000.0, -1.0, 0.1);
        let mut tp = TruePeak::default();
        tp.push(&s);
        let [l, _] = tp.take();
        assert!((20.0 * l.log10() + 1.0).abs() < 0.1, "{}", 20.0 * l.log10());
    }
}
