//! Voice processing for a microphone channel, as a podcast or a stream wants
//! it: a high-pass (rumble, handling noise, plosives), a noise gate (the room
//! between words), a de-esser (sharp s and t), and a compressor (an even
//! level whatever the distance to the mic). In that order, before the
//! channel's EQ and fader. Stereo-linked: one gain for both sides, so a
//! stereo mic's image doesn't move. Runs on the real-time thread: no
//! allocation, no locks; settings come in as plain values each cycle.

pub use patch_core::mic::Settings;

/// Coherent control-plane publication, with a bounded RT read. The caller
/// retains its previous settings while a publication is in progress.
pub struct Published {
    generation: std::sync::atomic::AtomicU64,
    values: [std::sync::atomic::AtomicU32; 9],
}
impl Default for Published {
    fn default() -> Self {
        let p = Self {
            generation: std::sync::atomic::AtomicU64::new(0),
            values: std::array::from_fn(|_| std::sync::atomic::AtomicU32::new(0)),
        };
        p.set(Settings::OFF);
        p
    }
}
impl Published {
    /// One serialized control-plane writer per lane, as for channel EQ.
    pub fn set(&self, s: Settings) {
        use std::sync::atomic::Ordering;
        if s.validate().is_err() {
            return;
        }
        let values = [
            u32::from(s.on),
            s.hpf_hz.to_bits(),
            s.gate_db.unwrap_or(f32::NAN).to_bits(),
            s.deess.to_bits(),
            s.comp.to_bits(),
            s.expander_db.unwrap_or(f32::NAN).to_bits(),
            s.noise_db.unwrap_or(f32::NAN).to_bits(),
            s.input_gain_db.to_bits(),
            s.output_gain_db.to_bits(),
        ];
        self.generation.fetch_add(1, Ordering::AcqRel);
        for (slot, value) in self.values.iter().zip(values) {
            slot.store(value, Ordering::Relaxed);
        }
        self.generation.fetch_add(1, Ordering::Release);
    }
    pub fn read(&self) -> Option<Settings> {
        use std::sync::atomic::Ordering;
        let generation = self.generation.load(Ordering::Acquire);
        if generation & 1 != 0 {
            return None;
        }
        let values = self.values.each_ref().map(|v| v.load(Ordering::Relaxed));
        std::sync::atomic::fence(Ordering::Acquire);
        if self.generation.load(Ordering::Relaxed) != generation {
            return None;
        }
        let f = |i: usize| f32::from_bits(values[i]);
        let optional = |i| (!f(i).is_nan()).then_some(f(i));
        Some(Settings {
            on: values[0] != 0,
            hpf_hz: f(1),
            gate_db: optional(2),
            deess: f(3),
            comp: f(4),
            expander_db: optional(5),
            noise_db: optional(6),
            input_gain_db: f(7),
            output_gain_db: f(8),
        })
    }
}

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
    /// Butterworth low-pass at `f` Hz.
    fn lowpass(f: f32, rate: f32) -> Biquad {
        let w = std::f32::consts::TAU * (f / rate).clamp(1e-5, 0.45);
        let (s, c) = w.sin_cos();
        let alpha = s / std::f32::consts::SQRT_2;
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

    /// Butterworth high-pass at `f` Hz.
    fn highpass(f: f32, rate: f32) -> Biquad {
        let w = std::f32::consts::TAU * (f / rate).clamp(1e-5, 0.45);
        let (s, c) = w.sin_cos();
        let alpha = s / std::f32::consts::SQRT_2;
        let a0 = 1.0 + alpha;
        let b0 = (1.0 + c) / 2.0 / a0;
        Biquad {
            b0,
            b1: -(1.0 + c) / a0,
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

    fn keep_coeffs(&mut self, from: &Biquad) {
        let (z1, z2) = (self.z1, self.z2);
        *self = *from;
        self.z1 = z1;
        self.z2 = z2;
    }
}

/// A one-pole envelope follower: fast up, slow down.
#[derive(Clone, Copy, Default)]
struct Follow {
    up: f32,
    down: f32,
    v: f32,
}

impl Follow {
    fn new(attack_s: f32, release_s: f32, rate: f32) -> Follow {
        let k = |t: f32| 1.0 - (-1.0 / (t * rate)).exp();
        Follow {
            up: k(attack_s),
            down: k(release_s),
            v: 0.0,
        }
    }

    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        let k = if x > self.v { self.up } else { self.down };
        self.v += (x - self.v) * k;
        if self.v < 1e-20 {
            self.v = 0.0;
        }
        self.v
    }
}

/// One channel's state.
pub struct Voice {
    cached_settings: Settings,
    last_active: Settings,
    bypass_mix: f32,
    rate: u32,
    noise: crate::voice_noise::NoiseSuppressor,
    input_gain: f32,
    output_gain: f32,
    input_peak: f32,
    input_rms: f32,
    hpf_for: f32,
    hp: [Biquad; 2],
    /// De-esser: the band above ~4.5 kHz, split off with a 2nd-order
    /// low-pass (low + high is exactly the input), its level against the
    /// whole's.
    split: [Biquad; 2],
    sib: Follow,
    whole: Follow,
    deess_gain: f32,
    gate_env: Follow,
    expand_env: Follow,
    expand_gain: f32,
    gate_gain: f32,
    gate_hold: u32,
    comp_env: Follow,
    comp_gain: f32,
    /// The compressor's gain worked out every 16 samples (a log and a power),
    /// glided to per sample: the cost of a few multiplies a sample.
    comp_want: f32,
    tick: u32,
    smooth: f32,
}

impl Default for Voice {
    fn default() -> Self {
        Voice {
            cached_settings: Settings::OFF,
            last_active: Settings::OFF,
            bypass_mix: 0.0,
            rate: 0,
            noise: crate::voice_noise::NoiseSuppressor::default(),
            input_gain: 1.0,
            output_gain: 1.0,
            input_peak: 0.0,
            input_rms: 0.0,
            hpf_for: -1.0,
            hp: [Biquad::default(); 2],
            split: [Biquad::default(); 2],
            sib: Follow::default(),
            whole: Follow::default(),
            deess_gain: 1.0,
            gate_env: Follow::default(),
            expand_env: Follow::default(),
            expand_gain: 1.0,
            gate_gain: 1.0,
            gate_hold: 0,
            comp_env: Follow::default(),
            comp_gain: 1.0,
            comp_want: 1.0,
            tick: 0,
            smooth: 0.0,
        }
    }
}

fn db(x: f32) -> f32 {
    20.0 * x.max(1e-9).log10()
}

fn lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

impl Voice {
    fn prepare(&mut self, rate: u32, s: &Settings) {
        let r = rate.max(1) as f32;
        if rate != self.rate {
            self.rate = rate;
            self.hpf_for = -1.0;
            self.split = [Biquad::lowpass(4500.0, r); 2];
            self.sib = Follow::new(0.001, 0.040, r);
            self.whole = Follow::new(0.001, 0.040, r);
            self.expand_env = Follow::new(0.001, 0.080, r);
            self.gate_env = Follow::new(0.0005, 0.060, r);
            self.comp_env = Follow::new(0.005, 0.120, r);
            // Gains glide over ~3 ms: no zipper noise, no clicks.
            self.smooth = 1.0 - (-1.0 / (0.003 * r)).exp();
        }
        if s.hpf_hz > 0.0 && s.hpf_hz != self.hpf_for {
            let b = Biquad::highpass(s.hpf_hz, r);
            for h in &mut self.hp {
                h.keep_coeffs(&b);
            }
            self.hpf_for = s.hpf_hz;
        }
    }

    pub fn read_settings(&mut self, published: &Published) -> Settings {
        if let Some(settings) = published.read() {
            self.cached_settings = settings;
        }
        self.cached_settings
    }

    /// Live bypass transition, using the engine's existing raw input slices.
    /// There is no additional buffer or delay; dry/processed signals fade
    /// over 3 ms, avoiding an abrupt jump when voice bypass changes.
    pub fn process_crossfaded(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        dry_l: &[f32],
        dry_r: &[f32],
        s: &Settings,
        rate: u32,
    ) {
        if s.on {
            self.last_active = *s;
        }
        let effective = if s.on {
            *s
        } else if self.bypass_mix > 0.0 {
            self.last_active
        } else {
            *s
        };
        self.process(l, r, &effective, rate);
        let want = if s.on { 1.0 } else { 0.0 };
        if self.bypass_mix == want {
            return;
        }
        let k = 1.0 - (-1.0 / (0.003 * rate.max(1) as f32)).exp();
        for i in 0..l.len().min(r.len()) {
            self.bypass_mix += (want - self.bypass_mix) * k;
            if (want - self.bypass_mix).abs() < 1e-5 {
                self.bypass_mix = want;
            }
            let dry_left = dry_l.get(i).copied().unwrap_or(0.0);
            let dry_right = dry_r.get(i).copied().unwrap_or(0.0);
            l[i] = dry_left + (l[i] - dry_left) * self.bypass_mix;
            r[i] = dry_right + (r[i] - dry_right) * self.bypass_mix;
        }
    }

    /// Linear latest-block input measurements and final detector gains.
    /// Detector gains exclude compressor makeup; no added sample delay.
    pub fn reading(&self) -> [f32; 5] {
        [
            self.input_peak,
            self.input_rms,
            self.gate_gain,
            self.comp_gain,
            self.deess_gain,
        ]
    }

    /// In place, both sides (`r` may be the same signal for a mono mic).
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32], s: &Settings, rate: u32) {
        let n = l.len().min(r.len());
        self.input_peak = 0.0;
        let mut sum = 0.0_f64;
        for (&a, &b) in l[..n].iter().zip(&r[..n]) {
            self.input_peak = self.input_peak.max(a.abs()).max(b.abs());
            sum += a as f64 * a as f64 + b as f64 * b as f64;
        }
        self.input_rms = (sum / (2 * n).max(1) as f64).sqrt() as f32;
        if !s.on {
            self.gate_gain = 1.0;
            self.comp_gain = 1.0;
            self.deess_gain = 1.0;
            return;
        }
        if s.gate_db.is_none() {
            self.gate_gain = 1.0;
        }
        if s.comp == 0.0 {
            self.comp_gain = 1.0;
        }
        if s.deess == 0.0 {
            self.deess_gain = 1.0;
        }
        self.prepare(rate, s);
        let n = l.len().min(r.len());
        let rf = rate.max(1) as f32;
        let hold = (0.050 * rf) as u32;
        // The compressor: threshold from −14 to −30 dBFS, ratio 2:1 to 5:1
        // with the amount; half the reduction at −10 dBFS made up.
        let thr = -14.0 - 16.0 * s.comp;
        let ratio = 2.0 + 3.0 * s.comp;
        let makeup = if s.comp > 0.0 {
            lin(((-10.0 - thr).max(0.0) * (1.0 - 1.0 / ratio)) * 0.5)
        } else {
            1.0
        };
        // The de-esser acts when the band's share of the level passes this.
        let sib_at = 0.45 - 0.25 * s.deess;
        let sib_floor = lin(-12.0 * s.deess);
        let gate_at = s.gate_db.map(lin);
        let room = lin(-30.0);
        let (l, r) = (&mut l[..n], &mut r[..n]);
        let input_target = lin(s.input_gain_db);
        if input_target != 1.0 || self.input_gain != 1.0 {
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                self.input_gain += (input_target - self.input_gain) * self.smooth;
                *a *= self.input_gain;
                *b *= self.input_gain;
            }
        }
        // Stage by stage over the block (they're in series, each with its
        // own state: the same result as sample by sample), each loop's state
        // in locals: 4 stages in one loop spilled registers and cost twice
        // the sum of their parts.
        if s.hpf_hz > 0.0 {
            let (mut h0, mut h1) = (self.hp[0], self.hp[1]);
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                *a = h0.run(*a);
                *b = h1.run(*b);
            }
            self.hp = [h0, h1];
        }
        if let Some(threshold) = s.noise_db {
            self.noise.process(l, r, rate, threshold);
        }
        // De-esser: a dynamic cut of the top band only.
        if s.deess > 0.0 {
            let (mut s0, mut s1, mut sib, mut whole, mut g) = (
                self.split[0],
                self.split[1],
                self.sib,
                self.whole,
                self.deess_gain,
            );
            let k = self.smooth;
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                let (la, lb) = (s0.run(*a), s1.run(*b));
                let (ha, hb) = (*a - la, *b - lb);
                let band = sib.run(ha.abs().max(hb.abs()));
                let all = whole.run(a.abs().max(b.abs()));
                // band / all > sib_at, without the division.
                let want = if band > sib_at * all && all > 1e-6 {
                    (sib_at * all / band).max(sib_floor)
                } else {
                    1.0
                };
                g += (want - g) * k;
                *a = la + ha * g;
                *b = lb + hb * g;
            }
            (
                self.split[0],
                self.split[1],
                self.sib,
                self.whole,
                self.deess_gain,
            ) = (s0, s1, sib, whole, g);
        }
        // Gate: open above the threshold, held 50 ms, then down to −30 dB;
        // opens in ~1 ms, closes over ~100 ms.
        if let Some(at) = gate_at {
            let (mut env, mut g, mut held) = (self.gate_env, self.gate_gain, self.gate_hold);
            let (open_k, close_k) = ((self.smooth * 3.0).min(1.0), self.smooth * 0.03);
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                let open = env.run(a.abs().max(b.abs())) > at;
                if open {
                    held = hold;
                } else {
                    held = held.saturating_sub(1);
                }
                let want = if held > 0 { 1.0 } else { room };
                g += (want - g) * if want > g { open_k } else { close_k };
                *a *= g;
                *b *= g;
            }
            (self.gate_env, self.gate_gain, self.gate_hold) = (env, g, held);
        }
        // Downward expansion uses the same linked-envelope convention as
        // the gate, with a continuous 2:1 curve instead of an on/off floor.
        if let Some(threshold) = s.expander_db {
            let at = lin(threshold);
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                let level = self.expand_env.run(a.abs().max(b.abs()));
                let want = (level / at).clamp(room, 1.0);
                self.expand_gain += (want - self.expand_gain) * self.smooth;
                *a *= self.expand_gain;
                *b *= self.expand_gain;
            }
        }
        // Compressor: on the level both sides share; its gain worked out
        // every 16 samples, glided to in between.
        if s.comp > 0.0 {
            let (mut env, mut g, mut want, mut tick) =
                (self.comp_env, self.comp_gain, self.comp_want, self.tick);
            let k = self.smooth;
            let slope = 1.0 - 1.0 / ratio;
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                let e = env.run(a.abs().max(b.abs()));
                if tick == 0 {
                    let over = db(e) - thr;
                    want = if over > 0.0 { lin(-over * slope) } else { 1.0 };
                }
                tick = (tick + 1) & 15;
                g += (want - g) * k;
                let m = g * makeup;
                *a *= m;
                *b *= m;
            }
            (self.comp_env, self.comp_gain, self.comp_want, self.tick) = (env, g, want, tick);
        }
        let output_target = lin(s.output_gain_db);
        if output_target != 1.0 || self.output_gain != 1.0 {
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                self.output_gain += (output_target - self.output_gain) * self.smooth;
                *a *= self.output_gain;
                *b *= self.output_gain;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn tone(f: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (std::f32::consts::TAU * f * i as f32 / RATE as f32).sin() * amp)
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    fn run(s: Settings, x: &[f32]) -> Vec<f32> {
        let mut v = Voice::default();
        let (mut l, mut r) = (x.to_vec(), x.to_vec());
        for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            v.process(a, b, &s, RATE);
        }
        assert_eq!(l, r, "a mono mic stays mono");
        l
    }

    #[test]
    fn off_leaves_the_signal_alone() {
        let x = tone(440.0, 0.3, 4800);
        assert_eq!(run(Settings::OFF, &x), x);
    }

    #[test]
    fn the_high_pass_takes_rumble_not_the_voice() {
        let only = Settings {
            hpf_hz: 80.0,
            gate_db: None,
            deess: 0.0,
            comp: 0.0,
            on: true,
            expander_db: None,
            noise_db: None,
            input_gain_db: 0.0,
            output_gain_db: 0.0,
        };
        let n = RATE as usize;
        let low = run(only, &tone(30.0, 0.3, n));
        let voice = run(only, &tone(300.0, 0.3, n));
        assert!(
            rms(&low[n / 2..]) < 0.3 * 0.707 * 0.2,
            "30 Hz down: {}",
            rms(&low[n / 2..])
        );
        assert!(
            (rms(&voice[n / 2..]) / (0.3 * 0.707) - 1.0).abs() < 0.05,
            "300 Hz kept"
        );
    }

    #[test]
    fn the_gate_turns_the_room_down_and_opens_for_a_word() {
        let gate = Settings {
            hpf_hz: 0.0,
            gate_db: Some(-50.0),
            deess: 0.0,
            comp: 0.0,
            on: true,
            expander_db: None,
            noise_db: None,
            input_gain_db: 0.0,
            output_gain_db: 0.0,
        };
        let n = RATE as usize;
        // (It closes over ~100 ms, as it should for speech: measured after.)
        let room = run(gate, &tone(200.0, 0.001, n)); // −60 dBFS
        assert!(
            rms(&room[n * 9 / 10..]) < 0.001 * 0.707 * 0.06,
            "room ~30 dB down: {}",
            rms(&room[n * 9 / 10..]) / (0.001 * 0.707)
        );
        let word = run(gate, &tone(200.0, 0.1, n)); // −20 dBFS
        assert!(
            (rms(&word[n / 2..]) / (0.1 * 0.707) - 1.0).abs() < 0.02,
            "a word passes"
        );
    }

    #[test]
    fn the_compressor_evens_out_loud_and_soft() {
        let comp = Settings {
            hpf_hz: 0.0,
            gate_db: None,
            deess: 0.0,
            comp: 0.6,
            on: true,
            expander_db: None,
            noise_db: None,
            input_gain_db: 0.0,
            output_gain_db: 0.0,
        };
        let n = RATE as usize;
        let loud = run(comp, &tone(300.0, 0.8, n));
        let soft = run(comp, &tone(300.0, 0.05, n));
        let (dl, ds) = (db(rms(&loud[n / 2..])), db(rms(&soft[n / 2..])));
        let before = db(0.8) - db(0.05);
        assert!(
            dl - ds < before - 6.0,
            "{before:.1} dB apart before, {:.1} after",
            dl - ds
        );
        assert!(loud[n / 2..].iter().all(|v| v.abs() < 1.0), "no overs");
    }

    #[test]
    fn the_de_esser_cuts_an_s_not_a_vowel() {
        let de = Settings {
            hpf_hz: 0.0,
            gate_db: None,
            deess: 0.8,
            comp: 0.0,
            on: true,
            expander_db: None,
            noise_db: None,
            input_gain_db: 0.0,
            output_gain_db: 0.0,
        };
        let n = RATE as usize / 2;
        let s = run(de, &tone(7000.0, 0.3, n));
        let vowel = run(de, &tone(400.0, 0.3, n));
        assert!(
            rms(&s[n / 2..]) < 0.3 * 0.707 * 0.7,
            "an s comes down 3 dB or more: {}",
            rms(&s[n / 2..]) / (0.3 * 0.707)
        );
        assert!(
            (rms(&vowel[n / 2..]) / (0.3 * 0.707) - 1.0).abs() < 0.05,
            "a vowel untouched"
        );
    }

    /// cargo test --release -p patch-engine voice_cost -- --ignored --nocapture
    #[test]
    #[ignore]
    fn voice_cost() {
        let x = tone(300.0, 0.3, 1024);
        let off = Settings {
            on: true,
            hpf_hz: 0.0,
            gate_db: None,
            deess: 0.0,
            comp: 0.0,
            expander_db: None,
            noise_db: None,
            input_gain_db: 0.0,
            output_gain_db: 0.0,
        };
        for (name, s) in [
            ("all four", Settings::default()),
            (
                "high-pass",
                Settings {
                    hpf_hz: 80.0,
                    ..off
                },
            ),
            (
                "gate",
                Settings {
                    gate_db: Some(-50.0),
                    ..off
                },
            ),
            ("de-esser", Settings { deess: 0.4, ..off }),
            ("compressor", Settings { comp: 0.5, ..off }),
            (
                "expander",
                Settings {
                    expander_db: Some(-50.0),
                    ..off
                },
            ),
            (
                "suppression",
                Settings {
                    noise_db: Some(-50.0),
                    ..off
                },
            ),
        ] {
            let mut v = Voice::default();
            let (mut l, mut r) = (x.clone(), x.clone());
            for _ in 0..200 {
                v.process(&mut l, &mut r, &s, RATE);
            }
            let n = 20_000;
            let t = std::time::Instant::now();
            for _ in 0..n {
                l.copy_from_slice(&x);
                r.copy_from_slice(&x);
                v.process(&mut l, &mut r, &s, RATE);
            }
            let per = t.elapsed().as_secs_f64() / n as f64;
            println!(
                "{name:>10}, 1024 frames stereo: {:5.1} µs ({:.3} % of a block)",
                per * 1e6,
                per / (1024.0 / 48_000.0) * 100.0
            );
        }
    }

    #[test]
    fn nothing_blows_up_in_silence_or_at_full_scale() {
        let x: Vec<f32> = (0..RATE as usize)
            .map(|i| {
                if i < 24_000 {
                    0.0
                } else {
                    if i % 2 == 0 { 1.0 } else { -1.0 }
                }
            })
            .collect();
        let y = run(Settings::default(), &x);
        assert!(y.iter().all(|v| v.is_finite() && v.abs() <= 2.0));
    }
}
