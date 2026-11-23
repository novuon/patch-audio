//! Every implementation of every op, with what it needs from the CPU, and
//! the table the engine calls through.
//!
//! The table is built once, outside the real-time thread (at engine start),
//! from: the CPU's features, the global tier override (`PATCH_DSP`), and
//! the installation's profile written by `omarchy-patch optimize`. An
//! implementation the CPU can't run is never selected; the reference is
//! always there.

use crate::abi::{KernelFn, Op};
use crate::cpu::Feature;

#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(include_str!("x86_64/avx2/mix_ramp.s"), options(raw));
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(include_str!("x86_64/avx2/peak.s"), options(raw));

#[cfg(target_arch = "x86_64")]
unsafe extern "C" {
    fn patch_k_mix_ramp_avx2(
        i: *const *const f32,
        o: *const *mut f32,
        f: u32,
        c: u32,
        p: *const std::ffi::c_void,
        s: *mut std::ffi::c_void,
    ) -> i32;
    fn patch_k_peak_avx2(
        i: *const *const f32,
        o: *const *mut f32,
        f: u32,
        c: u32,
        p: *const std::ffi::c_void,
        s: *mut std::ffi::c_void,
    ) -> i32;
}

/// One implementation of an op.
#[derive(Clone, Copy, Debug)]
pub struct Impl {
    pub op: Op,
    /// "reference", "avx2", …
    pub name: &'static str,
    /// Where it comes from: "rust", "asm", "generated".
    pub source: &'static str,
    pub needs: &'static [Feature],
    pub f: KernelFn,
}

impl Impl {
    pub fn supported(&self) -> bool {
        crate::cpu::supports(self.needs)
    }

    pub fn is_reference(&self) -> bool {
        self.name == "reference"
    }

    /// The tier this kernel belongs to, from what it needs: AVX or later
    /// is the AVX2 tier, any other SIMD the baseline (SSE2) tier.
    pub fn tier(&self) -> crate::Tier {
        use crate::cpu::Feature as F;
        if self.is_reference() {
            crate::Tier::Reference
        } else if self.needs.iter().any(|f| {
            matches!(
                f,
                F::Avx | F::Avx2 | F::Fma | F::Avx512f | F::Avx512bw | F::Avx512vl
            )
        }) {
            crate::Tier::Avx2
        } else {
            crate::Tier::Sse2
        }
    }
}

/// All implementations compiled into this build (supported or not).
///
/// Not here, by measurement: an AVX interleave2 (0.92× the reference,
/// which the compiler already vectorises; see docs/bench.md).
pub fn all() -> Vec<Impl> {
    let mut v = vec![
        Impl {
            op: Op::MixRamp,
            name: "reference",
            source: "rust",
            needs: &[],
            f: crate::reference::mix_ramp,
        },
        Impl {
            op: Op::Peak,
            name: "reference",
            source: "rust",
            needs: &[],
            f: crate::reference::peak,
        },
        Impl {
            op: Op::Interleave2,
            name: "reference",
            source: "rust",
            needs: &[],
            f: crate::reference::interleave2,
        },
    ];
    #[cfg(target_arch = "x86_64")]
    v.extend([
        Impl {
            op: Op::MixRamp,
            name: "avx2",
            source: "asm",
            needs: &[Feature::Avx2, Feature::Fma],
            f: patch_k_mix_ramp_avx2,
        },
        Impl {
            op: Op::Peak,
            name: "avx2",
            source: "asm",
            needs: &[Feature::Avx],
            f: patch_k_peak_avx2,
        },
    ]);
    v
}

/// Implementations of `op` this CPU can run, reference first.
pub fn candidates(op: Op) -> Vec<Impl> {
    all()
        .into_iter()
        .filter(|i| i.op == op && i.supported())
        .collect()
}

pub fn reference(op: Op) -> Impl {
    all()
        .into_iter()
        .find(|i| i.op == op && i.is_reference())
        .expect("every op has a reference")
}

/// The selected implementation per op (index = `Op as usize`).
#[derive(Clone, Copy, Debug)]
pub struct Table {
    pub mix_ramp: Impl,
    pub peak: Impl,
    pub interleave2: Impl,
}

impl Table {
    /// The table for this machine: the profile's choice where it names a
    /// supported implementation; else the fastest known tier; the
    /// reference under `PATCH_DSP=reference`.
    pub fn build(profile: Option<&crate::profile::Profile>) -> Table {
        Self::build_for_tier(profile, crate::tier())
    }

    /// Explicit tier for isolated benchmarks and selection tests.
    pub fn build_for_tier(profile: Option<&crate::profile::Profile>, tier: crate::Tier) -> Table {
        let pick = |op: Op| -> Impl {
            // Only kernels of this tier or below (an SSE2 kernel stays
            // available under the SSE2 limit; AVX2 ones don't).
            let cands: Vec<_> = candidates(op)
                .into_iter()
                .filter(|i| i.tier() <= tier)
                .collect();
            if tier == crate::Tier::Reference {
                return reference(op);
            }
            if let Some(name) = profile.and_then(|p| p.selected(op))
                && let Some(i) = cands.iter().find(|i| i.name == name)
            {
                return *i;
            }
            // No profile: the best tier this CPU runs (asm over reference).
            cands
                .iter()
                .rev()
                .find(|i| !i.is_reference())
                .copied()
                .unwrap_or_else(|| reference(op))
        };
        Table {
            mix_ramp: pick(Op::MixRamp),
            peak: pick(Op::Peak),
            interleave2: pick(Op::Interleave2),
        }
    }

    pub fn get(&self, op: Op) -> Impl {
        match op {
            Op::MixRamp => self.mix_ramp,
            Op::Peak => self.peak,
            Op::Interleave2 => self.interleave2,
        }
    }
}

static TABLE: std::sync::OnceLock<Table> = std::sync::OnceLock::new();

/// The process-wide table: built on first use (call [`init`] before the
/// real-time thread starts so that first use isn't in the callback).
pub fn table() -> &'static Table {
    TABLE.get_or_init(|| Table::build(crate::profile::load().as_ref()))
}

/// Build the table now (engine start), from the installation's profile.
pub fn init() -> &'static Table {
    table()
}

// Safe wrappers the engine calls (slices in, the ABI underneath).

/// `out += input · (g0 + n·dg)`.
#[inline]
pub fn mix_ramp(out: &mut [f32], input: &[f32], g0: f32, dg: f32) {
    let n = out.len().min(input.len());
    let p = crate::abi::MixRamp { g0, dg };
    let (i, o) = ([input.as_ptr()], [out.as_mut_ptr()]);
    // SAFETY: one channel of `n` floats each; params is a MixRamp.
    unsafe {
        (table().mix_ramp.f)(
            i.as_ptr(),
            o.as_ptr(),
            n as u32,
            1,
            &p as *const _ as *const _,
            std::ptr::null_mut(),
        )
    };
}

/// Sample peak |x| of one channel.
#[inline]
pub fn peak(x: &[f32]) -> f32 {
    let mut m = 0.0f32;
    let (i, o) = ([x.as_ptr()], [&mut m as *mut f32]);
    // SAFETY: one channel of x.len() floats; one output float.
    unsafe {
        (table().peak.f)(
            i.as_ptr(),
            o.as_ptr(),
            x.len() as u32,
            1,
            std::ptr::null(),
            std::ptr::null_mut(),
        )
    };
    m
}

/// Planar stereo → interleaved (`out` holds 2 · frames floats).
#[inline]
pub fn interleave2(l: &[f32], r: &[f32], out: &mut [f32]) {
    let n = l.len().min(r.len()).min(out.len() / 2);
    let (i, o) = ([l.as_ptr(), r.as_ptr()], [out.as_mut_ptr()]);
    // SAFETY: two channels of n floats; out holds 2n floats.
    unsafe {
        (table().interleave2.f)(
            i.as_ptr(),
            o.as_ptr(),
            n as u32,
            2,
            std::ptr::null(),
            std::ptr::null_mut(),
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Choice, Profile};

    fn profile_selecting(name: &str) -> Profile {
        Profile {
            patch_version: "test".into(),
            arch: std::env::consts::ARCH.into(),
            features: vec![],
            env: crate::bench::env(false),
            choices: Op::ALL
                .iter()
                .map(|&op| Choice {
                    op,
                    selected: name.into(),
                    measured: vec![],
                    rejected: vec![],
                })
                .collect(),
        }
    }

    #[test]
    fn an_unknown_or_unsupported_selection_falls_back() {
        // A profile naming an implementation this build/CPU doesn't have.
        let t = Table::build(Some(&profile_selecting("sve2-generated")));
        for op in Op::ALL {
            assert!(
                t.get(op).supported(),
                "{}: never an unsupported kernel",
                op.name()
            );
        }
        let t = Table::build(Some(&profile_selecting("reference")));
        assert!(
            t.get(Op::MixRamp).is_reference(),
            "the profile's reference choice holds"
        );
    }

    #[test]
    fn a_kernel_counts_by_what_it_needs() {
        let sse2 = Impl {
            op: Op::Peak,
            name: "sse2",
            source: "test",
            needs: &[Feature::Sse2],
            f: crate::reference::peak,
        };
        let avx2 = Impl {
            op: Op::Peak,
            name: "avx2",
            source: "test",
            needs: &[Feature::Avx2, Feature::Fma],
            f: crate::reference::peak,
        };
        assert_eq!(sse2.tier(), crate::Tier::Sse2, "kept under the SSE2 limit");
        assert_eq!(avx2.tier(), crate::Tier::Avx2);
        assert!(sse2.tier() <= crate::Tier::Sse2 && avx2.tier() > crate::Tier::Sse2);
    }

    #[test]
    fn tier_limit_overrides_profiles() {
        for tier in [crate::Tier::Reference, crate::Tier::Sse2] {
            let table = Table::build_for_tier(Some(&profile_selecting("avx2")), tier);
            assert!(Op::ALL.iter().all(|&op| table.get(op).is_reference()));
        }
        let table = Table::build_for_tier(None, crate::Tier::Avx2);
        assert!(Op::ALL.iter().all(|&op| table.get(op).supported()));
    }

    #[test]
    fn unsupported_isas_are_never_candidates() {
        let foreign = if cfg!(target_arch = "x86_64") {
            Feature::Sve
        } else {
            Feature::Avx2
        };
        let imp = Impl {
            op: Op::Peak,
            name: "foreign",
            source: "test",
            needs: &[Feature::Sve, Feature::Avx2],
            f: crate::reference::peak,
        };
        assert!(!foreign.present());
        assert!(!imp.supported());
        for op in Op::ALL {
            assert!(candidates(op).iter().all(|i| i.supported()));
            assert!(
                candidates(op).iter().any(|i| i.is_reference()),
                "the reference is always a candidate"
            );
        }
    }

    #[test]
    fn safe_wrappers_match_the_reference() {
        let x: Vec<f32> = (0..1027).map(|i| ((i as f32) * 0.37).sin()).collect();
        let y: Vec<f32> = (0..1027).map(|i| ((i as f32) * 0.11).cos()).collect();
        let want = x
            .iter()
            .fold(0.0f32, |m, v| if v.abs() > m { v.abs() } else { m });
        assert_eq!(peak(&x), want);
        let mut o = vec![0.0f32; 2054];
        interleave2(&x, &y, &mut o);
        assert!(
            o.chunks(2)
                .zip(x.iter().zip(&y))
                .all(|(p, (a, b))| p[0] == *a && p[1] == *b)
        );
        let mut m = y.clone();
        mix_ramp(&mut m, &x, 0.5, 0.001);
        let mut r = y.clone();
        for (n, (o, v)) in r.iter_mut().zip(&x).enumerate() {
            *o = v.mul_add((n as f32).mul_add(0.001, 0.5), *o);
        }
        assert_eq!(m, r);
    }
}
