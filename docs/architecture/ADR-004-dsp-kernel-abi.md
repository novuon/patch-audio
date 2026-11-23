# ADR-004: DSP kernel ABI, CPU dispatch and the assembly policy

Status: Accepted (ABI and harness being introduced; existing kernels migrate)

## Principle

Patch uses high-level languages to express architecture and semantics,
while performance-critical kernels are replaceable by architecture-specific
assembly or machine-generated implementations through a stable kernel ABI.
Assembly isn't a goal in itself. It has to lower latency, CPU use or
jitter, or raise throughput, measurably.

## The kernel ABI

A kernel is a pure function over caller-owned memory:

```c
// C view; Rust uses extern "sysv64" (x86-64) / "C" (AArch64).
int32_t patch_k_<op>_<variant>(
    const float *const *in,   // in[channel] -> frames samples
    float *const *out,        // out[channel] -> frames samples
    uint32_t frames,
    uint32_t channels,
    const void *params,       // op-specific POD struct, read-only
    void *state);             // op-specific POD struct, kernel-owned, or NULL
```

Contract (same for every implementation of an op):

* **Calling convention:** the platform C ABI (System V AMD64, AAPCS64). No
  vector registers are preserved beyond what that ABI requires; the kernel
  saves any callee-saved registers it uses.
* **Format:** 32-bit IEEE float, planar (one pointer per channel).
  Interleaved I/O is its own op (`interleave`, `deinterleave`).
* **Alignment:** pointers are at least 4-byte aligned. Kernels must accept
  any `frames` (0 included) and unaligned starts, using unaligned loads or
  a scalar head and tail.
* **Aliasing:** `out[c] == in[c]` (in place) is allowed where the op says
  so; partial overlap never is.
* **Ownership:** the caller owns every buffer. `state` is allocated,
  zeroed and destroyed by the caller; its size is part of the op.
* **Effects:** no allocation, syscalls, locks, I/O or global state. Memory
  is touched only within `[ptr, ptr + frames)` per channel, `params` and
  `state`.
* **Numerics:** denormals are flushed by the caller's thread (FTZ/DAZ on
  the RT thread); a kernel may not depend on either. NaN/Inf propagate like
  the reference (no kernel may turn NaN into a finite value unless the op is
  a limiter/clamp that defines it). Clipping happens only in ops that say
  they clip.
* **Latency:** a kernel adds none, unless its op declares a fixed latency in
  frames (e.g. an oversampler).
* **Result:** 0 = ok; a positive op-defined status (e.g. clip count); never
  negative except for "unsupported" at probe time.

Each op has a **reference implementation** in portable Rust. It is the
semantic definition. An optimised implementation must match it within the
op's stated tolerance: bit-exact where the op is a copy or conversion, or
where we have made it so (gain-ramp mix, VU ballistics); a stated relative
or absolute error otherwise (FMA contraction, vector reduction order).

## Dispatch

`patch-dsp` detects CPU features once (x86-64: SSE2, SSE4.1, AVX, AVX2,
FMA, AVX-512F/BW/VL; AArch64: NEON, SVE) and builds a table of function
pointers per op, before the RT thread starts. The RT thread never branches
on CPU features and never changes the table. Overrides: `PATCH_DSP=` for
all ops, and a persisted per-installation profile written by `omarchy-patch
optimize` (below). An unsupported selection falls back to the best
supported implementation and is reported, never executed.

## Verification and benchmarks

One harness checks every implementation of every op against the reference:
zero, 1, odd and large frame counts, unaligned starts, silence, full scale,
impulses, sines, noise, denormals, NaN and Inf, in place and out of place,
1 to 8 channels, plus randomized differential runs with a fixed seed. One
benchmark harness measures ns/frame at 16 … 4096 frames after warm-up,
with median and p95 over repeated runs, pinned to one CPU when allowed, and
reports the CPU model, governor and flags. Publishing a number requires a
release build and records the environment. Nothing is invented: if a
measurement isn't possible (no cycle counter, no affinity), the report says
so.

`omarchy-patch optimize` = detect features → list implementations → verify
each against the reference → benchmark → select the fastest verified one
per op → persist `~/.config/omarchy-patch/kernels.json`, with the CPU model
and the measurements. Profiles belong to the installation, never to
`.patchdj` data.

## Assembly acceptance gate

An optimised kernel is kept only if: a reference exists; the correctness
and randomized differential tests pass; its CPU feature is checked before
use; unsupported CPUs fall back; a release benchmark shows a meaningful
gain at realistic sizes (16 … 1024 frames); it is documented (registers,
clobbers, alignment, ISA, assumptions) in its source file; the build works
without it; it keeps the RT rules. Source assembly goes in `.s` files, one
directory per ISA level (`patch-dsp/src/x86_64/avx2/…`), never scattered
through unrelated crates. Inline assembly only for a few instructions.

## Generated kernels (future)

A kernel may one day come from a compiler, an agent-written IR or agent-
written machine code. The pipeline stays: semantic op → reference →
machine-checkable contract → candidate → verification → benchmark →
approved implementation. Rules for that day:

* Never run unverified output. Candidates are verified and fuzzed offline,
  optionally in a sandboxed subprocess, before they're accepted.
* Accepted code is identified by a content hash and stored with its
  verification record. Loading maps it W^X (write, then seal read+execute).
* Swaps happen outside the RT thread: build and verify, prepare the state,
  then switch the table at a graph-cycle boundary (the engine's handover
  or an atomic table swap read once per cycle). Keep the old one until the
  new one has run cleanly.
* The portable reference is always compiled in. A crashing or rejected
  kernel falls back to it, and the rejection is persisted.

A small DSP IR (gain, multiply-add, clamp, matrix mix, biquad, delay,
interpolation, accumulate) is worth prototyping when two conditions hold:
we have at least five ops behind the ABI, and a generator would lower the
IR to more than one ISA. Until then the ABI plus the reference is the
contract.
