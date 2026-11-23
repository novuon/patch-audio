//! The master EQ (the EQ Unit, docs/eq): eight parametric bands on the
//! program, after the mix and before Tune and the limiter.
//!
//! * Settings are published by the control thread through a seqlock; the
//!   RT thread designs the filters (RBJ, `patch_dsp::filter`, f64) when the
//!   settings or the graph rate change — about a microsecond for 8 bands.
//! * Changes glide: the coefficients are interpolated across the cycle
//!   they arrive in, so dragging a band never clicks or zippers; a band
//!   switched off glides to unity before it stops being processed.
//! * The input gain (a correction's preamp) rides in the first band's
//!   filter, its numerator scaled: no extra pass, and it glides with it.
//! * Zero latency. Nothing to do (bypassed, or every band off or flat):
//!   nothing is processed and the program passes bit-exact, at no cost.
//! * No allocation, no locking, no system calls on the RT thread.

use patch_core::eq::{BANDS, EqBand, EqKind, EqState};
pub use patch_dsp::filter::Coefficients;
use patch_dsp::filter::{self, ChannelTarget, FilterType};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// The settings as the RT thread reads them (lock-free snapshot).
pub struct Published {
    generation: AtomicU64,
    /// Per band: on, kind, freq, gain dB, Q (f32 bits / small ints).
    bands: [[AtomicU32; 5]; BANDS],
    bypass: AtomicBool,
    /// Input gain, dB (f32 bits).
    gain: AtomicU32,
}

impl Default for Published {
    fn default() -> Self {
        let p = Published {
            generation: AtomicU64::new(0),
            bands: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU32::new(0))),
            bypass: AtomicBool::new(false),
            gain: AtomicU32::new(0),
        };
        p.set(&EqState::default());
        p
    }
}

fn kind_code(k: EqKind) -> u32 {
    EqKind::ALL.iter().position(|x| *x == k).unwrap_or(0) as u32
}

impl Published {
    /// Publish new settings (control thread).
    pub fn set(&self, st: &EqState) {
        let st = st.clone().normalized();
        self.generation.fetch_add(1, Ordering::AcqRel); // odd: writing
        for (slot, b) in self.bands.iter().zip(&st.bands) {
            slot[0].store(b.on as u32, Ordering::Relaxed);
            slot[1].store(kind_code(b.kind), Ordering::Relaxed);
            slot[2].store(b.freq.to_bits(), Ordering::Relaxed);
            slot[3].store(b.gain_db.to_bits(), Ordering::Relaxed);
            slot[4].store(b.q.to_bits(), Ordering::Relaxed);
        }
        self.bypass.store(st.bypass, Ordering::Relaxed);
        self.gain.store(st.gain_db.to_bits(), Ordering::Relaxed);
        self.generation.fetch_add(1, Ordering::Release); // even: complete
    }

    /// A consistent snapshot, or None while the control thread writes.
    fn read(&self, seen: u64) -> Option<(u64, bool, f32, [EqBand; BANDS])> {
        let g = self.generation.load(Ordering::Acquire);
        if g == seen || g & 1 == 1 {
            return None;
        }
        let bands = std::array::from_fn(|i| {
            let s = &self.bands[i];
            EqBand {
                on: s[0].load(Ordering::Relaxed) != 0,
                kind: EqKind::ALL
                    [(s[1].load(Ordering::Relaxed) as usize).min(EqKind::ALL.len() - 1)],
                freq: f32::from_bits(s[2].load(Ordering::Relaxed)),
                gain_db: f32::from_bits(s[3].load(Ordering::Relaxed)),
                q: f32::from_bits(s[4].load(Ordering::Relaxed)),
            }
        });
        let bypass = self.bypass.load(Ordering::Relaxed);
        let gain = f32::from_bits(self.gain.load(Ordering::Relaxed));
        (self.generation.load(Ordering::Acquire) == g).then_some((g, bypass, gain, bands))
    }
}

const UNITY: Coefficients = Coefficients {
    b0: 1.0,
    b1: 0.0,
    b2: 0.0,
    a1: 0.0,
    a2: 0.0,
};

fn is_unity(c: &Coefficients) -> bool {
    *c == UNITY
}

/// A band's filter at `rate` (unity when it changes nothing).
pub fn coefficients(b: &EqBand, rate: f64) -> Coefficients {
    if !b.active() {
        return UNITY;
    }
    let kind = match b.kind {
        EqKind::Bell => FilterType::Bell,
        EqKind::LowShelf => FilterType::LowShelf,
        EqKind::HighShelf => FilterType::HighShelf,
        EqKind::LowPass => FilterType::LowPass,
        EqKind::HighPass => FilterType::HighPass,
        EqKind::Notch => FilterType::Notch,
    };
    let band = filter::Band {
        enabled: true,
        kind,
        // Below Nyquist at any rate (a 20 kHz band at 44.1 kHz).
        frequency_hz: (b.freq as f64).clamp(1.0, rate * 0.45),
        gain_db: b.gain_db as f64,
        q: b.q as f64,
        target: ChannelTarget::All,
    };
    filter::design(band, rate).unwrap_or(UNITY)
}

/// The RT side: filter state and the coefficients in use.
pub struct MasterEq {
    seen: u64,
    rate: u32,
    bands: [EqBand; BANDS],
    bypass: bool,
    gain_db: f32,
    cur: [Coefficients; BANDS],
    target: [Coefficients; BANDS],
    /// Transposed direct form II state per band, per side: (z1, z2).
    z: [[[f64; 2]; 2]; BANDS],
    gliding: bool,
    /// Bands to run this cycle (from `prepare`).
    live: [usize; BANDS],
    n: usize,
}

impl Default for MasterEq {
    fn default() -> Self {
        MasterEq {
            seen: 0,
            rate: 0,
            bands: EqState::default()
                .bands
                .try_into()
                .unwrap_or([EqBand::default(); BANDS]),
            bypass: false,
            gain_db: 0.0,
            cur: [UNITY; BANDS],
            target: [UNITY; BANDS],
            z: [[[0.0; 2]; 2]; BANDS],
            gliding: false,
            live: [0; BANDS],
            n: 0,
        }
    }
}

impl MasterEq {
    /// A reused playback lane must carry no prior filter history. Keep the
    /// prepared storage: this reset runs on the real-time callback.
    pub fn reset(&mut self) {
        self.seen = 0;
        self.rate = 0;
        self.cur = [UNITY; BANDS];
        self.target = [UNITY; BANDS];
        self.z = [[[0.0; 2]; 2]; BANDS];
        self.gliding = false;
        self.live = [0; BANDS];
        self.n = 0;
    }

    /// Process the program (both sides) in place.
    pub fn process(&mut self, p: &Published, rate: u32, l: &mut [f32], r: &mut [f32]) {
        if self.prepare(p, rate) {
            self.run_side(0, l);
            self.run_side(1, r);
            self.finish();
        }
    }

    /// Once per cycle, before `run_side`: take new settings (or a new
    /// rate) and find the bands to run. False: nothing to do this cycle
    /// (the audio passes bit-exact; don't call `run_side`).
    pub fn prepare(&mut self, p: &Published, rate: u32) -> bool {
        let rate = rate.max(8_000);
        let fresh = p.read(self.seen);
        if fresh.is_some() || rate != self.rate {
            if let Some((g, bypass, gain, bands)) = fresh {
                self.seen = g;
                self.bypass = bypass;
                self.gain_db = gain;
                self.bands = bands;
            }
            self.rate = rate;
            for i in 0..BANDS {
                self.target[i] = if self.bypass {
                    UNITY
                } else {
                    coefficients(&self.bands[i], rate as f64)
                };
            }
            if !self.bypass && self.gain_db.abs() >= 0.01 {
                let g = 10f64.powf(self.gain_db as f64 / 20.0);
                let c = &mut self.target[0];
                (c.b0, c.b1, c.b2) = (c.b0 * g, c.b1 * g, c.b2 * g);
            }
            self.gliding = self.cur != self.target;
        }
        // Bands in use: whatever is not unity now or where it is heading.
        self.n = 0;
        for i in 0..BANDS {
            if !is_unity(&self.cur[i]) || !is_unity(&self.target[i]) {
                self.live[self.n] = i;
                self.n += 1;
            } else {
                self.z[i] = [[0.0; 2]; 2];
            }
        }
        self.n > 0
    }

    /// One side (0 = left, 1 = right) in place, after `prepare`. A change
    /// glides: the coefficients are interpolated across this cycle.
    pub fn run_side(&mut self, side: usize, x: &mut [f32]) {
        let side = side.min(1);
        if self.gliding {
            let inv = 1.0 / x.len().max(1) as f64;
            for (f, v) in x.iter_mut().enumerate() {
                let t = (f + 1) as f64 * inv;
                let mut y = *v as f64;
                for &i in &self.live[..self.n] {
                    let (a, b) = (&self.cur[i], &self.target[i]);
                    let c = Coefficients {
                        b0: a.b0 + (b.b0 - a.b0) * t,
                        b1: a.b1 + (b.b1 - a.b1) * t,
                        b2: a.b2 + (b.b2 - a.b2) * t,
                        a1: a.a1 + (b.a1 - a.a1) * t,
                        a2: a.a2 + (b.a2 - a.a2) * t,
                    };
                    y = run(&c, &mut self.z[i][side], y);
                }
                *v = y as f32;
            }
        } else {
            // Bands in pairs, software-pipelined: band a on sample n while
            // band b takes sample n − 1, so the two recursions overlap in
            // the CPU instead of waiting on each other. Exact, no latency.
            let live = self.live;
            let mut k = 0;
            while k < self.n {
                let len = x.len();
                if len == 0 {
                    break;
                }
                if k + 1 < self.n {
                    let (ia, ib) = (live[k], live[k + 1]);
                    let (ca, cb) = (self.cur[ia], self.cur[ib]);
                    let (mut za, mut zb) = (self.z[ia][side], self.z[ib][side]);
                    let mut pa = run(&ca, &mut za, x[0] as f64);
                    for f in 1..len {
                        let ya = run(&ca, &mut za, x[f] as f64);
                        x[f - 1] = run(&cb, &mut zb, pa) as f32;
                        pa = ya;
                    }
                    x[len - 1] = run(&cb, &mut zb, pa) as f32;
                    self.z[ia][side] = za;
                    self.z[ib][side] = zb;
                    k += 2;
                } else {
                    let i = live[k];
                    let c = self.cur[i];
                    let mut z = self.z[i][side];
                    for v in x.iter_mut() {
                        *v = run(&c, &mut z, *v as f64) as f32;
                    }
                    self.z[i][side] = z;
                    k += 1;
                }
            }
        }
    }

    /// After both sides: the glide is done; keep the state out of denormals.
    pub fn finish(&mut self) {
        if self.gliding {
            self.cur = self.target;
            self.gliding = false;
        }
        for &i in &self.live[..self.n] {
            for side in &mut self.z[i] {
                for v in side.iter_mut() {
                    if v.abs() < 1e-30 || !v.is_finite() {
                        *v = 0.0;
                    }
                }
            }
        }
    }
}

/// One transposed direct form II step.
#[inline(always)]
fn run(c: &Coefficients, z: &mut [f64; 2], x: f64) -> f64 {
    let y = c.b0 * x + z[0];
    z[0] = c.b1 * x - c.a1 * y + z[1];
    z[1] = c.b2 * x - c.a2 * y;
    y
}

/// Magnitude (dB) of a band's filter at `hz`, for response curves.
pub fn response_db(c: &Coefficients, hz: f64, rate: f64) -> f64 {
    let w = std::f64::consts::TAU * hz / rate;
    let (c1, s1) = (w.cos(), w.sin());
    let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());
    let num = (c.b0 + c.b1 * c1 + c.b2 * c2, -(c.b1 * s1 + c.b2 * s2));
    let den = (1.0 + c.a1 * c1 + c.a2 * c2, -(c.a1 * s1 + c.a2 * s2));
    let m = num.0.hypot(num.1) / den.0.hypot(den.1).max(1e-300);
    20.0 * m.max(1e-12).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| ((std::f64::consts::TAU * f * i as f64 / 48_000.0).sin() * 0.25) as f32)
            .collect()
    }

    fn rms_db(x: &[f32]) -> f64 {
        let t = &x[x.len() / 2..];
        20.0 * ((t.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / t.len() as f64).sqrt()
            / (0.25 / 2f64.sqrt()))
        .log10()
    }

    fn run_eq(st: &EqState, f: f64) -> f64 {
        let p = Published::default();
        p.set(st);
        let mut eq = MasterEq::default();
        let x = sine(f, 48_000);
        let (mut l, mut r) = (x.clone(), x);
        for (bl, br) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
            eq.process(&p, 48_000, bl, br);
        }
        rms_db(&l)
    }

    #[test]
    fn flat_is_bit_exact_and_free() {
        let p = Published::default();
        let mut eq = MasterEq::default();
        let x = sine(1000.0, 4096);
        let (mut l, mut r) = (x.clone(), x.clone());
        eq.process(&p, 48_000, &mut l, &mut r);
        assert_eq!(l, x);
        assert_eq!(r, x);
    }

    #[test]
    fn a_bell_boosts_its_band_by_its_gain() {
        let mut st = EqState::default();
        st.bands[3] = EqBand {
            on: true,
            kind: EqKind::Bell,
            freq: 1000.0,
            gain_db: 6.0,
            q: 1.0,
        };
        assert!((run_eq(&st, 1000.0) - 6.0).abs() < 0.1);
        assert!(run_eq(&st, 100.0).abs() < 0.3, "leaves far bands alone");
        st.bypass = true;
        assert!(run_eq(&st, 1000.0).abs() < 0.01, "bypass");
    }

    #[test]
    fn a_high_pass_removes_the_lows() {
        let mut st = EqState::default();
        st.bands[0] = EqBand {
            on: true,
            kind: EqKind::HighPass,
            freq: 200.0,
            gain_db: 0.0,
            q: 0.71,
        };
        assert!(run_eq(&st, 40.0) < -24.0);
        assert!(run_eq(&st, 2000.0).abs() < 0.2);
    }

    #[test]
    fn changes_glide_without_steps() {
        // A 12 dB jump mid-signal: the output envelope moves smoothly.
        let p = Published::default();
        let mut eq = MasterEq::default();
        let mut st = EqState::default();
        st.bands[3].gain_db = 0.01;
        p.set(&st);
        let x = sine(1000.0, 1024 * 8);
        let (mut l, mut r) = (x.clone(), x.clone());
        for (k, (bl, br)) in l.chunks_mut(1024).zip(r.chunks_mut(1024)).enumerate() {
            if k == 4 {
                st.bands[3].gain_db = 12.0;
                p.set(&st);
            }
            eq.process(&p, 48_000, bl, br);
        }
        let worst = l
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        // A 1 kHz sine at 0.25 · 4 (12 dB) moves at most ~0.13 per sample.
        assert!(worst < 0.15, "largest step {worst}");
    }

    #[test]
    fn the_input_gain_scales_the_program() {
        let mut st = EqState {
            gain_db: -6.0,
            ..EqState::default()
        };
        // Alone (every band flat), and on top of a bell.
        assert!((run_eq(&st, 1000.0) + 6.0).abs() < 0.05);
        st.bands[3] = EqBand {
            kind: EqKind::Bell,
            freq: 1000.0,
            gain_db: 4.0,
            ..EqBand::default()
        };
        assert!((run_eq(&st, 1000.0) + 2.0).abs() < 0.1);
        st.bypass = true;
        assert!(run_eq(&st, 1000.0).abs() < 0.01, "bypass passes the gain by too");
    }

    #[test]
    fn response_matches_the_design() {
        let b = EqBand {
            on: true,
            kind: EqKind::Bell,
            freq: 1000.0,
            gain_db: -9.0,
            q: 2.0,
        };
        let c = coefficients(&b, 48_000.0);
        assert!((response_db(&c, 1000.0, 48_000.0) + 9.0).abs() < 0.01);
        assert!(response_db(&c, 20.0, 48_000.0).abs() < 0.05);
    }

    /// `cargo test --release -p patch-engine master_eq_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn master_eq_cost() {
        let p = Published::default();
        let mut st = EqState::default();
        for (i, b) in st.bands.iter_mut().enumerate() {
            b.on = true;
            b.gain_db = if i % 2 == 0 { 3.0 } else { -3.0 };
        }
        p.set(&st);
        let mut eq = MasterEq::default();
        let x = sine(440.0, 1024);
        let (mut l, mut r) = (x.clone(), x.clone());
        eq.process(&p, 48_000, &mut l, &mut r);
        let t0 = std::time::Instant::now();
        for _ in 0..10_000 {
            l.copy_from_slice(&x);
            r.copy_from_slice(&x);
            eq.process(&p, 48_000, &mut l, &mut r);
        }
        println!(
            "master EQ, 8 bands: {:.2} µs per 1024-frame stereo cycle",
            t0.elapsed().as_secs_f64() * 1e6 / 10_000.0
        );
    }
}
