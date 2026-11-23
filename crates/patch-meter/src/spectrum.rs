//! Band spectrum analyser: 4096-point FFT (Hann window), power summed into
//! logarithmically spaced bands, calibrated so a full-scale sine inside a band
//! reads 0 dBFS. Summing the energy of every bin in a band (instead of
//! sampling one frequency) is what fractional-octave analysers do, and it keeps
//! wide high bands and narrow low bands on the same footing.

use std::f64::consts::TAU;

pub const FFT_SIZE: usize = 4096;
pub const LOW_HZ: f64 = 25.0;
pub const HIGH_HZ: f64 = 20_000.0;

/// In-place iterative radix-2 FFT. `re`/`im` length must be a power of two.
pub fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -TAU / len as f64;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0, 0.0);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let nr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = nr;
            }
        }
        len <<= 1;
    }
}

/// Band edges (Hz) for `bands` log-spaced bands.
pub fn band_edges(bands: usize) -> Vec<f64> {
    (0..=bands)
        .map(|b| LOW_HZ * (HIGH_HZ / LOW_HZ).powf(b as f64 / bands as f64))
        .collect()
}

pub struct Analyzer {
    window: Vec<f64>,
    window_power: f64,
    re: Vec<f64>,
    im: Vec<f64>,
}

impl Default for Analyzer {
    fn default() -> Self {
        let window: Vec<f64> = (0..FFT_SIZE)
            .map(|i| 0.5 - 0.5 * (TAU * i as f64 / FFT_SIZE as f64).cos())
            .collect();
        Self::with_window(window)
    }
}

impl Analyzer {
    /// Causal display spectrum: the newest sample has full weight. A centred
    /// Hann window makes a transient grow for half a window before peaking
    /// (42.7 ms at 48 kHz). Use its rising half for reactive pictures while
    /// keeping the symmetric window in `Default` for reference measurements.
    /// Frequency bins and power calibration are unchanged; the taper has
    /// wider leakage, so studio analysis should continue to use `Default`.
    pub fn reactive() -> Self {
        let window = (0..FFT_SIZE)
            .map(|i| 0.5 - 0.5 * (std::f64::consts::PI * i as f64 / (FFT_SIZE - 1) as f64).cos())
            .collect();
        Self::with_window(window)
    }

    fn with_window(window: Vec<f64>) -> Self {
        let window_power = window.iter().map(|w| w * w).sum();
        Analyzer {
            window,
            window_power,
            re: vec![0.0; FFT_SIZE],
            im: vec![0.0; FFT_SIZE],
        }
    }

    /// Band levels in dBFS for the latest `FFT_SIZE` mono samples.
    pub fn bands(&mut self, samples: &[f32], rate: u32, bands: usize) -> Vec<f32> {
        let n = FFT_SIZE;
        let samples = &samples[samples.len().saturating_sub(n)..];
        let padding = n - samples.len();
        for i in 0..n {
            let x = i
                .checked_sub(padding)
                .and_then(|i| samples.get(i))
                .copied()
                .unwrap_or(0.0) as f64;
            self.re[i] = x * self.window[i];
            self.im[i] = 0.0;
        }
        fft(&mut self.re, &mut self.im);
        // One-sided power normalised to mean square: a sine of amplitude A
        // gives A²/2 summed over its bins, so full scale (A = 1) → 0 dBFS.
        let norm = 2.0 / (n as f64 * self.window_power);
        let bin_hz = rate as f64 / n as f64;
        let edges = band_edges(bands);
        (0..bands)
            .map(|b| {
                let (lo, hi) = (edges[b], edges[b + 1]);
                let mut k0 = (lo / bin_hz).ceil() as usize;
                let mut k1 = (hi / bin_hz).floor() as usize;
                if k1 < k0 {
                    // Band narrower than a bin: use the bin nearest its centre.
                    k0 = ((lo * hi).sqrt() / bin_hz).round() as usize;
                    k1 = k0;
                }
                let k1 = k1.min(n / 2 - 1);
                let p: f64 = (k0.max(1)..=k1)
                    .map(|k| (self.re[k] * self.re[k] + self.im[k] * self.im[k]) * norm)
                    .sum();
                let ms = p; // mean square contributed by this band
                if ms > 1e-14 {
                    (10.0 * (2.0 * ms).log10()) as f32
                } else {
                    -140.0
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f64, amp: f64, rate: f64) -> Vec<f32> {
        (0..FFT_SIZE)
            .map(|i| (amp * (TAU * freq * i as f64 / rate).sin()) as f32)
            .collect()
    }

    #[test]
    fn full_scale_sine_reads_zero_dbfs_in_its_band() {
        let mut a = Analyzer::default();
        for f in [100.0, 1000.0, 9000.0] {
            let bands = a.bands(&tone(f, 1.0, 48_000.0), 48_000, 32);
            let peak = bands.iter().cloned().fold(f32::MIN, f32::max);
            assert!((peak - 0.0).abs() < 0.6, "{f} Hz: {peak}");
        }
    }

    #[test]
    fn minus_20_dbfs_sine_reads_minus_20() {
        let mut a = Analyzer::default();
        let bands = a.bands(&tone(1000.0, 0.1, 48_000.0), 48_000, 32);
        let peak = bands.iter().cloned().fold(f32::MIN, f32::max);
        assert!((peak + 20.0).abs() < 0.6, "{peak}");
    }

    #[test]
    fn reactive_transient_peaks_at_the_newest_sample() {
        let mut fast = Analyzer::reactive();
        let mut reference = Analyzer::default();
        let mut samples = vec![0.0; FFT_SIZE];
        let energy = |bands: Vec<f32>| {
            bands
                .into_iter()
                .map(|db| 10f64.powf(db as f64 / 10.0))
                .sum::<f64>()
        };
        let mut reactive_peak = (0, 0.0);
        let mut reference_peak = (0, 0.0);
        for age in (0..FFT_SIZE).step_by(64) {
            samples.fill(0.0);
            samples[FFT_SIZE - 1 - age] = 0.25;
            let a = energy(fast.bands(&samples, 48_000, 64));
            let b = energy(reference.bands(&samples, 48_000, 64));
            if a > reactive_peak.1 {
                reactive_peak = (age, a);
            }
            if b > reference_peak.1 {
                reference_peak = (age, b);
            }
        }
        assert_eq!(reactive_peak.0, 0);
        assert!((1984..=2112).contains(&reference_peak.0));
    }

    #[test]
    fn reactive_partial_history_is_right_aligned() {
        let mut a = Analyzer::reactive();
        let mut full = vec![0.0; FFT_SIZE];
        full[FFT_SIZE - 1] = 0.25;
        assert_eq!(a.bands(&[0.25], 48_000, 64), a.bands(&full, 48_000, 64));
        assert!(a.bands(&[], 48_000, 64).iter().all(|db| *db == -140.0));
    }

    #[test]
    fn fft_matches_a_known_bin() {
        let mut re: Vec<f64> = (0..8).map(|i| (TAU * i as f64 / 8.0).cos()).collect();
        let mut im = vec![0.0; 8];
        fft(&mut re, &mut im);
        assert!((re[1] - 4.0).abs() < 1e-9 && (re[7] - 4.0).abs() < 1e-9);
        assert!(re[0].abs() < 1e-9 && re[2].abs() < 1e-9);
    }
}
