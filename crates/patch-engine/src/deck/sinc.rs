//! Varispeed without keylock: band-limited resampling (vinyl behaviour).
//!
//! Windowed-sinc interpolation, 32 taps, Kaiser window (β = 8.6), from a
//! table of 256 sub-sample phases with linear interpolation between them.
//! Above unity speed the cutoff follows 1/speed, so speeding up never
//! aliases. Deterministic and allocation-free once built.

use std::f64::consts::PI;

pub const TAPS: usize = 32;
const PHASES: usize = 256;

pub struct Sinc {
    /// table[phase][tap], PHASES + 1 rows (the last closes the interpolation).
    table: Vec<[f32; TAPS]>,
    /// The same, each tap twice ([w0, w0, w1, w1, …]): one pass over
    /// interleaved stereo gives both channels (the fast path).
    table2: Vec<[f32; 2 * TAPS]>,
    cutoff: f64,
}

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    while term > 1e-12 * sum {
        term *= (x / (2.0 * k)).powi(2);
        sum += term;
        k += 1.0;
    }
    sum
}

impl Sinc {
    pub fn new() -> Sinc {
        let mut s = Sinc {
            table: vec![[0.0; TAPS]; PHASES + 1],
            table2: vec![[0.0; 2 * TAPS]; PHASES + 1],
            cutoff: 0.0,
        };
        s.design(1.0);
        s
    }

    /// Cutoff as a fraction of Nyquist (≤ 1). Rebuilt only when it changes
    /// by more than 1 %, so tempo moves cost nothing.
    fn design(&mut self, cutoff: f64) {
        if (cutoff - self.cutoff).abs() < 0.01 {
            return;
        }
        self.cutoff = cutoff;
        let beta = 8.6;
        let norm = bessel_i0(beta);
        let half = TAPS as f64 / 2.0;
        for (p, row) in self.table.iter_mut().enumerate() {
            let frac = p as f64 / PHASES as f64;
            let mut sum = 0.0;
            let mut taps = [0.0f64; TAPS];
            for (k, t) in taps.iter_mut().enumerate() {
                // Tap k sits at offset (k − half + 1 − frac) from the point.
                let x = k as f64 - half + 1.0 - frac;
                let sinc = if x.abs() < 1e-12 {
                    1.0
                } else {
                    (PI * cutoff * x).sin() / (PI * cutoff * x)
                };
                let w = (1.0 - (x / half).powi(2)).max(0.0);
                *t = cutoff * sinc * bessel_i0(beta * w.sqrt()) / norm;
                sum += *t;
            }
            // Unity DC gain at every phase.
            for (k, t) in taps.iter().enumerate() {
                row[k] = (t / sum) as f32;
            }
        }
        for (row, row2) in self.table.iter().zip(self.table2.iter_mut()) {
            for (k, w) in row.iter().enumerate() {
                row2[2 * k] = *w;
                row2[2 * k + 1] = *w;
            }
        }
    }

    /// The same deterministic interpolation phase for every bundle channel.
    pub fn at(&mut self, pos: f64, step: f64, src: impl Fn(i64) -> f32) -> f32 {
        self.design(1.0 / step.abs().max(1.0));
        let base = pos.floor();
        let fp = (pos - base) * PHASES as f64;
        let p = fp as usize;
        let t = (fp - p as f64) as f32;
        let first = base as i64 - TAPS as i64 / 2 + 1;
        (0..TAPS)
            .map(|k| {
                (self.table[p][k] + (self.table[p + 1][k] - self.table[p][k]) * t)
                    * src(first + k as i64)
            })
            .sum()
    }

    /// As [`render`](Self::render), with the track's samples in hand:
    /// `x` is interleaved stereo, and frames `0 .. until` of it are read
    /// directly (the loop mapping is the identity there). Windows that reach
    /// outside (the edges, a loop's wrap) take the careful path through
    /// `src`. AVX2 + FMA where the CPU has them: 8 taps an instruction.
    pub fn render_from(
        &mut self,
        out: &mut [f32],
        mut pos: f64,
        step: f64,
        x: &[f32],
        until: i64,
        src: impl Fn(i64, usize) -> f32,
    ) -> f64 {
        self.design((1.0 / step.max(1.0)).min(1.0) * 0.97);
        let half = TAPS as i64 / 2;
        let until = until.min((x.len() / 2) as i64);
        let wide = simd();
        for f in out.as_chunks_mut::<2>().0 {
            let base = pos.floor();
            let fp = (pos - base) * PHASES as f64;
            let p = fp as usize;
            let t = (fp - p as f64) as f32;
            let first = base as i64 - half + 1;
            if first >= 0 && first + TAPS as i64 <= until {
                let xs = &x[first as usize * 2..first as usize * 2 + 2 * TAPS];
                let (a, b) = (&self.table2[p], &self.table2[p + 1]);
                // SAFETY: `wide` only when the CPU has AVX2 and FMA.
                let (l, r) = if wide {
                    unsafe { dot_avx2(a, b, t, xs) }
                } else {
                    dot(a, b, t, xs)
                };
                f[0] = l;
                f[1] = r;
            } else {
                let (a, b) = (&self.table[p], &self.table[p + 1]);
                let (mut l, mut r) = (0.0f32, 0.0f32);
                for k in 0..TAPS {
                    let w = a[k] + (b[k] - a[k]) * t;
                    let i = first + k as i64;
                    l += w * src(i, 0);
                    r += w * src(i, 1);
                }
                f[0] = l;
                f[1] = r;
            }
            pos += step;
        }
        pos
    }
}

/// AVX2 and FMA on this CPU (checked once).
fn simd() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        static WIDE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *WIDE.get_or_init(|| {
            std::env::var("PATCH_DSP").as_deref() != Ok("reference")
                && is_x86_feature_detected!("avx2")
                && is_x86_feature_detected!("fma")
        })
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// Both channels of one output frame: interpolated taps (`a`…`b` at `t`)
/// times 32 interleaved stereo frames.
#[inline]
fn dot(a: &[f32; 2 * TAPS], b: &[f32; 2 * TAPS], t: f32, x: &[f32]) -> (f32, f32) {
    let (mut l, mut r) = (0.0f32, 0.0f32);
    for k in 0..TAPS {
        let w = a[2 * k] + (b[2 * k] - a[2 * k]) * t;
        l += w * x[2 * k];
        r += w * x[2 * k + 1];
    }
    (l, r)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn dot_avx2(a: &[f32; 2 * TAPS], b: &[f32; 2 * TAPS], t: f32, x: &[f32]) -> (f32, f32) {
    use std::arch::x86_64::*;
    // SAFETY: AVX2 and FMA are enabled for this function; every load reads
    // 8 floats inside the 64 of a, b and x (x.len() >= 64).
    unsafe {
        debug_assert!(x.len() >= 2 * TAPS);
        let tv = _mm256_set1_ps(t);
        let mut acc0 = _mm256_setzero_ps();
        let mut acc1 = _mm256_setzero_ps();
        // 64 floats: 8 vectors of [L R L R L R L R]; two accumulators hide
        // the FMA latency.
        let (pa, pb, px) = (a.as_ptr(), b.as_ptr(), x.as_ptr());
        for j in (0..8).step_by(2) {
            let (a0, b0, x0) = (
                _mm256_loadu_ps(pa.add(8 * j)),
                _mm256_loadu_ps(pb.add(8 * j)),
                _mm256_loadu_ps(px.add(8 * j)),
            );
            let (a1, b1, x1) = (
                _mm256_loadu_ps(pa.add(8 * j + 8)),
                _mm256_loadu_ps(pb.add(8 * j + 8)),
                _mm256_loadu_ps(px.add(8 * j + 8)),
            );
            acc0 = _mm256_fmadd_ps(_mm256_fmadd_ps(_mm256_sub_ps(b0, a0), tv, a0), x0, acc0);
            acc1 = _mm256_fmadd_ps(_mm256_fmadd_ps(_mm256_sub_ps(b1, a1), tv, a1), x1, acc1);
        }
        let acc = _mm256_add_ps(acc0, acc1);
        // [L R L R] + [L R L R] → [L R L R] → L and R.
        let s = _mm_add_ps(_mm256_castps256_ps128(acc), _mm256_extractf128_ps(acc, 1));
        let s = _mm_add_ps(s, _mm_movehl_ps(s, s));
        (_mm_cvtss_f32(s), _mm_cvtss_f32(_mm_shuffle_ps(s, s, 1)))
    }
}

#[cfg(not(target_arch = "x86_64"))]
unsafe fn dot_avx2(a: &[f32; 2 * TAPS], b: &[f32; 2 * TAPS], t: f32, x: &[f32]) -> (f32, f32) {
    dot(a, b, t, x)
}

impl Sinc {
    /// Render `out.len() / 2` stereo frames starting at source position
    /// `pos`, advancing `step` source frames per output frame. `src(i, c)`
    /// fetches source frame `i` (callers map loops and bounds). Returns the
    /// position after the last frame.
    pub fn render(
        &mut self,
        out: &mut [f32],
        mut pos: f64,
        step: f64,
        src: impl Fn(i64, usize) -> f32,
    ) -> f64 {
        self.design((1.0 / step.max(1.0)).min(1.0) * 0.97);
        let half = TAPS as i64 / 2;
        for f in out.as_chunks_mut::<2>().0 {
            let base = pos.floor();
            let frac = pos - base;
            let fp = frac * PHASES as f64;
            let p = fp as usize;
            let t = (fp - p as f64) as f32;
            let (a, b) = (&self.table[p], &self.table[p + 1]);
            let first = base as i64 - half + 1;
            let (mut l, mut r) = (0.0f32, 0.0f32);
            for k in 0..TAPS {
                let w = a[k] + (b[k] - a[k]) * t;
                let i = first + k as i64;
                l += w * src(i, 0);
                r += w * src(i, 1);
            }
            f[0] = l;
            f[1] = r;
            pos += step;
        }
        pos
    }
}

impl Default for Sinc {
    fn default() -> Self {
        Sinc::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(f: f64, n: usize, rate: f64) -> Vec<f32> {
        (0..n)
            .flat_map(|i| {
                let v = (2.0 * PI * f * i as f64 / rate).sin() as f32 * 0.5;
                [v, v]
            })
            .collect()
    }

    fn fetch(x: &[f32]) -> impl Fn(i64, usize) -> f32 + '_ {
        move |i, c| {
            if i >= 0 && (i as usize) < x.len() / 2 {
                x[i as usize * 2 + c]
            } else {
                0.0
            }
        }
    }

    /// cargo test --release -p patch-engine sinc_speed -- --ignored --nocapture
    #[test]
    #[ignore]
    fn sinc_speed() {
        let x = tone(440.0, 48_000 * 70, 48_000.0);
        let n = 48_000 * 60;
        let mut y = vec![0.0f32; n * 2];
        let t = std::time::Instant::now();
        Sinc::new().render(&mut y, 100.0, 1.0013, fetch(&x));
        let slow = t.elapsed();
        let t = std::time::Instant::now();
        Sinc::new().render_from(&mut y, 100.0, 1.0013, &x, i64::MAX, fetch(&x));
        let fast = t.elapsed();
        println!(
            "60 s of stereo at +0.13 %: careful {slow:?} · fast {fast:?} · {:.1}x",
            slow.as_secs_f64() / fast.as_secs_f64()
        );
    }

    #[test]
    fn the_fast_path_matches_the_careful_one() {
        let x = tone(1234.5, 8192, 48_000.0);
        for step in [1.0, 1.0013, 0.97, 1.25] {
            let (mut a, mut b) = (Sinc::new(), Sinc::new());
            let (mut y1, mut y2) = (vec![0.0f32; 2048], vec![0.0f32; 2048]);
            let e1 = a.render(&mut y1, 10.3, step, fetch(&x));
            let e2 = b.render_from(&mut y2, 10.3, step, &x, i64::MAX, fetch(&x));
            assert_eq!(e1, e2);
            let worst = y1
                .iter()
                .zip(&y2)
                .map(|(p, q)| (p - q).abs())
                .fold(0.0f32, f32::max);
            assert!(worst < 1e-5, "step {step}: {worst}");
        }
        // Near the start (the careful path) and a loop end (until) as well.
        let (mut a, mut b) = (Sinc::new(), Sinc::new());
        let (mut y1, mut y2) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        a.render(&mut y1, 0.0, 1.0013, fetch(&x));
        b.render_from(&mut y2, 0.0, 1.0013, &x, 100, fetch(&x));
        let worst = y1
            .iter()
            .zip(&y2)
            .map(|(p, q)| (p - q).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-5, "{worst}");
    }

    #[test]
    fn unity_reproduces_the_source() {
        let x = tone(1000.0, 4096, 48_000.0);
        let mut s = Sinc::new();
        let mut y = vec![0.0f32; 2048 * 2];
        s.render(&mut y, 1000.0, 1.0, fetch(&x));
        for i in 0..2048 {
            assert!((y[i * 2] - x[(1000 + i) * 2]).abs() < 1e-4, "frame {i}");
        }
    }

    #[test]
    fn double_speed_doubles_the_pitch() {
        // 1 kHz at 2× reads as 2 kHz: zero crossings twice as dense.
        let x = tone(1000.0, 48_000, 48_000.0);
        let mut s = Sinc::new();
        let mut y = vec![0.0f32; 12_000 * 2];
        s.render(&mut y, 100.0, 2.0, fetch(&x));
        let crossings = y
            .as_chunks::<2>()
            .0
            .iter()
            .collect::<Vec<_>>()
            .windows(2)
            .filter(|w| (w[0][0] <= 0.0) != (w[1][0] <= 0.0))
            .count();
        // 12 000 frames at 48 kHz = 0.25 s; 2 kHz → 1000 crossings.
        assert!((crossings as i64 - 1000).abs() <= 2, "{crossings}");
    }

    #[test]
    fn speeding_up_does_not_alias() {
        // 20 kHz at 1.5× would fold down to 18 kHz… if not filtered.
        let x = tone(20_000.0, 48_000, 48_000.0);
        let mut s = Sinc::new();
        let mut y = vec![0.0f32; 8_000 * 2];
        s.render(&mut y, 200.0, 1.5, fetch(&x));
        let rms = (y
            .iter()
            .step_by(2)
            .map(|v| (*v as f64).powi(2))
            .sum::<f64>()
            / 8_000.0)
            .sqrt();
        assert!(
            20.0 * (rms / 0.353).log10() < -40.0,
            "alias at {:.1} dB",
            20.0 * (rms / 0.353).log10()
        );
    }

    #[test]
    fn fractional_position_is_accurate() {
        // A ramp, sampled half-way between frames, reads the midpoint.
        let x: Vec<f32> = (0..2048).flat_map(|i| [i as f32 * 1e-3, 0.0]).collect();
        let mut s = Sinc::new();
        let mut y = vec![0.0f32; 2];
        s.render(&mut y, 1000.5, 1.0, fetch(&x));
        assert!((y[0] - 1.0005).abs() < 1e-3, "{}", y[0]);
    }
}
