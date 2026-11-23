//! CPU capabilities, detected at run time (never assumed from the build
//! machine). Kernels declare what they need; the registry offers only
//! implementations this CPU can run.

/// Instruction-set features a kernel may require.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Feature {
    Sse2,
    Sse41,
    Avx,
    Avx2,
    Fma,
    Avx512f,
    Avx512bw,
    Avx512vl,
    Neon,
    Sve,
}

impl Feature {
    pub const ALL: [Feature; 10] = [
        Feature::Sse2,
        Feature::Sse41,
        Feature::Avx,
        Feature::Avx2,
        Feature::Fma,
        Feature::Avx512f,
        Feature::Avx512bw,
        Feature::Avx512vl,
        Feature::Neon,
        Feature::Sve,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Feature::Sse2 => "SSE2",
            Feature::Sse41 => "SSE4.1",
            Feature::Avx => "AVX",
            Feature::Avx2 => "AVX2",
            Feature::Fma => "FMA",
            Feature::Avx512f => "AVX-512F",
            Feature::Avx512bw => "AVX-512BW",
            Feature::Avx512vl => "AVX-512VL",
            Feature::Neon => "NEON",
            Feature::Sve => "SVE",
        }
    }

    /// Does this CPU (and OS: AVX state saving is part of the check) have it?
    pub fn present(self) -> bool {
        #[cfg(target_arch = "x86_64")]
        {
            match self {
                Feature::Sse2 => std::is_x86_feature_detected!("sse2"),
                Feature::Sse41 => std::is_x86_feature_detected!("sse4.1"),
                Feature::Avx => std::is_x86_feature_detected!("avx"),
                Feature::Avx2 => std::is_x86_feature_detected!("avx2"),
                Feature::Fma => std::is_x86_feature_detected!("fma"),
                Feature::Avx512f => std::is_x86_feature_detected!("avx512f"),
                Feature::Avx512bw => std::is_x86_feature_detected!("avx512bw"),
                Feature::Avx512vl => std::is_x86_feature_detected!("avx512vl"),
                Feature::Neon | Feature::Sve => false,
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            match self {
                Feature::Neon => std::arch::is_aarch64_feature_detected!("neon"),
                Feature::Sve => std::arch::is_aarch64_feature_detected!("sve"),
                _ => false,
            }
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            false
        }
    }
}

/// This machine: architecture, model name and the features present.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Cpu {
    pub arch: &'static str,
    pub model: String,
    pub features: Vec<Feature>,
}

pub fn this() -> Cpu {
    let model = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|t| {
            t.lines()
                .find(|l| l.starts_with("model name") || l.starts_with("Model"))
                .and_then(|l| l.split(':').nth(1))
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "unknown".into());
    Cpu {
        arch: std::env::consts::ARCH,
        model,
        features: Feature::ALL.into_iter().filter(|f| f.present()).collect(),
    }
}

/// Does the CPU have every feature in `need`?
pub fn supports(need: &[Feature]) -> bool {
    need.iter().all(|f| f.present())
}

/// Flush denormals to zero on the calling thread (x86-64: MXCSR FTZ + DAZ;
/// AArch64: FPCR FZ). Real-time threads call it: a decaying IIR state that
/// reaches the denormal range otherwise stays there and costs ~100x per
/// operation (a 4-band EQ after a pause: 12 µs → 1.5 ms per 1024 frames).
/// Cheap enough to call every cycle; it writes the register only when a
/// flag is missing (a plugin may have cleared it).
#[inline]
pub fn flush_denormals() {
    #[cfg(target_arch = "x86_64")]
    {
        #[allow(deprecated)]
        use std::arch::x86_64::{_mm_getcsr, _mm_setcsr};
        const FTZ_DAZ: u32 = 0x8040;
        // SAFETY: reading and setting MXCSR's flush flags only changes how
        // this thread rounds results below the normal range.
        #[allow(deprecated)]
        unsafe {
            let csr = _mm_getcsr();
            if csr & FTZ_DAZ != FTZ_DAZ {
                _mm_setcsr(csr | FTZ_DAZ);
            }
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        const FZ: u64 = 1 << 24;
        let fpcr: u64;
        // SAFETY: FPCR's FZ flag only changes how this thread rounds results
        // below the normal range.
        unsafe {
            std::arch::asm!("mrs {}, fpcr", out(reg) fpcr, options(nomem, nostack, preserves_flags));
            if fpcr & FZ == 0 {
                std::arch::asm!("msr fpcr, {}", in(reg) fpcr | FZ, options(nomem, nostack, preserves_flags));
            }
        }
    }
}

#[cfg(test)]
mod denormal_tests {
    #[test]
    fn a_flushed_thread_rounds_denormals_to_zero() {
        let tiny = |x: f64| std::hint::black_box(x) * std::hint::black_box(0.25);
        std::thread::spawn(move || {
            assert!(tiny(f64::MIN_POSITIVE) > 0.0, "a fresh thread keeps denormals");
            super::flush_denormals();
            assert_eq!(tiny(f64::MIN_POSITIVE), 0.0);
            let f = std::hint::black_box(f32::MIN_POSITIVE) * std::hint::black_box(0.25f32);
            assert_eq!(f, 0.0);
            // Normal numbers are untouched.
            assert_eq!(tiny(1.0), 0.25);
        })
        .join()
        .unwrap();
    }
}
