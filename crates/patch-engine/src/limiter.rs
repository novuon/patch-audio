//! The master limiter: a look-ahead brick wall on the program output.
//!
//! Two correlated tracks at the crossfader's centre sum to +3 dB, an EQ
//! boost adds up to +6: without it, those peaks clip at the DAC. It never
//! colours normal play; it only acts on what would go over.
//!
//! How it guarantees the ceiling (no overshoot, no clicks):
//!
//! * the audio is delayed by the look-ahead `L` (1.5 ms);
//! * `hold[n]` is the gain the loudest sample in the window `[n − L, n]`
//!   needs (a sliding maximum, O(1) with a monotonic queue);
//! * a moving average of `hold` over `L` samples is the attack: every term
//!   in it is at most the gain the delayed sample needs, so the sample
//!   comes out at or under the ceiling, and the gain has eased down over
//!   the whole look-ahead;
//! * the release is a one-pole (80 ms) that only ever rises towards the
//!   average (never above it), so the guarantee holds while releasing too.
//!
//! Zero latency when nothing of Patch's own can go over (no deck loaded,
//! so no DJ mix summing or boosting: apps through Patch): the gain drops on
//! the very sample that would go over (instant attack, the same release),
//! never clipping, at the cost of a little distortion on such a sample
//! (plain PipeWire clips there). The mode changes only after a stretch of
//! digital silence as long as the look-ahead: the 72 frames dropped or
//! inserted are silence, so nothing clicks.
//!
//! Stereo-linked, preallocated, no allocation or locking: RT-safe.

/// Longest look-ahead supported (1.5 ms at 192 kHz is 288 frames).
const MAX_LOOKAHEAD: usize = 512;

pub struct Limiter {
    ceiling: f32,
    lookahead: usize,
    rate: u32,
    /// Delay line (stereo), `lookahead` frames used.
    dl: [[f32; MAX_LOOKAHEAD + 1]; 2],
    /// Monotonic queue of (frame index, peak) for the sliding maximum.
    q_idx: [u64; MAX_LOOKAHEAD + 2],
    q_val: [f32; MAX_LOOKAHEAD + 2],
    q_head: usize,
    q_len: usize,
    /// Moving average of the hold gain (a ring and its running sum).
    avg_ring: [f32; MAX_LOOKAHEAD + 1],
    avg_sum: f64,
    /// Output gain (after the release) and the release coefficient.
    gain: f32,
    release: f32,
    n: u64,
    /// Delay-line / average-ring position (wraps at the look-ahead).
    pos: usize,
    /// Lowest gain in the last block (for meters and diagnostics).
    pub reduction: f32,
    /// Look-ahead in use (else zero latency), and the frames of digital
    /// silence in a row (a mode change waits for `lookahead` of them).
    delayed: bool,
    silent: usize,
}

/// Below this, a sample counts as silence (−100 dBFS).
const SILENCE: f32 = 1e-5;

impl Limiter {
    pub fn new(ceiling_dbfs: f32, rate: u32) -> Limiter {
        let rate = rate.max(1);
        let lookahead = ((rate as f64 * 0.0015).round() as usize).clamp(1, MAX_LOOKAHEAD);
        Limiter {
            ceiling: 10f32.powf(ceiling_dbfs / 20.0),
            lookahead,
            rate,
            dl: [[0.0; MAX_LOOKAHEAD + 1]; 2],
            q_idx: [0; MAX_LOOKAHEAD + 2],
            q_val: [0.0; MAX_LOOKAHEAD + 2],
            q_head: 0,
            q_len: 0,
            avg_ring: [1.0; MAX_LOOKAHEAD + 1],
            avg_sum: lookahead as f64,
            gain: 1.0,
            release: (-1.0 / (0.080 * rate as f64)).exp() as f32,
            n: 0,
            pos: 0,
            reduction: 1.0,
            delayed: true,
            silent: 0,
        }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Capture starts with no delay; its first non-silent block must pass too.
    pub fn new_zero_latency(ceiling_dbfs: f32, rate: u32) -> Self {
        let mut limiter = Self::new(ceiling_dbfs, rate);
        limiter.delayed = false;
        limiter
    }

    /// Latency added to the program, in frames (0 in the zero-latency mode).
    pub fn latency(&self) -> usize {
        if self.delayed { self.lookahead } else { 0 }
    }

    /// Whether the look-ahead is in use.
    pub fn delayed(&self) -> bool {
        self.delayed
    }

    /// Limit with the look-ahead (always).
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        self.process_mode(l, r, true);
    }

    /// Limit; `want_delay`: the look-ahead is wanted (a deck is loaded).
    /// The mode follows it at the next stretch of silence.
    pub fn process_mode(&mut self, l: &mut [f32], r: &mut [f32], want_delay: bool) {
        if want_delay != self.delayed && self.silent > self.lookahead {
            // Everything in the delay line, and everything now in the
            // window, is silence: drop it or start from it, unheard.
            self.delayed = want_delay;
            self.dl = [[0.0; MAX_LOOKAHEAD + 1]; 2];
            self.q_len = 0;
            self.avg_ring = [1.0; MAX_LOOKAHEAD + 1];
            self.avg_sum = self.lookahead as f64;
            self.pos = 0;
        }
        let quiet = |a: f32, b: f32| a.abs() < SILENCE && b.abs() < SILENCE;
        if !self.delayed {
            let mut lowest = 1.0f32;
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                self.silent = if quiet(*a, *b) {
                    self.silent.saturating_add(1)
                } else {
                    0
                };
                let peak = a.abs().max(b.abs());
                let hold = if peak > self.ceiling {
                    self.ceiling / peak
                } else {
                    1.0
                };
                self.gain = if hold < self.gain {
                    hold
                } else {
                    hold + (self.gain - hold) * self.release
                };
                let g = self.gain;
                lowest = lowest.min(g);
                *a = (*a * g).clamp(-self.ceiling, self.ceiling);
                *b = (*b * g).clamp(-self.ceiling, self.ceiling);
                self.n += 1;
            }
            self.reduction = lowest;
            return;
        }
        for (a, b) in l.iter().zip(r.iter()) {
            self.silent = if quiet(*a, *b) {
                self.silent.saturating_add(1)
            } else {
                0
            };
        }
        self.process_delayed(l, r);
    }

    fn process_delayed(&mut self, l: &mut [f32], r: &mut [f32]) {
        let la = self.lookahead;
        let cap = self.q_idx.len();
        let mut lowest = 1.0f32;
        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
            let n = self.n;
            let slot = self.pos;
            // The sample leaving the delay (L frames ago), then the new one in.
            let (out_l, out_r) = (self.dl[0][slot], self.dl[1][slot]);
            self.dl[0][slot] = *a;
            self.dl[1][slot] = *b;
            // Sliding maximum of |x| over the window [n − L, n].
            let v = a.abs().max(b.abs());
            while self.q_len > 0 {
                let back = wrap(self.q_head + self.q_len - 1, cap);
                if self.q_val[back] <= v {
                    self.q_len -= 1;
                } else {
                    break;
                }
            }
            let tail = wrap(self.q_head + self.q_len, cap);
            self.q_idx[tail] = n;
            self.q_val[tail] = v;
            self.q_len += 1;
            while self.q_idx[self.q_head] + (la as u64) < n {
                self.q_head = wrap(self.q_head + 1, cap);
                self.q_len -= 1;
            }
            let peak = self.q_val[self.q_head];
            let hold = if peak > self.ceiling {
                self.ceiling / peak
            } else {
                1.0
            };
            // Attack: moving average of the hold over the look-ahead.
            let ai = slot;
            self.avg_sum += hold as f64 - self.avg_ring[ai] as f64;
            self.avg_ring[ai] = hold;
            let attack = (self.avg_sum / la as f64).min(1.0) as f32;
            // Release: rise towards the average, never above it.
            self.gain = if attack < self.gain {
                attack
            } else {
                attack + (self.gain - attack) * self.release
            };
            let g = self.gain;
            lowest = lowest.min(g);
            // The guarantee holds by construction; the clamp only guards
            // against float rounding at the ceiling.
            *a = (out_l * g).clamp(-self.ceiling, self.ceiling);
            *b = (out_r * g).clamp(-self.ceiling, self.ceiling);
            self.n = n + 1;
            self.pos = if slot + 1 == la { 0 } else { slot + 1 };
        }
        self.reduction = lowest;
    }
}

/// `i` wrapped into `0..cap` (i < 2 · cap): a compare, not a division.
#[inline(always)]
fn wrap(i: usize, cap: usize) -> usize {
    if i >= cap { i - cap } else { i }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(n: usize, f: f32, amp: f32) -> Vec<f32> {
        (0..n)
            .map(|i| (i as f32 * f * std::f32::consts::TAU / 48_000.0).sin() * amp)
            .collect()
    }

    #[test]
    fn quiet_audio_passes_untouched_after_the_delay() {
        let mut lim = Limiter::new(-0.5, 48_000);
        let x = sine(4800, 440.0, 0.5);
        let (mut l, mut r) = (x.clone(), x.clone());
        lim.process(&mut l, &mut r);
        let d = lim.latency();
        assert!(
            l[d..].iter().zip(&x).all(|(a, b)| (a - b).abs() < 1e-7),
            "bit-true, only delayed"
        );
    }

    #[test]
    fn nothing_ever_exceeds_the_ceiling() {
        // Two correlated tracks summed at the centre: +3 dB over full scale,
        // plus sudden transients.
        let mut lim = Limiter::new(-0.5, 48_000);
        let ceiling = 10f32.powf(-0.5 / 20.0);
        let mut x = sine(48_000, 100.0, 1.41);
        for i in (0..48_000).step_by(4_000) {
            x[i] = 2.0;
            x[i + 1] = -1.9;
        }
        let (mut l, mut r) = (x.clone(), x.iter().map(|v| v * 0.9).collect::<Vec<_>>());
        for (bl, br) in l.chunks_mut(1024).zip(r.chunks_mut(1024)) {
            lim.process(bl, br);
        }
        let over = l.iter().chain(&r).map(|v| v.abs()).fold(0.0f32, f32::max);
        assert!(
            over <= ceiling + 1e-6,
            "peak {over} over the ceiling {ceiling}"
        );
        // And it does not pump the audio to silence: most of it stays loud.
        let loud = l.iter().filter(|v| v.abs() > 0.5).count();
        assert!(loud > 10_000);
    }

    #[test]
    fn zero_latency_without_a_deck_and_a_silent_switch() {
        let mut lim = Limiter::new(-0.5, 48_000);
        let ceiling = 10f32.powf(-0.5 / 20.0);
        // Silence first: the mode changes at once.
        let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        lim.process_mode(&mut l, &mut r, false);
        let x = sine(4800, 440.0, 0.5);
        let (mut l, mut r) = (x.clone(), x.clone());
        lim.process_mode(&mut l, &mut r, false);
        assert_eq!(lim.latency(), 0);
        assert!(
            l.iter().zip(&x).all(|(a, b)| (a - b).abs() < 1e-7),
            "bit-true, no delay"
        );
        // Overs: never past the ceiling, even without a look-ahead.
        let hot = sine(4800, 100.0, 1.6);
        let (mut l, mut r) = (hot.clone(), hot.clone());
        lim.process_mode(&mut l, &mut r, false);
        assert!(l.iter().all(|v| v.abs() <= ceiling + 1e-6));
        // Wanted back while it plays: it waits for silence.
        let (mut l, mut r) = (x.clone(), x.clone());
        lim.process_mode(&mut l, &mut r, true);
        assert_eq!(lim.latency(), 0, "no change mid-sound");
        let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        lim.process_mode(&mut l, &mut r, true);
        let (mut l, mut r) = (vec![0.0f32; 16], vec![0.0f32; 16]);
        lim.process_mode(&mut l, &mut r, true);
        assert_eq!(
            lim.latency(),
            lim.lookahead,
            "look-ahead again after silence"
        );
    }

    #[test]
    fn gain_changes_are_smooth() {
        // A step from quiet to far over: the gain eases over the look-ahead,
        // no sample-to-sample jumps larger than a small fraction.
        let mut lim = Limiter::new(-0.5, 48_000);
        let mut x = vec![0.2f32; 2000];
        x.extend(std::iter::repeat_n(1.8f32, 2000));
        let (mut l, mut r) = (x.clone(), x.clone());
        lim.process(&mut l, &mut r);
        let d = lim.latency();
        let gains: Vec<f32> = l[d..].iter().zip(&x).map(|(y, x)| y / x).collect();
        let worst = gains
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 0.02, "largest per-sample gain step {worst}");
    }

    /// `cargo test --release -p patch-engine limiter_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn limiter_cost() {
        let mut lim = Limiter::new(-0.5, 48_000);
        let x = sine(1024, 440.0, 1.3);
        let (mut l, mut r) = (x.clone(), x.clone());
        let t0 = std::time::Instant::now();
        for _ in 0..10_000 {
            l.copy_from_slice(&x);
            r.copy_from_slice(&x);
            lim.process(&mut l, &mut r);
        }
        println!(
            "limiter: {:.2} µs per 1024-frame stereo cycle",
            t0.elapsed().as_secs_f64() * 1e6 / 10_000.0
        );
    }
}
