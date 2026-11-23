//! The correctness harness: every implementation against its op's
//! reference, over the cases that break kernels — lengths around the vector
//! width, unaligned starts, silence, full scale, impulses, sines, noise,
//! denormals, NaN and Inf, several channel counts — with canaries after
//! every buffer to catch out-of-bounds writes. Used by the test suite and by
//! `omarchy-patch optimize` on the machine it will run on.

use crate::abi::{MixRamp, Op};
use crate::registry::Impl;

/// Why an implementation failed (the first failure).
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    pub case: String,
    pub detail: String,
}

/// Frame counts: empty, tiny, around 8/16/32-wide vectors, typical periods,
/// large, odd.
pub const LENGTHS: [usize; 24] = [
    0, 1, 2, 3, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 255, 256, 511, 1024, 4099,
    8192,
];

const CANARY: f32 = 1.234_567e-7;
const PAD: usize = 16;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        // xorshift64*: reproducible, no dependency.
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let v = self.0.wrapping_mul(0x2545_f491_4f6c_dd1d);
        (v >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    }
}

/// Signal generators by name.
fn signal(kind: &str, n: usize, ch: usize, rng: &mut Rng) -> Vec<f32> {
    (0..n)
        .map(|i| match kind {
            "silence" => 0.0,
            "full" => {
                if (i + ch).is_multiple_of(2) {
                    1.0
                } else {
                    -1.0
                }
            }
            "impulse" => {
                if i == n / 2 {
                    1.0
                } else {
                    0.0
                }
            }
            "sine" => ((i as f32) * 0.031 + ch as f32).sin() * 0.7,
            "denormal" => {
                if i % 3 == 0 {
                    1.0e-40
                } else {
                    -2.0e-39
                }
            }
            "special" => match i % 7 {
                0 => f32::NAN,
                1 => f32::INFINITY,
                2 => f32::NEG_INFINITY,
                3 => -0.0,
                _ => rng.next(),
            },
            _ => rng.next(),
        })
        .collect()
}

const SIGNALS: [&str; 7] = [
    "silence", "full", "impulse", "sine", "noise", "denormal", "special",
];

fn same(a: f32, b: f32, ulps: u32) -> bool {
    if a.is_nan() || b.is_nan() {
        return a.is_nan() && b.is_nan();
    }
    if a == b {
        return true;
    }
    let (ia, ib) = (a.to_bits() as i64, b.to_bits() as i64);
    (ia - ib).unsigned_abs() <= ulps as u64 && a.is_sign_negative() == b.is_sign_negative()
}

/// Run `imp` and the reference on the same inputs; compare.
fn run_case(
    imp: &Impl,
    reference: &Impl,
    frames: usize,
    channels: usize,
    offset: usize,
    kind: &str,
    rng: &mut Rng,
) -> Result<(), Failure> {
    let op = imp.op;
    let ins: Vec<Vec<f32>> = (0..channels)
        .map(|c| {
            let mut v = vec![0.0f32; offset];
            v.extend(signal(kind, frames, c, rng));
            v
        })
        .collect();
    let out_len = op.out_len(frames);
    let outs_init: Vec<Vec<f32>> = (0..op.out_channels(channels))
        .map(|c| {
            let mut v = vec![0.0f32; offset];
            v.extend((0..out_len).map(|i| {
                if op == Op::MixRamp {
                    ((i + c) as f32 * 0.013).cos() * 0.5
                } else {
                    0.0
                }
            }));
            v.extend([CANARY; PAD]);
            v
        })
        .collect();
    let params = MixRamp {
        g0: 0.8,
        dg: -0.37 / frames.max(1) as f32,
    };
    let call = |f: crate::abi::KernelFn| -> (Vec<Vec<f32>>, i32) {
        let mut outs = outs_init.clone();
        let ip: Vec<*const f32> = ins.iter().map(|v| v[offset..].as_ptr()).collect();
        let op_: Vec<*mut f32> = outs.iter_mut().map(|v| v[offset..].as_mut_ptr()).collect();
        // SAFETY: buffers sized per the op's contract (plus canaries).
        let r = unsafe {
            f(
                ip.as_ptr(),
                op_.as_ptr(),
                frames as u32,
                channels as u32,
                &params as *const _ as *const _,
                std::ptr::null_mut(),
            )
        };
        (outs, r)
    };
    let (want, rw) = call(reference.f);
    let (got, rg) = call(imp.f);
    let case = format!(
        "{} frames, {} ch, offset {}, {}",
        frames, channels, offset, kind
    );
    if rw != rg {
        return Err(Failure {
            case,
            detail: format!("status {rg}, reference {rw}"),
        });
    }
    for (c, (w, g)) in want.iter().zip(&got).enumerate() {
        for (i, (a, b)) in w.iter().zip(g).enumerate() {
            let in_bounds = i >= offset && i < offset + out_len;
            if !in_bounds && b.to_bits() != a.to_bits() {
                return Err(Failure {
                    case,
                    detail: format!("wrote outside its buffer (channel {c}, index {i})"),
                });
            }
            if in_bounds && !same(*a, *b, op.tolerance_ulps()) {
                return Err(Failure {
                    case,
                    detail: format!("channel {c} sample {}: {b:e}, reference {a:e}", i - offset),
                });
            }
        }
    }
    Ok(())
}

/// Verify one implementation. `quick` runs fewer randomized rounds.
pub fn verify(imp: &Impl, quick: bool) -> Result<usize, Failure> {
    let reference = crate::registry::reference(imp.op);
    let chans: Vec<usize> = match imp.op.channels() {
        Some(c) => vec![c as usize],
        None => vec![1, 2, 3, 8],
    };
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    let mut cases = 0;
    let rounds = if quick { 1 } else { 4 };
    for _ in 0..rounds {
        for &frames in &LENGTHS {
            for &channels in &chans {
                for offset in [0usize, 1, 3] {
                    for kind in SIGNALS {
                        run_case(imp, &reference, frames, channels, offset, kind, &mut rng)?;
                        cases += 1;
                    }
                }
            }
        }
    }
    Ok(cases)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_implementation_matches_its_reference() {
        for imp in crate::registry::all().into_iter().filter(|i| i.supported()) {
            match verify(&imp, false) {
                Ok(n) => assert!(n > 1000, "{} {} ran {n} cases", imp.op.name(), imp.name),
                Err(f) => panic!("{} {}: {} — {}", imp.op.name(), imp.name, f.case, f.detail),
            }
        }
    }

    #[test]
    fn the_harness_catches_a_wrong_kernel_and_an_overrun() {
        unsafe extern "C" fn off_by_one(
            i: *const *const f32,
            o: *const *mut f32,
            f: u32,
            c: u32,
            p: *const std::ffi::c_void,
            s: *mut std::ffi::c_void,
        ) -> i32 {
            // Correct, then one float too far.
            let r = unsafe { crate::reference::peak(i, o, f, c, p, s) };
            unsafe { *(*o).add(1) = 9.0 };
            r
        }
        unsafe extern "C" fn wrong(
            _: *const *const f32,
            o: *const *mut f32,
            _: u32,
            c: u32,
            _: *const std::ffi::c_void,
            _: *mut std::ffi::c_void,
        ) -> i32 {
            for k in 0..c as usize {
                unsafe { **o.add(k) = 0.5 };
            }
            0
        }
        let mk = |f| Impl {
            op: Op::Peak,
            name: "bad",
            source: "test",
            needs: &[],
            f,
        };
        assert!(
            verify(&mk(off_by_one), true)
                .unwrap_err()
                .detail
                .contains("outside")
        );
        assert!(verify(&mk(wrong), true).is_err());
    }
}
