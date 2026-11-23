//! Patch recording unit.
//!
//! * [`convert`]: float32 → 24-bit PCM with TPDF dither, clip count and peak in
//!   one pass (AVX2 assembly, `x86_64.s`), bit-identical to [`reference`].
//! * [`wav`]: crash-safe Broadcast WAV writer that becomes RF64 past 4 GiB.
//! * [`daemon`]: the detached recorder process, controlled over a socket,
//!   so a take continues when Patch's UI restarts or crashes.

pub mod daemon;
pub mod reference;
pub mod wav;

#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(include_str!("x86_64.s"), options(raw));

/// Running statistics of the conversion.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ConvStats {
    /// Samples with |x| > 1.0 before conversion (they clip).
    pub clips: u64,
    /// Max |x| seen.
    pub peak: f32,
    _pad: u32,
}

#[cfg(target_arch = "x86_64")]
unsafe extern "sysv64" {
    fn patch_rec_s24_avx2(
        src: *const f32,
        n: usize,
        dst: *mut u8,
        rng: *mut [u32; 8],
        dither: *const f32,
        st: *mut ConvStats,
    );
}

pub(crate) fn has_avx2() -> bool {
    patch_dsp::allows(patch_dsp::Tier::Avx2)
}

pub fn backend() -> &'static str {
    patch_dsp::name()
}

/// Dither state: eight xorshift32 generators, one per SIMD lane.
#[derive(Debug, Clone)]
pub struct Dither {
    pub rng: [u32; 8],
    /// Peak of each uniform component in LSB; 1.0 = standard TPDF (±1 LSB).
    pub amount: f32,
}

impl Dither {
    pub fn tpdf(seed: u32) -> Self {
        let mut rng = [0u32; 8];
        let mut x = seed.max(1);
        for r in &mut rng {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *r = x.max(1);
        }
        Dither { rng, amount: 1.0 }
    }

    pub fn none() -> Self {
        Dither {
            amount: 0.0,
            ..Dither::tpdf(1)
        }
    }
}

/// Convert `src` (any channel count, interleaved) to packed 24-bit PCM,
/// appending to `out`. Updates `stats` and the dither state.
pub fn convert_s24(src: &[f32], dither: &mut Dither, stats: &mut ConvStats, out: &mut Vec<u8>) {
    let fast = if has_avx2() { src.len() / 8 * 8 } else { 0 };
    let start = out.len();
    out.resize(start + src.len() * 3 + 4, 0); // +4: the kernel's overlapping stores
    #[cfg(target_arch = "x86_64")]
    if fast > 0 {
        // SAFETY: reads `fast` floats, writes fast·3 (+4 slack) bytes inside
        // the buffer resized above, updates 8 rng words and the stats.
        unsafe {
            patch_rec_s24_avx2(
                src.as_ptr(),
                fast,
                out[start..].as_mut_ptr(),
                &mut dither.rng,
                &dither.amount,
                stats,
            )
        };
    }
    reference::convert_s24_lanes(
        &src[fast..],
        dither,
        stats,
        &mut out[start + fast * 3..],
        fast,
    );
    out.truncate(start + src.len() * 3);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| ((i as f32) * 0.013).sin() * 1.1 + if i % 97 == 0 { 0.5 } else { 0.0 })
            .collect()
    }

    #[test]
    fn kernel_is_bit_identical_to_reference() {
        for n in [0, 7, 8, 9, 64, 1000, 48_000] {
            let s = signal(n);
            let (mut da, mut db) = (Dither::tpdf(42), Dither::tpdf(42));
            let (mut sa, mut sb) = (ConvStats::default(), ConvStats::default());
            let (mut oa, mut ob) = (vec![], vec![]);
            convert_s24(&s, &mut da, &mut sa, &mut oa);
            reference::convert_s24(&s, &mut db, &mut sb, &mut ob);
            assert_eq!(oa, ob, "bytes, n = {n}");
            assert_eq!(sa, sb, "stats, n = {n}");
            assert_eq!(da.rng, db.rng, "rng, n = {n}");
        }
    }

    #[test]
    fn full_scale_and_clipping() {
        let s = [1.0f32, -1.0, 2.0, -2.0, 0.5, 0.0, 0.25, -0.25];
        let mut d = Dither::none();
        let mut st = ConvStats::default();
        let mut out = vec![];
        convert_s24(&s, &mut d, &mut st, &mut out);
        let v = |i: usize| {
            let b = &out[i * 3..i * 3 + 3];
            (((b[0] as u32) | (b[1] as u32) << 8 | (b[2] as u32) << 16) << 8) as i32 >> 8
        };
        assert_eq!(v(0), 8_388_607); // +1.0 clamps to max
        assert_eq!(v(1), -8_388_608);
        assert_eq!(v(2), 8_388_607);
        assert_eq!(v(3), -8_388_608);
        assert_eq!(v(4), 4_194_304);
        assert_eq!(v(5), 0);
        assert_eq!(st.clips, 2);
        assert_eq!(st.peak, 2.0);
    }

    #[test]
    fn tpdf_dither_is_triangular_and_unbiased() {
        // Silence in, dither only out: values in {−1, 0, +1}, mean ≈ 0,
        // P(0) ≈ 1/2 for ±1 LSB triangular noise.
        let s = vec![0.0f32; 80_000];
        let mut d = Dither::tpdf(7);
        let mut st = ConvStats::default();
        let mut out = vec![];
        convert_s24(&s, &mut d, &mut st, &mut out);
        let vals: Vec<i32> = out
            .chunks(3)
            .map(|b| (((b[0] as u32) | (b[1] as u32) << 8 | (b[2] as u32) << 16) << 8) as i32 >> 8)
            .collect();
        assert!(vals.iter().all(|v| (-1..=1).contains(v)));
        let mean = vals.iter().map(|v| *v as f64).sum::<f64>() / vals.len() as f64;
        let zeros = vals.iter().filter(|v| **v == 0).count() as f64 / vals.len() as f64;
        assert!(mean.abs() < 0.01, "mean {mean}");
        assert!((zeros - 0.75).abs() < 0.02, "P(0) = {zeros}");
    }
}

mod native;

pub mod mic;
