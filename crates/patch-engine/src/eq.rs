//! DJ channel EQ and filter, on the real-time thread.
//!
//! * **Isolator EQ**: three bands split by 4th-order Linkwitz–Riley
//!   crossovers at 250 Hz and 2.5 kHz (two cascaded Butterworth biquads per
//!   filter). The low band passes through the upper crossover's LP+HP pair
//!   too, so all three bands share one phase response and sum flat
//!   (magnitude) at unity. Each band: full kill … +6 dB, gains ramped
//!   per sample: no zipper noise.
//! * **Filter**: one knob, −1 … +1. Left: resonant low-pass from 20 kHz down
//!   to 60 Hz; right: high-pass from 20 Hz up to 8 kHz; swept on an octave
//!   scale, coefficients updated every 64 samples so sweeps stay smooth.
//! * **Bypass**: flat EQ and a centred filter leave the signal untouched
//!   (bit-exact), so a channel without EQ costs nothing and changes nothing.
//!
//! All state is preallocated; nothing allocates, locks or blocks. Filters
//! run in f64 (low crossovers at 48 kHz need the precision).

use std::f64::consts::PI;

#[derive(Clone, Copy, Debug, Default)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    fn set(&mut self, b: [f64; 3], a: [f64; 3]) {
        self.b0 = b[0] / a[0];
        self.b1 = b[1] / a[0];
        self.b2 = b[2] / a[0];
        self.a1 = a[1] / a[0];
        self.a2 = a[2] / a[0];
    }

    pub fn set_normalized(&mut self, c: patch_tune::BiquadCoefficients) {
        self.b0 = c.b0;
        self.b1 = c.b1;
        self.b2 = c.b2;
        self.a1 = c.a1;
        self.a2 = c.a2;
    }

    /// RBJ cookbook low-pass.
    pub fn lowpass(&mut self, f: f64, q: f64, fs: f64) {
        let w = 2.0 * PI * (f / fs).min(0.49);
        let (c, al) = (w.cos(), w.sin() / (2.0 * q));
        self.set(
            [(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0],
            [1.0 + al, -2.0 * c, 1.0 - al],
        );
    }

    /// RBJ cookbook high-pass.
    pub fn highpass(&mut self, f: f64, q: f64, fs: f64) {
        let w = 2.0 * PI * (f / fs).min(0.49);
        let (c, al) = (w.cos(), w.sin() / (2.0 * q));
        self.set(
            [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0],
            [1.0 + al, -2.0 * c, 1.0 - al],
        );
    }

    /// Transposed direct form II.
    #[inline]
    pub fn run(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    /// The state holds numbers (no NaN or infinity got in).
    #[inline]
    fn finite(&self) -> bool {
        (self.z1 + self.z2).is_finite()
    }
}

const BUTTERWORTH_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// 4th-order Linkwitz–Riley split: (low, high) at one frequency.
#[derive(Clone, Copy, Debug, Default)]
struct Lr4 {
    lp: [Biquad; 2],
    hp: [Biquad; 2],
}

impl Lr4 {
    fn design(&mut self, f: f64, fs: f64) {
        for b in &mut self.lp {
            b.lowpass(f, BUTTERWORTH_Q, fs);
        }
        for b in &mut self.hp {
            b.highpass(f, BUTTERWORTH_Q, fs);
        }
    }

    #[inline]
    fn split(&mut self, x: f64) -> (f64, f64) {
        let lo = self.lp[0].run(x);
        let lo = self.lp[1].run(lo);
        let hi = self.hp[0].run(x);
        let hi = self.hp[1].run(hi);
        (lo, hi)
    }

    fn reset(&mut self) {
        for b in self.lp.iter_mut().chain(&mut self.hp) {
            b.reset();
        }
    }

    fn finite(&self) -> bool {
        self.lp.iter().chain(&self.hp).all(Biquad::finite)
    }
}

pub const LOW_HZ: f64 = 250.0;
pub const HIGH_HZ: f64 = 2500.0;
/// Coefficient update interval of the filter sweep.
const SUB: usize = 64;

/// EQ and filter settings of a channel: band gains (linear; 0 = kill) and
/// the filter knob (−1 low-pass … 0 off … +1 high-pass).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub low: f32,
    pub mid: f32,
    pub high: f32,
    pub filter: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            low: 1.0,
            mid: 1.0,
            high: 1.0,
            filter: 0.0,
        }
    }
}

impl Settings {
    pub fn eq_flat(&self) -> bool {
        self.low == 1.0 && self.mid == 1.0 && self.high == 1.0
    }

    pub fn filter_off(&self) -> bool {
        self.filter.abs() < 0.02
    }
}

/// Filter knob → (is high-pass, cutoff Hz). Octave-scaled sweep.
fn filter_cutoff(k: f64) -> (bool, f64) {
    if k < 0.0 {
        // 20 kHz … 60 Hz
        (false, 20_000.0 * (60.0f64 / 20_000.0).powf(-k))
    } else {
        // 20 Hz … 8 kHz
        (true, 20.0 * (8_000.0f64 / 20.0).powf(k))
    }
}

/// One mono lane of EQ + filter.
#[derive(Clone, Copy, Debug, Default)]
pub struct Lane {
    x1: Lr4,
    x2: Lr4,
    /// Phase match for the low band (same LP+HP pair as the upper split).
    x2_low: Lr4,
    filt: [Biquad; 2],
    /// Gains reached at the end of the last block.
    gain: [f64; 3],
    /// Filter knob reached (smoothed).
    knob: f64,
    /// The knob value the filter coefficients were designed for (NaN: the
    /// rate changed since, so they must be designed again).
    designed: f64,
    fs: f64,
    active: bool,
    filter_on: bool,
    /// Resonance compensation reached (linear): half the filter's bump in
    /// level, so a sweep adds at most +0.9 dB and the passband loses at
    /// most 0.9 dB (it used to add up to +3.9 dB).
    comp: f64,
}

/// Peak magnitude of a 2nd-order resonant low/high-pass with quality `q`.
fn resonance_peak(q: f64) -> f64 {
    if q <= std::f64::consts::FRAC_1_SQRT_2 {
        1.0
    } else {
        q / (1.0 - 1.0 / (4.0 * q * q)).sqrt()
    }
}

/// Resonance of the second filter stage at knob `k` (Q 0.75 … 1.1).
fn filter_q(k: f64) -> f64 {
    0.75 + 0.35 * k.abs()
}

/// Output gain taking half the resonant bump away (in dB), eased in over
/// the first tenth of the knob so engaging the filter never steps.
fn resonance_comp(k: f64) -> f64 {
    let full = 1.0 / resonance_peak(filter_q(k)).sqrt();
    1.0 + (full - 1.0) * (k.abs() / 0.1).min(1.0)
}

impl Lane {
    fn design(&mut self, fs: f64) {
        self.fs = fs;
        self.designed = f64::NAN;
        self.x1.design(LOW_HZ, fs);
        self.x2.design(HIGH_HZ, fs);
        self.x2_low.design(HIGH_HZ, fs);
    }

    fn reset(&mut self) {
        self.x1.reset();
        self.x2.reset();
        self.x2_low.reset();
        for f in &mut self.filt {
            f.reset();
        }
    }

    fn set_filter(&mut self, k: f64) {
        let (high, f) = filter_cutoff(k);
        // Resonance rises with the sweep, as on club mixers (Q 0.75 … 1.1).
        let q = filter_q(k);
        for (i, b) in self.filt.iter_mut().enumerate() {
            // Two stages: 4th order; only the second resonates.
            let qq = if i == 0 { BUTTERWORTH_Q } else { q };
            if high {
                b.highpass(f, qq, self.fs);
            } else {
                b.lowpass(f, qq, self.fs);
            }
        }
    }

    /// Process `x` in place towards `s`. Returns false when it bypassed.
    pub fn process(&mut self, x: &mut [f32], s: &Settings, fs: f64) -> bool {
        if fs != self.fs {
            self.design(fs);
            self.reset();
        }
        let target = [s.low as f64, s.mid as f64, s.high as f64];
        let flat = s.eq_flat() && s.filter_off();
        let settled = self.gain == [1.0; 3] && self.knob.abs() < 0.02;
        if flat && (settled || !self.active) {
            // Nothing to do: bit-exact passthrough; start clean next time.
            if self.active {
                self.reset();
            }
            self.active = false;
            self.gain = [1.0; 3];
            self.knob = 0.0;
            self.filter_on = false;
            return false;
        }
        if !self.active {
            self.active = true;
            self.gain = [1.0; 3];
            self.knob = 0.0;
        }
        let n = x.len().max(1) as f64;
        let dg = [
            (target[0] - self.gain[0]) / n,
            (target[1] - self.gain[1]) / n,
            (target[2] - self.gain[2]) / n,
        ];
        let mut g = self.gain;
        let want_knob = if s.filter_off() { 0.0 } else { s.filter as f64 };
        for chunk in x.chunks_mut(SUB) {
            // Glide the filter knob (≈ 20 ms to settle) and redesign.
            self.knob += (want_knob - self.knob) * 0.35;
            if (self.knob - want_knob).abs() < 1e-4 {
                self.knob = want_knob;
            }
            let on = self.knob.abs() >= 0.005;
            if on {
                if !self.filter_on {
                    for f in &mut self.filt {
                        f.reset();
                    }
                }
                // Redesign only when the knob moved: a settled knob's
                // coefficients are already in the filters (designing is
                // four trig calls per chunk otherwise).
                if self.designed.to_bits() != self.knob.to_bits() {
                    self.set_filter(self.knob);
                    self.designed = self.knob;
                }
            }
            self.filter_on = on;
            let comp_to = if on { resonance_comp(self.knob) } else { 1.0 };
            if self.comp == 0.0 {
                self.comp = 1.0;
            }
            let dc = (comp_to - self.comp) / chunk.len().max(1) as f64;
            for v in chunk.iter_mut() {
                let mut y = *v as f64;
                let (lo, rest) = self.x1.split(y);
                let (mid, hi) = self.x2.split(rest);
                let (a, b) = self.x2_low.split(lo);
                let lo = a + b;
                g[0] += dg[0];
                g[1] += dg[1];
                g[2] += dg[2];
                y = lo * g[0] + mid * g[1] + hi * g[2];
                self.comp += dc;
                if on {
                    let first = self.filt[0].run(y);
                    y = self.filt[1].run(first) * self.comp;
                }
                *v = y as f32;
            }
        }
        // A NaN or infinity that came in stays in a filter's state for good
        // (the channel would be lost until reloaded): start clean instead.
        if !(self.x1.finite()
            && self.x2.finite()
            && self.x2_low.finite()
            && self.filt.iter().all(Biquad::finite))
        {
            self.reset();
        }
        self.gain = target;
        true
    }
}

/// dB (−∞ as kill) → linear band gain, with the DJ-mixer range.
pub fn band_gain(db: f32) -> f32 {
    if !db.is_finite() || db <= -60.0 {
        0.0
    } else {
        10f32.powf(db.min(6.0) / 20.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;

    fn sine(f: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * PI * f * i as f64 / FS).sin() as f32 * 0.5)
            .collect()
    }

    /// Level (dB) of the steady second half after processing.
    fn level(lane: &mut Lane, s: &Settings, f: f64) -> f64 {
        let mut x = sine(f, 48_000);
        for block in x.chunks_mut(512) {
            lane.process(block, s, FS);
        }
        let tail = &x[24_000..];
        let rms =
            (tail.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / tail.len() as f64).sqrt();
        20.0 * (rms / (0.5 / 2f64.sqrt())).log10()
    }

    #[test]
    fn a_nan_does_not_stay_in_the_eq() {
        let mut lane = Lane::default();
        let s = Settings { low: 0.5, ..Settings::default() };
        let mut x = sine(1000.0, 512);
        x[100] = f32::NAN;
        lane.process(&mut x, &s, FS);
        let mut x = sine(1000.0, 512);
        lane.process(&mut x, &s, FS);
        assert!(x.iter().all(|v| v.is_finite()), "the next block plays again");
    }

    #[test]
    fn flat_is_bit_exact_bypass() {
        let mut lane = Lane::default();
        let x0 = sine(1000.0, 4096);
        let mut x = x0.clone();
        assert!(!lane.process(&mut x, &Settings::default(), FS));
        assert_eq!(x, x0);
    }

    #[test]
    fn bands_sum_flat() {
        // All bands at unity but "active" (a hair off 1.0 to force the path):
        // Linkwitz–Riley sums to an all-pass, so magnitude stays flat.
        let s = Settings {
            low: 1.0,
            mid: 0.999_999,
            high: 1.0,
            filter: 0.0,
        };
        for f in [40.0, 250.0, 800.0, 2500.0, 8000.0, 16000.0] {
            let db = level(&mut Lane::default(), &s, f);
            assert!(db.abs() < 0.05, "{f} Hz: {db:.3} dB");
        }
    }

    #[test]
    fn kills_isolate() {
        let kill_low = Settings {
            low: 0.0,
            ..Default::default()
        };
        assert!(
            level(&mut Lane::default(), &kill_low, 50.0) < -40.0,
            "low killed"
        );
        assert!(
            level(&mut Lane::default(), &kill_low, 6000.0).abs() < 0.5,
            "highs untouched"
        );
        let kill_high = Settings {
            high: 0.0,
            ..Default::default()
        };
        assert!(level(&mut Lane::default(), &kill_high, 12_000.0) < -40.0);
        let kill_mid = Settings {
            mid: 0.0,
            ..Default::default()
        };
        assert!(level(&mut Lane::default(), &kill_mid, 800.0) < -30.0);
        let all = Settings {
            low: 0.0,
            mid: 0.0,
            high: 0.0,
            filter: 0.0,
        };
        assert!(level(&mut Lane::default(), &all, 800.0) < -120.0, "silence");
    }

    #[test]
    fn boost_is_plus_six() {
        let s = Settings {
            low: band_gain(6.0),
            ..Default::default()
        };
        let db = level(&mut Lane::default(), &s, 50.0);
        assert!((db - 6.0).abs() < 0.3, "{db:.2}");
    }

    #[test]
    fn filter_sweeps() {
        let lp = Settings {
            filter: -0.8,
            ..Default::default()
        };
        assert!(
            level(&mut Lane::default(), &lp, 5000.0) < -30.0,
            "low-pass cuts highs"
        );
        assert!(
            level(&mut Lane::default(), &lp, 60.0).abs() < 1.5,
            "keeps lows"
        );
        let hp = Settings {
            filter: 0.8,
            ..Default::default()
        };
        assert!(
            level(&mut Lane::default(), &hp, 60.0) < -30.0,
            "high-pass cuts lows"
        );
    }

    #[test]
    fn stays_finite_under_extremes() {
        let mut lane = Lane::default();
        let mut x = sine(30.0, 48_000);
        for (i, block) in x.chunks_mut(256).enumerate() {
            let k = ((i as f32) * 0.37).sin();
            let s = Settings {
                low: 0.0,
                mid: band_gain(6.0),
                high: 0.0,
                filter: k,
            };
            lane.process(block, &s, FS);
        }
        assert!(x.iter().all(|v| v.is_finite() && v.abs() < 4.0));
    }

    #[test]
    fn a_resonant_sweep_never_boosts() {
        // Every knob position, every frequency around its cutoff: the
        // resonance adds at most +1 dB (it was up to +3.9 dB).
        for k in [-0.95, -0.7, -0.4, 0.3, 0.65, 0.95] {
            let s = Settings {
                filter: k as f32,
                ..Default::default()
            };
            let (_, fc) = filter_cutoff(k);
            let mut worst = f64::NEG_INFINITY;
            for i in 0..=16 {
                let f = (fc * 2f64.powf((i as f64 - 8.0) / 8.0)).clamp(25.0, 20_000.0);
                let mut lane = Lane::default();
                worst = worst.max(level(&mut lane, &s, f));
            }
            assert!(worst <= 1.0, "knob {k}: peak gain {worst:.2} dB");
            assert!(
                worst >= -3.0,
                "knob {k}: still passes near its cutoff ({worst:.2} dB)"
            );
        }
    }
}
