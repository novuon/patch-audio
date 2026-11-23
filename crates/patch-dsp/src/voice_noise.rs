//! Low-latency filter-bank noise attenuation. This is a deterministic
//! four-band downward expander, not neural or adaptive noise cancellation.
//! Threshold describes the peak floor per band; calibration remains external.
use crate::filter::{Band, Biquad, ChannelTarget, FilterType, design};

pub struct NoiseSuppressor {
    filters: [[Biquad; 3]; 2],
    envelopes: [f32; 4],
    gains: [f32; 4],
    rate: u32,
    attack: f32,
    release: f32,
    smooth: f32,
}
impl Default for NoiseSuppressor {
    fn default() -> Self {
        Self {
            filters: [[Biquad::default(); 3]; 2],
            envelopes: [0.0; 4],
            gains: [1.0; 4],
            rate: 0,
            attack: 0.0,
            release: 0.0,
            smooth: 0.0,
        }
    }
}
impl NoiseSuppressor {
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32], rate: u32, threshold_db: f32) {
        let rate = rate.max(8000);
        if self.rate != rate {
            self.rate = rate;
            self.envelopes = [0.0; 4];
            self.gains = [1.0; 4];
            for side in &mut self.filters {
                for (filter, frequency) in side.iter_mut().zip([250.0, 1000.0, 3500.0]) {
                    filter.reset();
                    filter.set(
                        design(
                            Band {
                                enabled: true,
                                kind: FilterType::LowPass,
                                frequency_hz: frequency,
                                gain_db: 0.0,
                                q: std::f64::consts::FRAC_1_SQRT_2,
                                target: ChannelTarget::All,
                            },
                            rate as f64,
                        )
                        .expect("fixed filter frequencies are below Nyquist"),
                    );
                }
            }
            let k = |t: f32| 1.0 - (-1.0 / (t * rate as f32)).exp();
            self.attack = k(0.001);
            self.release = k(0.080);
            self.smooth = k(0.003);
        }
        let threshold = 10.0_f32.powf(threshold_db / 20.0);
        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
            let split = |filters: &mut [Biquad; 3], x: f32| {
                let lp = filters.each_mut().map(|f| f.process(x as f64) as f32);
                [lp[0], lp[1] - lp[0], lp[2] - lp[1], x - lp[2]]
            };
            let left = split(&mut self.filters[0], *a);
            let right = split(&mut self.filters[1], *b);
            *a = 0.0;
            *b = 0.0;
            for i in 0..4 {
                let peak = left[i].abs().max(right[i].abs());
                let k = if peak > self.envelopes[i] {
                    self.attack
                } else {
                    self.release
                };
                self.envelopes[i] += (peak - self.envelopes[i]) * k;
                let want = (self.envelopes[i] / threshold).clamp(0.1, 1.0);
                self.gains[i] += (want - self.gains[i]) * self.smooth;
                *a += left[i] * self.gains[i];
                *b += right[i] * self.gains[i];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attenuates_low_level_room_signal_and_preserves_voice() {
        let mut noise = NoiseSuppressor::default();
        let samples: Vec<f32> = (0..48000)
            .map(|i| 0.0003 * (std::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin())
            .collect();
        let mut l = samples.clone();
        let mut r = l.clone();
        noise.process(&mut l, &mut r, 48000, -50.0);
        assert_eq!(l, r);
        let energy = |x: &[f32]| x.iter().map(|s| s * s).sum::<f32>();
        assert!(energy(&l[24000..]) < energy(&samples[24000..]) * 0.1);
        let input: Vec<f32> = samples.iter().map(|s| s * 1000.0).collect();
        let mut l = input.clone();
        let mut r = l.clone();
        noise.process(&mut l, &mut r, 48000, -50.0);
        assert!((energy(&l[24000..]) / energy(&input[24000..]) - 1.0).abs() < 0.05);
    }
    #[test]
    fn sample_rate_changes_remain_finite() {
        let mut n = NoiseSuppressor::default();
        for rate in [8000, 44100, 48000, 96000, 192000] {
            let mut l = [0.3; 256];
            let mut r = l;
            n.process(&mut l, &mut r, rate, -50.0);
            assert!(l.iter().chain(&r).all(|s| s.is_finite()));
        }
    }
}
