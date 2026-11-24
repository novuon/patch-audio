//! The Analyzer Unit's measurements (studio reference, non-real-time).
//!
//! * [`StereoMeter`]: phase correlation, balance, M/S width, RMS (AES17,
//!   sine-referenced), DC offset and crest factor per channel, integrated
//!   exponentially (300 ms, as IEC correlation meters) from double-precision
//!   sums computed by the AVX2+FMA kernel [`crate::stereo_sums`].
//! * [`Rta`]: 1/6-octave real-time analyser, 22 Hz – 18 kHz (59 bands on
//!   ISO-style centres 1 kHz · 2^(k/6)). Two resolutions: a 4096-point
//!   FFT at the program rate for bands from 160 Hz, and an 8192-point FFT
//!   of the signal decimated by 8 (8th-order Butterworth anti-alias
//!   low-pass, > 90 dB rejection where aliases would land) for the bands
//!   below. Fast bass bars use a shorter 2048-point window; the slow line
//!   keeps the accurate bass resolution. Band energy is
//!   calibrated so a full-scale sine in a band reads 0 dBFS; fast (125 ms)
//!   and slow (1 s) ballistics and a 2 s peak hold.
//! * [`Scope`]: vectorscope points (M/S), the last ~40 ms.

use crate::spectrum::fft;
use std::collections::VecDeque;
use std::f64::consts::TAU;

const FULL_FFT_N: usize = 4096;
const LOW_FFT_N: usize = 8192;
const LOW_FAST_FFT_N: usize = 2048;
/// New full-rate frames between RTA updates (≈ 43 ms at 48 kHz).
const HOP: usize = 2048;
const DECIMATE: usize = 8;
/// Bands at or above this centre come from the full-rate FFT.
const SPLIT_HZ: f64 = 160.0;

/// Stereo correlation, balance, width, RMS, DC, crest.
#[derive(Clone, Debug)]
pub struct StereoMeter {
    rate: f64,
    tau: f64,
    acc: crate::StereoSums,
    weight: f64,
    /// DC: the mean over a longer window (3 s), so low bass and
    /// infrasonic swings do not read as offset.
    dc: [f64; 2],
    dc_weight: f64,
    peak: [f32; 2],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StereoReading {
    /// −1 … +1 (1 = mono, 0 = unrelated, −1 = out of phase).
    pub correlation: f64,
    /// L re R, dB (positive = left louder).
    pub balance_db: f64,
    /// S re M, dB (−∞ = mono; 0 = as much side as mid).
    pub width_db: f64,
    /// RMS per channel, dBFS (AES17: a full-scale sine reads 0 dBFS).
    pub rms_dbfs: [f64; 2],
    /// Mean per channel over 3 s (DC offset), dBFS.
    pub dc_dbfs: [f64; 2],
    /// Sample peak (held over the integration time), dBFS.
    pub peak_dbfs: [f64; 2],
    /// Peak re RMS, dB.
    pub crest_db: [f64; 2],
    /// Mid and side RMS, dBFS.
    pub mid_dbfs: f64,
    pub side_dbfs: f64,
}

impl Default for StereoReading {
    fn default() -> Self {
        Self {
            correlation: 0.0,
            balance_db: 0.0,
            width_db: f64::NEG_INFINITY,
            rms_dbfs: [f64::NEG_INFINITY; 2],
            dc_dbfs: [f64::NEG_INFINITY; 2],
            peak_dbfs: [f64::NEG_INFINITY; 2],
            crest_db: [0.0; 2],
            mid_dbfs: f64::NEG_INFINITY,
            side_dbfs: f64::NEG_INFINITY,
        }
    }
}

fn db(x: f64) -> f64 {
    if x > 1e-12 {
        10.0 * x.log10()
    } else {
        f64::NEG_INFINITY
    }
}

impl StereoMeter {
    pub fn new(rate: u32) -> StereoMeter {
        StereoMeter {
            rate: rate as f64,
            tau: 0.3,
            acc: Default::default(),
            weight: 0.0,
            dc: [0.0; 2],
            dc_weight: 0.0,
            peak: [0.0; 2],
        }
    }

    /// Integrate one block of interleaved stereo.
    pub fn push(&mut self, samples: &[f32]) {
        let n = (samples.len() / 2) as f64;
        if n == 0.0 {
            return;
        }
        let a = (-n / (self.tau * self.rate)).exp();
        let s = crate::stereo_sums(samples);
        let st = crate::stats(samples);
        let o = &mut self.acc;
        o.sum = [o.sum[0] * a + s.sum[0], o.sum[1] * a + s.sum[1]];
        o.sq = [o.sq[0] * a + s.sq[0], o.sq[1] * a + s.sq[1]];
        o.lr = o.lr * a + s.lr;
        self.weight = self.weight * a + n;
        let d = (-n / (3.0 * self.rate)).exp();
        self.dc = [self.dc[0] * d + s.sum[0], self.dc[1] * d + s.sum[1]];
        self.dc_weight = self.dc_weight * d + n;
        for c in 0..2 {
            self.peak[c] = (self.peak[c] * a as f32).max(st.peak[c]);
        }
    }

    pub fn reading(&self) -> StereoReading {
        let w = self.weight.max(1e-9);
        let (ll, rr, lr) = (self.acc.sq[0] / w, self.acc.sq[1] / w, self.acc.lr / w);
        let correlation = if ll > 1e-12 && rr > 1e-12 {
            (lr / (ll * rr).sqrt()).clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let mm = (ll + rr + 2.0 * lr) / 4.0;
        let ss = ((ll + rr - 2.0 * lr) / 4.0).max(0.0);
        let rms = [db(2.0 * ll), db(2.0 * rr)];
        let peak = [
            20.0 * (self.peak[0] as f64).max(1e-9).log10(),
            20.0 * (self.peak[1] as f64).max(1e-9).log10(),
        ];
        StereoReading {
            correlation,
            balance_db: if ll > 1e-12 && rr > 1e-12 {
                db(ll) - db(rr)
            } else {
                0.0
            },
            width_db: if mm > 1e-12 {
                db(ss) - db(mm)
            } else {
                f64::NEG_INFINITY
            },
            rms_dbfs: rms,
            dc_dbfs: {
                let dw = self.dc_weight.max(1e-9);
                [
                    20.0 * (self.dc[0] / dw).abs().max(1e-12).log10(),
                    20.0 * (self.dc[1] / dw).abs().max(1e-12).log10(),
                ]
            },
            peak_dbfs: peak,
            // Crest re the sine-referenced RMS + 3.01 dB (so a sine reads 3.01).
            crest_db: [peak[0] - rms[0] + 3.0103, peak[1] - rms[1] + 3.0103],
            mid_dbfs: db(2.0 * mm),
            side_dbfs: db(2.0 * ss),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn lowpass(f: f64, q: f64, fs: f64) -> Biquad {
        let w = TAU * f / fs;
        let (c, al) = (w.cos(), w.sin() / (2.0 * q));
        let a0 = 1.0 + al;
        Biquad {
            b: [(1.0 - c) / 2.0 / a0, (1.0 - c) / a0, (1.0 - c) / 2.0 / a0],
            a: [-2.0 * c / a0, (1.0 - al) / a0],
            z: [0.0; 2],
        }
    }

    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// 1/6-octave band centres, 20 Hz – 20 kHz.
pub fn band_centres() -> Vec<f64> {
    let lo = (6.0 * (20.0f64 / 1000.0).log2()).ceil() as i32;
    let hi = (6.0 * (20_000.0f64 / 1000.0).log2()).floor() as i32;
    (lo..=hi)
        .map(|k| 1000.0 * 2f64.powf(k as f64 / 6.0))
        .collect()
}

struct Fft {
    n: usize,
    window: Vec<f64>,
    norm: f64,
    re: Vec<f64>,
    im: Vec<f64>,
}

impl Fft {
    fn new(n: usize) -> Fft {
        let window: Vec<f64> = (0..n)
            .map(|i| 0.5 - 0.5 * (TAU * i as f64 / n as f64).cos())
            .collect();
        let wp: f64 = window.iter().map(|w| w * w).sum();
        // One-sided power normalised to mean square; ×2 at readout so a
        // full-scale sine reads 0 dBFS (AES17).
        Fft {
            n,
            window,
            norm: 2.0 / (n as f64 * wp),
            re: vec![0.0; n],
            im: vec![0.0; n],
        }
    }

    /// Band powers (mean square) of the last `n` samples of `x` for the
    /// given (lo, hi) edges at `rate`.
    fn bands(&mut self, x: &VecDeque<f32>, rate: f64, edges: &[(f64, f64)]) -> Vec<f64> {
        let off = x.len().saturating_sub(self.n);
        for i in 0..self.n {
            self.re[i] = x.get(off + i).copied().unwrap_or(0.0) as f64 * self.window[i];
            self.im[i] = 0.0;
        }
        fft(&mut self.re, &mut self.im);
        let bin = rate / self.n as f64;
        edges
            .iter()
            .map(|&(lo, hi)| {
                let mut k0 = (lo / bin).ceil() as usize;
                let mut k1 = (hi / bin).floor() as usize;
                if k1 < k0 {
                    k0 = ((lo * hi).sqrt() / bin).round() as usize;
                    k1 = k0;
                }
                (k0.max(1)..=k1.min(self.n / 2 - 1))
                    .map(|k| (self.re[k] * self.re[k] + self.im[k] * self.im[k]) * self.norm)
                    .sum::<f64>()
            })
            .collect()
    }
}

pub struct Rta {
    rate: f64,
    centres: Vec<f64>,
    /// Mono history at the full rate and decimated.
    full: VecDeque<f32>,
    low: VecDeque<f32>,
    aa: [Biquad; 4],
    phase: usize,
    since: usize,
    fft_full: Fft,
    fft_low: Fft,
    fft_low_fast: Fft,
    /// Band power (mean square): fast, slow; peak (dBFS) and its age.
    fast: Vec<f64>,
    slow: Vec<f64>,
    peak: Vec<f64>,
    peak_age: Vec<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RtaReading {
    pub centres: Vec<f64>,
    /// dBFS per band (AES17 sine-referenced).
    pub fast: Vec<f32>,
    pub slow: Vec<f32>,
    pub peak: Vec<f32>,
}

impl Rta {
    pub fn new(rate: u32) -> Rta {
        let fs = rate as f64;
        // 8th-order Butterworth at 1.5 kHz (Q values of the 4 sections).
        let qs = [0.5098, 0.6013, 0.9000, 2.5629];
        let centres = band_centres();
        let n = centres.len();
        Rta {
            rate: fs,
            centres,
            full: VecDeque::with_capacity(FULL_FFT_N + HOP),
            low: VecDeque::with_capacity(LOW_FFT_N + HOP),
            aa: qs.map(|q| Biquad::lowpass(1500.0, q, fs)),
            phase: 0,
            since: 0,
            fft_full: Fft::new(FULL_FFT_N),
            fft_low: Fft::new(LOW_FFT_N),
            fft_low_fast: Fft::new(LOW_FAST_FFT_N),
            fast: vec![0.0; n],
            slow: vec![0.0; n],
            peak: vec![-140.0; n],
            peak_age: vec![0.0; n],
        }
    }

    pub fn reset_peaks(&mut self) {
        self.peak.fill(-140.0);
    }

    /// Feed interleaved stereo (analysed as mono, (L+R)/2).
    pub fn push(&mut self, samples: &[f32]) {
        for f in samples.as_chunks::<2>().0 {
            let m = (f[0] + f[1]) * 0.5;
            self.full.push_back(m);
            let mut y = m as f64;
            for s in &mut self.aa {
                y = s.run(y);
            }
            self.phase += 1;
            if self.phase == DECIMATE {
                self.phase = 0;
                self.low.push_back(y as f32);
            }
            self.since += 1;
            if self.since >= HOP {
                self.since = 0;
                self.update(HOP as f64 / self.rate);
            }
        }
        while self.full.len() > FULL_FFT_N {
            self.full.pop_front();
        }
        while self.low.len() > LOW_FFT_N {
            self.low.pop_front();
        }
    }

    fn update(&mut self, dt: f64) {
        let edges: Vec<(f64, f64)> = self
            .centres
            .iter()
            .map(|c| (c * 2f64.powf(-1.0 / 12.0), c * 2f64.powf(1.0 / 12.0)))
            .collect();
        let split = self
            .centres
            .iter()
            .position(|c| *c >= SPLIT_HZ)
            .unwrap_or(self.centres.len());
        let high = self.fft_full.bands(&self.full, self.rate, &edges[split..]);
        let low = self
            .fft_low
            .bands(&self.low, self.rate / DECIMATE as f64, &edges[..split]);
        let low_fast =
            self.fft_low_fast
                .bands(&self.low, self.rate / DECIMATE as f64, &edges[..split]);
        let (af, as_) = ((-dt / 0.125).exp(), (-dt / 1.0).exp());
        for (i, (p, fast_p)) in low
            .into_iter()
            .chain(high.iter().copied())
            .zip(low_fast.into_iter().chain(high.iter().copied()))
            .enumerate()
        {
            self.fast[i] = self.fast[i] * af + fast_p * (1.0 - af);
            self.slow[i] = self.slow[i] * as_ + p * (1.0 - as_);
            let now = db(2.0 * self.fast[i]).max(-140.0);
            if now >= self.peak[i] {
                self.peak[i] = now;
                self.peak_age[i] = 0.0;
            } else {
                self.peak_age[i] += dt;
                if self.peak_age[i] > 2.0 {
                    self.peak[i] = (self.peak[i] - 20.0 * dt).max(now);
                }
            }
        }
    }

    pub fn reading(&self) -> RtaReading {
        let d = |v: &Vec<f64>| v.iter().map(|p| db(2.0 * p).max(-140.0) as f32).collect();
        RtaReading {
            centres: self.centres.clone(),
            fast: d(&self.fast),
            slow: d(&self.slow),
            peak: self.peak.iter().map(|p| *p as f32).collect(),
        }
    }
}

/// Vectorscope points: (side, mid) of the last `N` frames, at most `N`.
pub struct Scope {
    pts: VecDeque<[f32; 2]>,
    cap: usize,
}

impl Scope {
    pub fn new(cap: usize) -> Scope {
        Scope {
            pts: VecDeque::with_capacity(cap),
            cap,
        }
    }

    pub fn push(&mut self, samples: &[f32]) {
        for f in samples.as_chunks::<2>().0 {
            // Rotated 45°: mono stands up, out-of-phase lies down.
            let (m, s) = (
                (f[0] + f[1]) * std::f32::consts::FRAC_1_SQRT_2,
                (f[0] - f[1]) * std::f32::consts::FRAC_1_SQRT_2,
            );
            self.pts.push_back([s, m]);
        }
        while self.pts.len() > self.cap {
            self.pts.pop_front();
        }
    }

    pub fn points(&self) -> Vec<[f32; 2]> {
        self.pts.iter().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;

    fn stereo(n: usize, f: impl Fn(f64) -> (f64, f64)) -> Vec<f32> {
        (0..n)
            .flat_map(|i| {
                let (l, r) = f(i as f64 / FS);
                [l as f32, r as f32]
            })
            .collect()
    }

    #[test]
    fn kernel_matches_the_reference() {
        for n in [0, 1, 3, 4, 5, 17, 1024, 4099] {
            let x = stereo(n, |t| {
                (
                    (TAU * 440.0 * t).sin() * 0.7 + 0.01,
                    (TAU * 97.0 * t).cos() * 0.3 - 0.02,
                )
            });
            let (a, b) = (crate::stereo_sums(&x), crate::reference::stereo_sums(&x));
            let close = |p: f64, q: f64| (p - q).abs() <= 1e-9 * q.abs().max(1.0);
            assert!(
                close(a.sum[0], b.sum[0]) && close(a.sum[1], b.sum[1]),
                "n={n} sum"
            );
            assert!(
                close(a.sq[0], b.sq[0]) && close(a.sq[1], b.sq[1]),
                "n={n} sq"
            );
            assert!(close(a.lr, b.lr), "n={n} lr {} vs {}", a.lr, b.lr);
        }
    }

    fn settle(m: &mut StereoMeter, x: &[f32]) -> StereoReading {
        for b in x.chunks(1024) {
            m.push(b);
        }
        m.reading()
    }

    #[test]
    fn correlation_balance_width() {
        let n = FS as usize * 2;
        let mono = stereo(n, |t| {
            (
                (TAU * 1000.0 * t).sin() * 0.5,
                (TAU * 1000.0 * t).sin() * 0.5,
            )
        });
        let r = settle(&mut StereoMeter::new(48_000), &mono);
        assert!(
            (r.correlation - 1.0).abs() < 1e-6 && r.width_db < -100.0 && r.balance_db.abs() < 1e-6
        );
        let anti = stereo(n, |t| {
            (
                (TAU * 1000.0 * t).sin() * 0.5,
                -(TAU * 1000.0 * t).sin() * 0.5,
            )
        });
        assert!((settle(&mut StereoMeter::new(48_000), &anti).correlation + 1.0).abs() < 1e-6);
        let quad = stereo(n, |t| {
            (
                (TAU * 1000.0 * t).sin() * 0.5,
                (TAU * 1000.0 * t).cos() * 0.5,
            )
        });
        let q = settle(&mut StereoMeter::new(48_000), &quad);
        assert!(
            q.correlation.abs() < 0.01 && q.width_db.abs() < 0.1,
            "{q:?}"
        );
        let left = stereo(n, |t| {
            (
                (TAU * 1000.0 * t).sin() * 0.5,
                (TAU * 1000.0 * t).sin() * 0.25,
            )
        });
        assert!((settle(&mut StereoMeter::new(48_000), &left).balance_db - 6.0206).abs() < 0.01);
    }

    #[test]
    fn rms_crest_dc_follow_aes17() {
        let n = FS as usize * 2;
        // −20 dBFS sine: RMS reads −20 dBFS (AES17), crest 3.01 dB.
        let a = 10f64.powf(-20.0 / 20.0);
        let x = stereo(n, |t| {
            ((TAU * 997.0 * t).sin() * a, (TAU * 997.0 * t).sin() * a)
        });
        let r = settle(&mut StereoMeter::new(48_000), &x);
        assert!((r.rms_dbfs[0] + 20.0).abs() < 0.02, "{:?}", r.rms_dbfs);
        assert!((r.crest_db[0] - 3.01).abs() < 0.05, "{:?}", r.crest_db);
        let dc = stereo(n * 4, |_| (0.01, 0.0));
        assert!((settle(&mut StereoMeter::new(48_000), &dc).dc_dbfs[0] + 40.0).abs() < 0.05);
    }

    #[test]
    fn rta_reads_a_full_scale_sine_at_0_dbfs_in_its_band() {
        for f in [1000.0, 50.0, 25.0, 12_700.0] {
            let mut rta = Rta::new(48_000);
            let x = stereo(FS as usize * 4, |t| {
                ((TAU * f * t).sin(), (TAU * f * t).sin())
            });
            rta.push(&x);
            let r = rta.reading();
            let (i, _) = r
                .centres
                .iter()
                .enumerate()
                .min_by(|a, b| {
                    (a.1.log2() - f.log2())
                        .abs()
                        .total_cmp(&(b.1.log2() - f.log2()).abs())
                })
                .unwrap();
            assert!(
                r.slow[i].abs() < 0.8,
                "{f} Hz band reads {} dBFS",
                r.slow[i]
            );
            // Two bands away is far down (resolution, and no aliasing into
            // the low bands for high tones).
            if i >= 2 {
                assert!(
                    r.slow[i - 2] < -25.0,
                    "{f} Hz leaks {} dB two bands below",
                    r.slow[i - 2]
                );
            }
        }
    }

    #[test]
    fn low_bands_are_alias_free() {
        // A strong 5.9 kHz tone would fold to ~100 Hz without anti-aliasing.
        let mut rta = Rta::new(48_000);
        let x = stereo(FS as usize * 4, |t| {
            ((TAU * 5_900.0 * t).sin(), (TAU * 5_900.0 * t).sin())
        });
        rta.push(&x);
        let r = rta.reading();
        let worst = r
            .centres
            .iter()
            .zip(&r.slow)
            .filter(|(c, _)| **c < SPLIT_HZ)
            .map(|(_, v)| *v)
            .fold(-200.0f32, f32::max);
        assert!(worst < -85.0, "alias at {worst} dBFS");
    }

    #[test]
    fn fast_bass_reacts_before_the_slow_reference() {
        let mut rta = Rta::new(48_000);
        rta.push(&vec![0.0; FS as usize * 2 * 2]);
        let burst = stereo(FS as usize / 4, |t| {
            let x = (TAU * 50.0 * t).sin();
            (x, x)
        });
        rta.push(&burst);
        let r = rta.reading();
        let i = r
            .centres
            .iter()
            .position(|c| *c > 48.0 && *c < 53.0)
            .unwrap();
        assert!(
            r.fast[i] > r.slow[i] + 3.0,
            "fast {} slow {}",
            r.fast[i],
            r.slow[i]
        );
    }

    #[test]
    fn has_59_sixth_octave_bands() {
        let c = band_centres();
        assert_eq!(c.len(), 59);
        assert!((c[0] - 22.1).abs() < 0.1 && (*c.last().unwrap() - 17_959.0).abs() < 5.0);
    }
}
