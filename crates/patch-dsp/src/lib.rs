//! One runtime dispatch for every DSP kernel in Patch.
//!
//! Each kernel exists as a Rust reference (the oracle) and as hand-written
//! x86-64 assembly for a CPU tier. The tier is detected once; every kernel
//! asks [`allows`] before taking its assembly path, so the whole of Patch
//! runs on one consistent tier.
//!
//! `PATCH_DSP=reference|sse2|avx2|auto` overrides the choice (never above
//! what the CPU supports), e.g. to compare against the oracle or to rule
//! out a kernel when chasing a problem. [`set`] does the same at run time
//! (benchmarks, tests).

pub mod abi;
pub mod autoeq;
pub mod bench;
pub mod cpu;
pub mod filter;
pub mod profile;
pub mod reference;
pub mod registry;
pub mod verify;

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Tier {
    /// Portable Rust: the oracle every kernel is tested against.
    Reference = 1,
    /// x86-64 baseline.
    Sse2 = 2,
    /// AVX2 + FMA (Haswell and later, Zen and later).
    Avx2 = 3,
}

impl Tier {
    pub fn name(self) -> &'static str {
        match self {
            Tier::Reference => "Rust reference",
            Tier::Sse2 => "x86-64 SSE2 assembly",
            Tier::Avx2 => "x86-64 AVX2+FMA assembly",
        }
    }

    fn from_u8(v: u8) -> Tier {
        match v {
            3 => Tier::Avx2,
            2 => Tier::Sse2,
            _ => Tier::Reference,
        }
    }
}

/// The best tier this CPU runs.
pub fn detected() -> Tier {
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
            return Tier::Avx2;
        }
        Tier::Sse2
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        Tier::Reference
    }
}

static TIER: AtomicU8 = AtomicU8::new(0);

fn from_env() -> Tier {
    let best = detected();
    let want = match std::env::var("PATCH_DSP").as_deref() {
        Ok("reference") => Tier::Reference,
        Ok("sse2") => Tier::Sse2,
        Ok("avx2") => Tier::Avx2,
        _ => best,
    };
    want.min(best)
}

/// The tier in use.
pub fn tier() -> Tier {
    match TIER.load(Ordering::Relaxed) {
        0 => {
            let t = from_env();
            TIER.store(t as u8, Ordering::Relaxed);
            t
        }
        v => Tier::from_u8(v),
    }
}

/// May a kernel of tier `t` run?
#[inline]
pub fn allows(t: Tier) -> bool {
    tier() >= t
}

/// Use `t` from now on (capped at what the CPU supports). Returns the tier
/// actually set.
pub fn set(t: Tier) -> Tier {
    let t = t.min(detected());
    TIER.store(t as u8, Ordering::Relaxed);
    t
}

pub fn name() -> &'static str {
    tier().name()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_is_capped() {
        let best = detected();
        assert_eq!(set(Tier::Reference), Tier::Reference);
        assert!(!allows(Tier::Sse2) || best == Tier::Reference);
        assert_eq!(set(Tier::Avx2), best.min(Tier::Avx2));
        assert!(allows(Tier::Reference));
    }
}

pub mod voice;

pub mod voice_noise;
