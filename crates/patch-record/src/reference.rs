//! Rust reference for the conversion kernel: same float operations, same
//! order, same per-lane generators.

use crate::{ConvStats, Dither};

fn step(x: &mut u32) -> f32 {
    *x ^= *x << 13;
    *x ^= *x >> 17;
    *x ^= *x << 5;
    f32::from_bits((*x >> 9) | 0x3f80_0000) - 1.0
}

fn convert_one(x: f32, rng: &mut u32, amount: f32, stats: &mut ConvStats) -> i32 {
    let a = x.abs();
    stats.peak = stats.peak.max(a);
    if a > 1.0 {
        stats.clips += 1;
    }
    let u1 = step(rng);
    let u2 = step(rng);
    let d = (u1 - u2) * amount;
    // min/max, not clamp: the oracle keeps its NaN behaviour.
    #[allow(clippy::manual_clamp)]
    let y = (x * 8_388_608.0 + d).min(8_388_607.0).max(-8_388_608.0);
    y.round_ties_even() as i32
}

/// Samples starting at absolute index `offset` use lane `(offset + i) % 8`,
/// so the kernel's head and this tail continue the same generators.
pub fn convert_s24_lanes(
    src: &[f32],
    dither: &mut Dither,
    stats: &mut ConvStats,
    out: &mut [u8],
    offset: usize,
) {
    for (i, x) in src.iter().enumerate() {
        let lane = (offset + i) % 8;
        let v = convert_one(*x, &mut dither.rng[lane], dither.amount, stats);
        out[i * 3..i * 3 + 3].copy_from_slice(&v.to_le_bytes()[..3]);
    }
}

pub fn convert_s24(src: &[f32], dither: &mut Dither, stats: &mut ConvStats, out: &mut Vec<u8>) {
    let start = out.len();
    out.resize(start + src.len() * 3, 0);
    convert_s24_lanes(src, dither, stats, &mut out[start..], 0);
}
