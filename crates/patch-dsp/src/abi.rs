//! The Patch DSP kernel ABI (docs/architecture/ADR-004-dsp-kernel-abi.md).
//!
//! Every implementation of every op, the Rust reference included, has this
//! one C signature, so a kernel can come from Rust, C, assembly, a
//! compiler or (one day) a generator without the engine changing:
//!
//! ```c
//! int32_t kernel(const float *const *in, float *const *out,
//!                uint32_t frames, uint32_t channels,
//!                const void *params, void *state);
//! ```
//!
//! Planar f32; pointers at least 4-byte aligned (unaligned starts must
//! work); `frames` may be 0; no partial overlap between `in` and `out`
//! (in place only where an op says so); no allocation, syscalls, locks or
//! global state; memory touched only within the buffers the op defines.
//! Returns 0, or an op-defined positive status.

use std::ffi::c_void;

pub type KernelFn = unsafe extern "C" fn(
    input: *const *const f32,
    output: *const *mut f32,
    frames: u32,
    channels: u32,
    params: *const c_void,
    state: *mut c_void,
) -> i32;

/// The ops behind the ABI. Each has a reference implementation (the
/// semantic definition) and a tolerance for optimised ones.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// `out[c][n] += in[c][n] · (g0 + n·dg)`: channel mix with a
    /// click-free linear gain ramp. Params: [`MixRamp`]. Bit-exact
    /// (fused multiply-adds in the same order as the reference).
    MixRamp,
    /// `out[c][0] = max |in[c][n]|` over the block (0 for no frames): the
    /// sample peak meters use. NaN samples are ignored, ±Inf reads Inf.
    /// Bit-exact.
    Peak,
    /// `out[0][n·2 + c] = in[c][n]` for two channels: planar stereo to
    /// interleaved (the program ring, recording, devices). Bit-exact.
    Interleave2,
}

impl Op {
    pub const ALL: [Op; 3] = [Op::MixRamp, Op::Peak, Op::Interleave2];

    pub fn name(self) -> &'static str {
        match self {
            Op::MixRamp => "mix_ramp",
            Op::Peak => "peak",
            Op::Interleave2 => "interleave2",
        }
    }

    /// Channel counts the op is defined for (None = any ≥ 1).
    pub fn channels(self) -> Option<u32> {
        match self {
            Op::Interleave2 => Some(2),
            _ => None,
        }
    }

    /// Floats per output channel buffer for `frames` input frames.
    pub fn out_len(self, frames: usize) -> usize {
        match self {
            Op::MixRamp => frames,
            Op::Peak => 1,
            Op::Interleave2 => frames * 2,
        }
    }

    /// Output buffers for `channels` input channels.
    pub fn out_channels(self, channels: usize) -> usize {
        match self {
            Op::Interleave2 => 1,
            _ => channels,
        }
    }

    /// Allowed difference from the reference, in units in the last place.
    pub fn tolerance_ulps(self) -> u32 {
        0
    }
}

/// Params of [`Op::MixRamp`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MixRamp {
    pub g0: f32,
    pub dg: f32,
}
