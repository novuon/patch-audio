//! Shared stable IIR filter design and processing primitives.
//!
//! Coefficients use the normalized RBJ Audio EQ Cookbook forms (Robert
//! Bristow-Johnson), calculated in f64 and applied with transposed direct
//! form II. Configuration is validated before it reaches audio processing.
//! Stateful processors are constructed and prepared off the audio thread.

use std::f64::consts::PI;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterType {
    Bell,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
    BandPass,
    Notch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelTarget {
    All,
    Left,
    Right,
    Mid,
    Side,
    Channel(u16),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    pub enabled: bool,
    pub kind: FilterType,
    pub frequency_hz: f64,
    pub gain_db: f64,
    pub q: f64,
    pub target: ChannelTarget,
}

impl Band {
    pub fn validate(self, sample_rate: f64) -> Result<Self, &'static str> {
        if !sample_rate.is_finite() || sample_rate < 8_000.0 {
            return Err("sample rate must be at least 8000 Hz");
        }
        if !self.frequency_hz.is_finite()
            || self.frequency_hz < 1.0
            || self.frequency_hz >= sample_rate * 0.49
        {
            return Err("frequency must be between 1 Hz and 0.49 × sample rate");
        }
        if !self.gain_db.is_finite() || self.gain_db.abs() > 36.0 {
            return Err("gain must be within ±36 dB");
        }
        if !self.q.is_finite() || !(0.1..=18.0).contains(&self.q) {
            return Err("Q must be between 0.1 and 18");
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Coefficients {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

/// RBJ Cookbook coefficients, normalized by a0. Shelves use the standard
/// slope=1 form; Q is used for bell and second-order filter shapes.
pub fn design(band: Band, sample_rate: f64) -> Result<Coefficients, &'static str> {
    let b = band.validate(sample_rate)?;
    let w = 2.0 * PI * b.frequency_hz / sample_rate;
    let (sn, cs) = w.sin_cos();
    let a = 10.0_f64.powf(b.gain_db / 40.0);
    let alpha = sn / (2.0 * b.q);
    let (bb0, bb1, bb2, aa0, aa1, aa2) = match b.kind {
        FilterType::Bell => (
            1.0 + alpha * a,
            -2.0 * cs,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cs,
            1.0 - alpha / a,
        ),
        FilterType::LowPass => (
            (1.0 - cs) / 2.0,
            1.0 - cs,
            (1.0 - cs) / 2.0,
            1.0 + alpha,
            -2.0 * cs,
            1.0 - alpha,
        ),
        FilterType::HighPass => (
            (1.0 + cs) / 2.0,
            -(1.0 + cs),
            (1.0 + cs) / 2.0,
            1.0 + alpha,
            -2.0 * cs,
            1.0 - alpha,
        ),
        FilterType::BandPass => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cs, 1.0 - alpha),
        FilterType::Notch => (1.0, -2.0 * cs, 1.0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha),
        FilterType::LowShelf | FilterType::HighShelf => {
            let shelf_alpha = sn / 2.0 * 2.0_f64.sqrt();
            let beta = 2.0 * a.sqrt() * shelf_alpha;
            let (b0, b1, b2, a0, a1, a2) = if b.kind == FilterType::LowShelf {
                (
                    a * ((a + 1.0) - (a - 1.0) * cs + beta),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cs),
                    a * ((a + 1.0) - (a - 1.0) * cs - beta),
                    (a + 1.0) + (a - 1.0) * cs + beta,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cs),
                    (a + 1.0) + (a - 1.0) * cs - beta,
                )
            } else {
                (
                    a * ((a + 1.0) + (a - 1.0) * cs + beta),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cs),
                    a * ((a + 1.0) + (a - 1.0) * cs - beta),
                    (a + 1.0) - (a - 1.0) * cs + beta,
                    2.0 * ((a - 1.0) - (a + 1.0) * cs),
                    (a + 1.0) - (a - 1.0) * cs - beta,
                )
            };
            (b0, b1, b2, a0, a1, a2)
        }
    };
    let c = Coefficients {
        b0: bb0 / aa0,
        b1: bb1 / aa0,
        b2: bb2 / aa0,
        a1: aa1 / aa0,
        a2: aa2 / aa0,
    };
    if [c.b0, c.b1, c.b2, c.a1, c.a2]
        .iter()
        .any(|x| !x.is_finite())
        || c.a2.abs() >= 1.0
    {
        return Err("unstable filter coefficients");
    }
    Ok(c)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Biquad {
    c: Coefficients,
    z1: f64,
    z2: f64,
}

impl Biquad {
    pub fn set(&mut self, c: Coefficients) {
        self.c = c;
    }
    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let y = self.c.b0 * x + self.z1;
        self.z1 = self.c.b1 * x - self.c.a1 * y + self.z2;
        self.z2 = self.c.b2 * x - self.c.a2 * y;
        if y.is_finite() {
            y
        } else {
            self.reset();
            0.0
        }
    }
    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

/// A single-channel prepared cascade. Build/update this outside the callback;
/// process performs no allocation, locking, logging, or I/O.
pub struct Cascade {
    sections: Vec<Biquad>,
    target: Vec<Coefficients>,
    sample_rate: f64,
}
impl Cascade {
    pub const MAX_BANDS: usize = 64;
    pub fn new(sample_rate: f64, bands: &[Band]) -> Result<Self, &'static str> {
        if bands.len() > Self::MAX_BANDS {
            return Err("too many EQ bands (maximum 64)");
        }
        let target: Vec<_> = bands
            .iter()
            .filter(|b| b.enabled)
            .map(|b| design(*b, sample_rate))
            .collect::<Result<_, _>>()?;
        let sections = target
            .iter()
            .copied()
            .map(|c| {
                let mut s = Biquad::default();
                s.set(c);
                s
            })
            .collect();
        Ok(Self {
            sections,
            target,
            sample_rate,
        })
    }
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }
    pub fn latency_frames(&self) -> usize {
        0
    }
    pub fn update(&mut self, bands: &[Band], sample_rate: f64) -> Result<(), &'static str> {
        if bands.len() > Self::MAX_BANDS {
            return Err("too many EQ bands (maximum 64)");
        }
        let next: Vec<_> = bands
            .iter()
            .filter(|b| b.enabled)
            .map(|b| design(*b, sample_rate))
            .collect::<Result<_, _>>()?;
        // Preserve filter state when topology is stable; topology swaps are
        // prepared by the caller at a graph boundary.
        if next.len() != self.sections.len() || sample_rate != self.sample_rate {
            self.sections = next
                .iter()
                .copied()
                .map(|c| {
                    let mut s = Biquad::default();
                    s.set(c);
                    s
                })
                .collect();
        } else {
            for (s, c) in self.sections.iter_mut().zip(&next) {
                s.set(*c);
            }
        }
        self.target = next;
        self.sample_rate = sample_rate;
        Ok(())
    }
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.sections.iter_mut().fold(x as f64, |v, s| s.process(v)) as f32
    }
    pub fn reset(&mut self) {
        for s in &mut self.sections {
            s.reset();
        }
    }
    pub fn band_count(&self) -> usize {
        self.target.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const FS: f64 = 48_000.0;
    fn band(kind: FilterType, f: f64, g: f64, q: f64) -> Band {
        Band {
            enabled: true,
            kind,
            frequency_hz: f,
            gain_db: g,
            q,
            target: ChannelTarget::All,
        }
    }
    #[test]
    fn validates_frequency_and_stability_across_rates() {
        for fs in [44_100.0, 48_000.0, 88_200.0, 96_000.0, 192_000.0] {
            for kind in [
                FilterType::Bell,
                FilterType::LowShelf,
                FilterType::HighShelf,
                FilterType::LowPass,
                FilterType::HighPass,
                FilterType::BandPass,
                FilterType::Notch,
            ] {
                assert!(design(band(kind, fs * 0.2, 18.0, 18.0), fs).is_ok());
            }
        }
        assert!(design(band(FilterType::Bell, 24_000.0, 0.0, 1.0), FS).is_err());
    }
    #[test]
    fn bell_boost_is_measured_at_center_frequency() {
        let c = design(band(FilterType::Bell, 1000.0, 6.0, 1.0), FS).unwrap();
        let w = 2.0 * PI * 1000.0 / FS;
        let z = (w.cos(), -w.sin());
        let eval = |b0: f64, b1: f64, b2: f64| {
            let r = b0 + b1 * z.0 + b2 * (2.0 * w).cos();
            let i = b1 * z.1 + b2 * (-(2.0 * w).sin());
            r.hypot(i)
        };
        let mag = eval(c.b0, c.b1, c.b2) / eval(1.0, c.a1, c.a2);
        assert!((20.0 * mag.log10() - 6.0).abs() < 0.01);
    }
    #[test]
    fn prepared_cascade_is_finite_and_zero_latency() {
        let mut e = Cascade::new(FS, &[band(FilterType::Notch, 120.0, 0.0, 12.0)]).unwrap();
        assert_eq!(e.latency_frames(), 0);
        for i in 0..100_000 {
            assert!(e.process(if i == 0 { 1.0 } else { 0.0 }).is_finite());
        }
    }
}
