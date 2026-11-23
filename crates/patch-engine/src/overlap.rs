//! Attenuation-only overlap control for manual and AI DJ playback.
//! Measures post-fader/post-FX audio, without allocation or lookahead.
#[derive(Default)]
pub(crate) struct Overlap {
    power: [f64; super::MAX_DECKS],
    sum: f64,
    gain: Option<f32>,
}
impl Overlap {
    pub fn apply(
        &mut self,
        powers: [f64; super::MAX_DECKS],
        l: &mut [f32],
        r: &mut [f32],
        rate: u32,
    ) -> (f32, f32) {
        let n = l.len().min(r.len());
        if n == 0 {
            return (1.0, 1.0);
        }
        let seconds = n as f64 / rate.max(1) as f64;
        let alpha = 1.0 - (-seconds / 0.25).exp();
        for (p, measured) in self.power.iter_mut().zip(powers) {
            *p += alpha * (measured / n as f64 - *p);
        }
        let measured = l
            .iter()
            .zip(r.iter())
            .map(|(&a, &b)| (a as f64).powi(2) + (b as f64).powi(2))
            .sum::<f64>()
            / n as f64;
        self.sum += alpha * (measured - self.sum);
        let total = self.power.iter().sum::<f64>();
        let dominant = self.power.iter().copied().fold(0.0, f64::max);
        // Only overlapping contributions warrant attenuation. Never boost
        // cancellation, silence, a channel fade or a quiet breakdown.
        let overlap = (total - dominant).max(0.0);
        let target = if dominant > 1e-12 && overlap > dominant * 1e-5 {
            (dominant / self.sum.max(dominant)).sqrt().clamp(0.25, 1.0) as f32
        } else {
            1.0
        };
        let from = self.gain.unwrap_or(1.0);
        let tau = if target < from { 0.08 } else { 0.4 };
        let to = from + (target - from) * (1.0 - (-seconds / tau).exp()) as f32;
        let delta = (to - from) / n as f32;
        let mut gain = from;
        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
            *a *= gain;
            *b *= gain;
            gain += delta;
        }
        self.gain = Some(to);
        (from, to)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn settle(a: f32, b: f32, correlated: bool) -> f32 {
        let mut control = Overlap::default();
        let mut peak = 0.0;
        for _ in 0..1200 {
            let mut l = [0.0; 256];
            let mut r = [0.0; 256];
            let mut powers = [0.0; super::super::MAX_DECKS];
            for i in 0..256 {
                let x = if i % 2 == 0 { a } else { -a };
                let y = if correlated {
                    if i % 2 == 0 { b } else { -b }
                } else {
                    if i % 4 < 2 { b } else { -b }
                };
                l[i] = x + y;
                r[i] = x + y;
                powers[0] += 2.0 * (x as f64).powi(2);
                powers[1] += 2.0 * (y as f64).powi(2);
            }
            control.apply(powers, &mut l, &mut r, 48_000);
            peak = (l.iter().map(|x| x * x).sum::<f32>() / 256.0).sqrt();
        }
        peak
    }
    #[test]
    fn solo_and_silence_are_never_boosted_or_attenuated() {
        assert!((settle(0.1, 0.0, false) - 0.1).abs() < 1e-5);
        assert!((settle(0.001, 0.0, false) - 0.001).abs() < 1e-7);
        assert_eq!(settle(0.0, 0.0, false), 0.0);
    }
    #[test]
    fn unrelated_and_correlated_overlaps_match_the_stronger_deck() {
        for correlated in [false, true] {
            for b in [0.1, 0.02] {
                assert!((settle(0.1, b, correlated) - 0.1).abs() < 1e-4);
            }
        }
    }
    #[test]
    fn overlap_entry_and_exit_are_smooth_and_solo_recovers() {
        let mut control = Overlap::default();
        let mut previous = 1.0;
        for block in 0..2400 {
            let overlapping = (600..1200).contains(&block);
            let mut powers = [0.0; super::super::MAX_DECKS];
            powers[0] = 256.0 * 0.02;
            powers[1] = if overlapping { powers[0] } else { 0.0 };
            let mut l = [if overlapping { 0.2 } else { 0.1 }; 256];
            let mut r = l;
            control.apply(powers, &mut l, &mut r, 48_000);
            let gain = control.gain.unwrap();
            assert!((gain - previous).abs() < 0.02, "no gain step: {block}");
            assert!((0.25..=1.0).contains(&gain));
            previous = gain;
        }
        assert!(
            (previous - 1.0).abs() < 1e-5,
            "solo recovers without a permanent dip"
        );
    }
    #[test]
    fn cancellation_is_not_boosted() {
        assert!(settle(0.1, -0.1, true) < 1e-6);
    }
}
