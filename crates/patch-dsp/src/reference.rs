//! Reference implementations: portable Rust, the semantic definition of
//! each op, behind the same ABI as every optimised kernel.

use crate::abi::MixRamp;
use std::ffi::c_void;

/// # Safety
/// The ABI contract (abi.rs): `channels` valid `in`/`out` pointers of
/// `frames` floats each; `params` points to a [`MixRamp`].
pub unsafe extern "C" fn mix_ramp(
    input: *const *const f32,
    output: *const *mut f32,
    frames: u32,
    channels: u32,
    params: *const c_void,
    _state: *mut c_void,
) -> i32 {
    let p = unsafe { *(params as *const MixRamp) };
    for c in 0..channels as usize {
        let (x, o) = unsafe {
            (
                std::slice::from_raw_parts(*input.add(c), frames as usize),
                std::slice::from_raw_parts_mut(*output.add(c), frames as usize),
            )
        };
        for (n, (o, x)) in o.iter_mut().zip(x).enumerate() {
            let g = (n as f32).mul_add(p.dg, p.g0);
            *o = x.mul_add(g, *o);
        }
    }
    0
}

/// # Safety
/// ABI contract; each `out[c]` points to at least one float.
pub unsafe extern "C" fn peak(
    input: *const *const f32,
    output: *const *mut f32,
    frames: u32,
    channels: u32,
    _params: *const c_void,
    _state: *mut c_void,
) -> i32 {
    for c in 0..channels as usize {
        let x = unsafe { std::slice::from_raw_parts(*input.add(c), frames as usize) };
        let mut m = 0.0f32;
        for v in x {
            let a = v.abs();
            // NaN compares false: ignored, like the vector kernels.
            if a > m {
                m = a;
            }
        }
        unsafe { **output.add(c) = m };
    }
    0
}

/// # Safety
/// ABI contract; two input channels; `out[0]` holds `frames · 2` floats.
pub unsafe extern "C" fn interleave2(
    input: *const *const f32,
    output: *const *mut f32,
    frames: u32,
    channels: u32,
    _params: *const c_void,
    _state: *mut c_void,
) -> i32 {
    if channels != 2 {
        return -1;
    }
    let n = frames as usize;
    let (l, r, o) = unsafe {
        (
            std::slice::from_raw_parts(*input, n),
            std::slice::from_raw_parts(*input.add(1), n),
            std::slice::from_raw_parts_mut(*output, n * 2),
        )
    };
    for i in 0..n {
        o[i * 2] = l[i];
        o[i * 2 + 1] = r[i];
    }
    0
}
