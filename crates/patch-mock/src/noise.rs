//! Cheap deterministic "signal" generators. No RNG state, so any frame can be
//! reproduced from its timestamp alone.

/// Smooth pseudo-random signal in 0..1.
pub fn wave(seed: u32, t: f64) -> f32 {
    let s = seed as f64 * 0.731;
    let v = (t * 1.7 + s).sin() * 0.5
        + (t * 3.1 + s * 2.3).sin() * 0.3
        + (t * 7.9 + s * 0.7).sin() * 0.2;
    ((v * 0.5 + 0.5) as f32).clamp(0.0, 1.0)
}
