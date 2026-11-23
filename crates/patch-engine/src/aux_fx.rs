//! The Mixer's effect returns: FX 1 is a reverb, FX 2 a delay. Each takes
//! its send bus (the sum of every channel's FX send) and returns only the
//! effect (wet), which the return strip's fader mixes into the program.
//!
//! Real-time safe: every buffer is allocated here, at construction, for
//! the highest rate the engine runs at; processing never allocates. When a
//! bus has had no input and its tail has died away, `idle` lets the engine
//! skip it (no cost while no one sends).

/// Rates up to this keep their full delay lines (above, times shorten).
const MAX_RATE: usize = 192_000;

/// Freeverb's tuning (in samples at 44.1 kHz), scaled to the rate.
const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASSES: [usize; 4] = [556, 441, 341, 225];
const STEREO_SPREAD: usize = 23;
/// Below this the tail counts as gone.
const SILENT: f32 = 1e-6;

struct Comb {
    buf: Box<[f32]>,
    len: usize,
    pos: usize,
    store: f32,
}

impl Comb {
    fn new(max: usize) -> Comb {
        Comb { buf: vec![0.0; max].into_boxed_slice(), len: max, pos: 0, store: 0.0 }
    }
    #[inline]
    fn run(&mut self, x: f32, feedback: f32, damp: f32) -> f32 {
        let y = self.buf[self.pos];
        self.store = y * (1.0 - damp) + self.store * damp;
        self.buf[self.pos] = x + self.store * feedback;
        self.pos += 1;
        if self.pos >= self.len {
            self.pos = 0;
        }
        y
    }
}

struct Allpass {
    buf: Box<[f32]>,
    len: usize,
    pos: usize,
}

impl Allpass {
    fn new(max: usize) -> Allpass {
        Allpass { buf: vec![0.0; max].into_boxed_slice(), len: max, pos: 0 }
    }
    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        let b = self.buf[self.pos];
        let y = b - x;
        self.buf[self.pos] = x + b * 0.5;
        self.pos += 1;
        if self.pos >= self.len {
            self.pos = 0;
        }
        y
    }
}

/// A stereo Freeverb: eight combs and four allpasses per side.
pub struct Reverb {
    combs: [Vec<Comb>; 2],
    allpasses: [Vec<Allpass>; 2],
    rate: u32,
    quiet: usize,
}

impl Default for Reverb {
    fn default() -> Self {
        let scale = |n: usize| n * MAX_RATE / 44_100 + 1;
        let side = |spread: usize| {
            (
                COMBS.iter().map(|n| Comb::new(scale(n + spread))).collect(),
                ALLPASSES.iter().map(|n| Allpass::new(scale(n + spread))).collect(),
            )
        };
        let (cl, al) = side(0);
        let (cr, ar) = side(STEREO_SPREAD);
        let mut r = Reverb { combs: [cl, cr], allpasses: [al, ar], rate: 0, quiet: usize::MAX };
        r.set_rate(48_000);
        r
    }
}

impl Reverb {
    fn set_rate(&mut self, rate: u32) {
        if rate == self.rate {
            return;
        }
        self.rate = rate;
        let r = (rate as usize).clamp(8_000, MAX_RATE);
        for (side, spread) in [(0, 0), (1, STEREO_SPREAD)] {
            for (c, n) in self.combs[side].iter_mut().zip(COMBS) {
                c.len = ((n + spread) * r / 44_100).clamp(1, c.buf.len());
                c.pos %= c.len;
            }
            for (a, n) in self.allpasses[side].iter_mut().zip(ALLPASSES) {
                a.len = ((n + spread) * r / 44_100).clamp(1, a.buf.len());
                a.pos %= a.len;
            }
        }
    }

    /// Nothing in and the tail under −120 dB for 2 s: the engine may skip it.
    pub fn idle(&self) -> bool {
        self.quiet > (self.rate as usize) * 2
    }

    /// In place: `l`, `r` (the send bus) become the reverb's wet output.
    /// `size` 0…1 sets the decay (a room to a hall).
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32], size: f32, rate: u32) {
        self.set_rate(rate);
        let feedback = 0.7 + 0.28 * size.clamp(0.0, 1.0);
        let damp = 0.2 + 0.3 * (1.0 - size.clamp(0.0, 1.0));
        let n = l.len().min(r.len());
        // The last sample above silence (counted from the end).
        let mut loud = None;
        for i in 0..n {
            // Mono into the tank (Freeverb's way), stereo out of it.
            let x = (l[i] + r[i]) * 0.015;
            let mut peak = x.abs();
            for (side, out) in [(0usize, &mut l[i]), (1, &mut r[i])] {
                let mut y = 0.0;
                for c in &mut self.combs[side] {
                    y += c.run(x, feedback, damp);
                }
                for a in &mut self.allpasses[side] {
                    y = a.run(y);
                }
                *out = y;
                peak = peak.max(y.abs());
            }
            if peak > SILENT {
                loud = Some(i);
            }
        }
        self.quiet = match loud {
            Some(i) => n - 1 - i,
            None => self.quiet.saturating_add(n),
        };
    }

    /// Skipped while idle: count the silence so it stays idle.
    pub fn rest(&mut self, frames: usize) {
        self.quiet = self.quiet.saturating_add(frames);
    }
}

/// A stereo delay with feedback and a darkening repeat.
pub struct Delay {
    buf: [Box<[f32]>; 2],
    pos: usize,
    tone: [f32; 2],
    quiet: usize,
    rate: u32,
}

impl Default for Delay {
    fn default() -> Self {
        // Two seconds at the highest rate.
        Delay {
            buf: [vec![0.0; MAX_RATE * 2].into_boxed_slice(), vec![0.0; MAX_RATE * 2].into_boxed_slice()],
            pos: 0,
            tone: [0.0; 2],
            quiet: usize::MAX,
            rate: 48_000,
        }
    }
}

impl Delay {
    /// Silent for longer than the longest delay (2 s) can hold a repeat.
    pub fn idle(&self) -> bool {
        self.quiet > (self.rate as usize) * 5
    }

    /// In place: the send bus becomes the repeats. `ms` 1…2000, `feedback`
    /// 0…0.9.
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32], ms: f32, feedback: f32, rate: u32) {
        self.rate = rate;
        let len = self.buf[0].len();
        let d = ((ms.clamp(1.0, 2000.0) / 1000.0 * rate as f32) as usize).clamp(1, len - 1);
        let fb = feedback.clamp(0.0, 0.9);
        let n = l.len().min(r.len());
        let mut loud = None;
        for i in 0..n {
            let read = (self.pos + len - d) % len;
            let mut peak = 0.0f32;
            for (side, x) in [(0usize, &mut l[i]), (1, &mut r[i])] {
                let y = self.buf[side][read];
                // Each repeat a little darker, like tape.
                self.tone[side] += (y - self.tone[side]) * 0.45;
                self.buf[side][self.pos] = *x + self.tone[side] * fb;
                peak = peak.max(y.abs()).max(x.abs());
                *x = y;
            }
            if peak > SILENT || self.tone[0].abs() > SILENT || self.tone[1].abs() > SILENT {
                loud = Some(i);
            }
            self.pos = (self.pos + 1) % len;
        }
        self.quiet = match loud {
            Some(i) => n - 1 - i,
            None => self.quiet.saturating_add(n),
        };
    }

    pub fn rest(&mut self, frames: usize) {
        self.quiet = self.quiet.saturating_add(frames);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_delay_repeats_after_its_time_and_dies_away() {
        let mut d = Delay::default();
        let mut l = vec![0.0f32; 48_000];
        let mut r = l.clone();
        l[0] = 1.0;
        r[0] = 1.0;
        d.process(&mut l, &mut r, 250.0, 0.5, 48_000);
        assert_eq!(l[0], 0.0, "wet only: no dry");
        assert!((l[12_000] - 1.0).abs() < 1e-6, "the first repeat at 250 ms");
        assert!(l[24_000] > 0.0 && l[24_000] < 0.5, "the second, quieter and darker");
        let mut tail = vec![0.0f32; 48_000 * 14];
        let mut tr = tail.clone();
        d.process(&mut tail, &mut tr, 250.0, 0.5, 48_000);
        assert!(tail.iter().rev().take(48_000).all(|v| v.abs() < 1e-5), "gone");
        assert!(d.idle());
    }

    #[test]
    fn the_reverb_rings_and_goes_quiet() {
        let mut rv = Reverb::default();
        assert!(rv.idle(), "nothing sent yet");
        let mut l = vec![0.0f32; 4096];
        let mut r = l.clone();
        l[0] = 1.0;
        r[0] = 1.0;
        rv.process(&mut l, &mut r, 0.5, 48_000);
        assert!(!rv.idle());
        let energy: f32 = l.iter().chain(&r).map(|v| v * v).sum();
        assert!(energy > 1e-6, "it rings");
        assert!(l.iter().chain(&r).all(|v| v.abs() < 1.0), "and stays below full scale");
        // Twelve seconds later it has died away and may be skipped.
        for _ in 0..(48_000 * 13 / 4096) {
            let (mut a, mut b) = (vec![0.0f32; 4096], vec![0.0f32; 4096]);
            rv.process(&mut a, &mut b, 0.5, 48_000);
        }
        assert!(rv.idle());
    }
}
