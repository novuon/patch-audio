//! Plain Rust reference implementations. The assembly kernels are tested
//! against these; they are also the fallback on CPUs without AVX2.

use crate::Stats;

pub fn stats(samples: &[f32]) -> Stats {
    let mut out = Stats::default();
    for frame in samples.as_chunks::<2>().0 {
        for ch in 0..2 {
            let x = frame[ch];
            out.peak[ch] = out.peak[ch].max(x.abs());
            out.sum_sq[ch] += x as f64 * x as f64;
        }
    }
    out
}

/// Same arithmetic, in the same order, as `patch_meter_ballistics_sse2`.
pub fn ballistics(samples: &[f32], state: &mut [f64; 4], coeff: f64) -> [f64; 2] {
    let [mut y1l, mut y1r, mut y2l, mut y2r] = *state;
    let mut max = [y2l, y2r];
    for frame in samples.as_chunks::<2>().0 {
        let (xl, xr) = ((frame[0] as f64).abs(), (frame[1] as f64).abs());
        y1l += (xl - y1l) * coeff;
        y1r += (xr - y1r) * coeff;
        y2l += (y1l - y2l) * coeff;
        y2r += (y1r - y2r) * coeff;
        max = [max[0].max(y2l), max[1].max(y2r)];
    }
    *state = [y1l, y1r, y2l, y2r];
    max
}

/// Same sums as `patch_meter_stereo_avx2`, sequentially.
pub fn stereo_sums(samples: &[f32]) -> crate::StereoSums {
    let mut o = crate::StereoSums::default();
    for f in samples.as_chunks::<2>().0 {
        let (l, r) = (f[0] as f64, f[1] as f64);
        o.sum[0] += l;
        o.sum[1] += r;
        o.sq[0] += l * l;
        o.sq[1] += r * r;
        o.lr += l * r;
    }
    o
}
