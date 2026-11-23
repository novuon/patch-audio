//! Surround and mono outputs: the program upmixed the way PipeWire upmixes
//! a stereo app for such a device (its default `psd` method; channelmix-ops
//! in PipeWire 1.6), so a 5.1 system or a MacBook's six speakers sound with
//! Patch as they do without it.
//!
//! * FC: (L + R) · √½, Linkwitz-Riley low-pass at 12 kHz.
//! * LFE: (L + R) · ½, Linkwitz-Riley low-pass at 150 Hz.
//! * A surround pair PipeWire decodes (rears of 4.0 and 7.1, the pair of
//!   5.1): L − R delayed 12 ms, · √½ on the left, · −√½ on the right.
//! * Any other pair: L · √½, R · √½ (PipeWire's matrix). A MacBook's six
//!   speakers (front, side, rear) get this; PipeWire itself takes that
//!   layout for 5.1 and puts the centre on one side speaker and nothing on
//!   the other, which isn't followed here.
//! * Mono: (L + R) · ½.
//!
//! Computed only for the outputs that are linked; no allocation.

/// Which pairs carry the decoded surround (else the √½ copies).
pub const SIDE_PSD: u8 = 1;
pub const REAR_PSD: u8 = 2;

const SQRT1_2: f32 = std::f32::consts::FRAC_1_SQRT_2;
/// 12 ms at up to 192 kHz fits (2304 frames).
const DELAY_LEN: usize = 4096;

#[derive(Clone, Copy, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    /// Butterworth low-pass (Q = √½) at `f` Hz; a pass-through above
    /// 0.45 · rate (PipeWire leaves such a filter out too).
    fn lowpass(f: f32, rate: f32) -> Biquad {
        if f >= rate * 0.45 {
            return Biquad {
                b0: 1.0,
                ..Default::default()
            };
        }
        let w = std::f32::consts::TAU * f / rate;
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * SQRT1_2);
        let a0 = 1.0 + alpha;
        let b0 = (1.0 - c) / 2.0 / a0;
        Biquad {
            b0,
            b1: (1.0 - c) / a0,
            b2: b0,
            a1: -2.0 * c / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Linkwitz-Riley 4th order: two Butterworth sections.
#[derive(Clone, Copy, Default)]
struct Lr4([Biquad; 2]);

impl Lr4 {
    fn lowpass(f: f32, rate: f32) -> Lr4 {
        let b = Biquad::lowpass(f, rate);
        Lr4([b, b])
    }

    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        let y = self.0[0].run(x);
        self.0[1].run(y)
    }
}

/// The extra outputs of one cycle (None: not linked).
#[derive(Default)]
pub struct Outs<'a> {
    pub fc: Option<&'a mut [f32]>,
    pub lfe: Option<&'a mut [f32]>,
    pub sl: Option<&'a mut [f32]>,
    pub sr: Option<&'a mut [f32]>,
    pub rl: Option<&'a mut [f32]>,
    pub rr: Option<&'a mut [f32]>,
    pub mono: Option<&'a mut [f32]>,
}

pub struct Surround {
    rate: u32,
    fc: Lr4,
    lfe: Lr4,
    /// L − R, for the decoded pairs.
    delay: Box<[f32]>,
    pos: usize,
}

impl Default for Surround {
    fn default() -> Self {
        Surround {
            rate: 0,
            fc: Lr4::default(),
            lfe: Lr4::default(),
            delay: vec![0.0; DELAY_LEN].into_boxed_slice(),
            pos: 0,
        }
    }
}

impl Surround {
    fn set_rate(&mut self, rate: u32) {
        if rate != self.rate && rate > 0 {
            self.rate = rate;
            self.fc = Lr4::lowpass(12_000.0, rate as f32);
            self.lfe = Lr4::lowpass(150.0, rate as f32);
            self.delay.fill(0.0);
        }
    }

    /// Fill the linked outputs from the program `l`/`r` (`psd`: which
    /// pairs are decoded, SIDE_PSD | REAR_PSD).
    pub fn run(&mut self, rate: u32, l: &[f32], r: &[f32], mut o: Outs, psd: u8) {
        self.set_rate(rate);
        let n = l.len().min(r.len());
        if let Some(fc) = o.fc.as_deref_mut() {
            for i in 0..n.min(fc.len()) {
                fc[i] = self.fc.run(l[i] + r[i]) * SQRT1_2;
            }
        }
        if let Some(lfe) = o.lfe.as_deref_mut() {
            for i in 0..n.min(lfe.len()) {
                lfe[i] = self.lfe.run(l[i] + r[i]) * 0.5;
            }
        }
        if let Some(m) = o.mono.as_deref_mut() {
            for i in 0..n.min(m.len()) {
                m[i] = (l[i] + r[i]) * 0.5;
            }
        }
        // The decoded pairs read L − R from 12 ms ago; the delay line runs
        // whenever one is linked.
        let delay = (self.rate as usize * 12 / 1000).min(DELAY_LEN - 1);
        let decoded = (psd & SIDE_PSD != 0 && (o.sl.is_some() || o.sr.is_some()))
            || (psd & REAR_PSD != 0 && (o.rl.is_some() || o.rr.is_some()));
        for (bit, (a, b)) in [
            (SIDE_PSD, (&mut o.sl, &mut o.sr)),
            (REAR_PSD, (&mut o.rl, &mut o.rr)),
        ] {
            if psd & bit == 0 {
                if let Some(a) = a.as_deref_mut() {
                    for i in 0..n.min(a.len()) {
                        a[i] = l[i] * SQRT1_2;
                    }
                }
                if let Some(b) = b.as_deref_mut() {
                    for i in 0..n.min(b.len()) {
                        b[i] = r[i] * SQRT1_2;
                    }
                }
            }
        }
        if decoded {
            let mask = DELAY_LEN - 1;
            for i in 0..n {
                self.delay[self.pos] = l[i] - r[i];
                let d = self.delay[(self.pos + DELAY_LEN - delay) & mask];
                self.pos = (self.pos + 1) & mask;
                for (bit, a, b) in [
                    (SIDE_PSD, &mut o.sl, &mut o.sr),
                    (REAR_PSD, &mut o.rl, &mut o.rr),
                ] {
                    if psd & bit != 0 {
                        if let Some(a) = a.as_deref_mut().filter(|a| i < a.len()) {
                            a[i] = d * SQRT1_2;
                        }
                        if let Some(b) = b.as_deref_mut().filter(|b| i < b.len()) {
                            b[i] = -d * SQRT1_2;
                        }
                    }
                }
            }
        }
    }
}

/// Which pairs PipeWire decodes for a device with these playback
/// positions (its special 4.0, 5.1 and 7.1 layouts; everything else gets
/// the √½ copies).
pub fn psd_for(positions: &[&str]) -> u8 {
    let mut set: Vec<&str> = positions
        .iter()
        .copied()
        .filter(|p| ["FL", "FR", "FC", "LFE", "SL", "SR", "RL", "RR"].contains(p))
        .collect();
    set.sort_unstable();
    set.dedup();
    let is = |want: &[&str]| {
        let mut w = want.to_vec();
        w.sort_unstable();
        set == w && positions.len() == want.len()
    };
    if is(&["FL", "FR", "RL", "RR"])
        || is(&["FL", "FR", "FC", "LFE", "RL", "RR"])
        || is(&["FL", "FR", "FC", "LFE", "SL", "SR", "RL", "RR"])
    {
        REAR_PSD
    } else if is(&["FL", "FR", "FC", "LFE", "SL", "SR"]) {
        SIDE_PSD
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_decode_as_pipewire_does() {
        assert_eq!(psd_for(&["FL", "FR", "RL", "RR"]), REAR_PSD, "4.0");
        assert_eq!(
            psd_for(&["FL", "FR", "FC", "LFE", "SL", "SR"]),
            SIDE_PSD,
            "5.1 sides"
        );
        assert_eq!(
            psd_for(&["FL", "FR", "FC", "LFE", "RL", "RR"]),
            REAR_PSD,
            "5.1 rears"
        );
        assert_eq!(
            psd_for(&["FL", "FR", "FC", "LFE", "SL", "SR", "RL", "RR"]),
            REAR_PSD,
            "7.1"
        );
        assert_eq!(
            psd_for(&["FL", "FR", "SL", "SR", "RL", "RR"]),
            0,
            "a MacBook's speakers: copies"
        );
        assert_eq!(psd_for(&["FL", "FR"]), 0);
    }

    #[test]
    fn levels_filters_and_the_decoded_delay() {
        let rate = 48_000;
        let mut s = Surround::default();
        let n = 4096;
        // Both sides the same tone: L − R = 0, the decoded pair silent.
        let tone: Vec<f32> = (0..n).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let (mut fc, mut lfe, mut mono, mut rl, mut rr) = (
            vec![0.0; n],
            vec![0.0; n],
            vec![0.0; n],
            vec![0.0; n],
            vec![0.0; n],
        );
        s.run(
            rate,
            &tone,
            &tone,
            Outs {
                fc: Some(&mut fc),
                lfe: Some(&mut lfe),
                mono: Some(&mut mono),
                rl: Some(&mut rl),
                rr: Some(&mut rr),
                ..Default::default()
            },
            REAR_PSD,
        );
        assert!(
            mono.iter().zip(&tone).all(|(m, t)| (m - t).abs() < 1e-6),
            "mono = (L+R)/2"
        );
        assert!(
            rl.iter().chain(&rr).all(|x| x.abs() < 1e-6),
            "no difference, no surround"
        );
        // A ~380 Hz tone: FC passes it (√½ · 2 = √2 times the input), the
        // 150 Hz LFE takes most of it out.
        let peak = |v: &[f32]| v[n / 2..].iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(
            (peak(&fc) - 0.5 * 2.0 * SQRT1_2).abs() < 0.02,
            "fc {}",
            peak(&fc)
        );
        assert!(peak(&lfe) < 0.5 * 0.3, "lfe {}", peak(&lfe));
        // An impulse on L alone: the decoded pair 12 ms later, ±√½.
        let mut s = Surround::default();
        let mut l = vec![0.0; n];
        l[10] = 1.0;
        let r = vec![0.0; n];
        let (mut rl, mut rr) = (vec![0.0; n], vec![0.0; n]);
        s.run(
            rate,
            &l,
            &r,
            Outs {
                rl: Some(&mut rl),
                rr: Some(&mut rr),
                ..Default::default()
            },
            REAR_PSD,
        );
        assert!((rl[10 + 576] - SQRT1_2).abs() < 1e-6 && (rr[10 + 576] + SQRT1_2).abs() < 1e-6);
        assert!(rl[..586].iter().all(|x| *x == 0.0));
        // Copies otherwise: L · √½.
        let mut s = Surround::default();
        let (mut sl, mut sr) = (vec![0.0; n], vec![0.0; n]);
        s.run(
            rate,
            &l,
            &r,
            Outs {
                sl: Some(&mut sl),
                sr: Some(&mut sr),
                ..Default::default()
            },
            REAR_PSD,
        );
        assert!((sl[10] - SQRT1_2).abs() < 1e-6 && sr[10] == 0.0);
    }
}
