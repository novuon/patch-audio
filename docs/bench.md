# Patch benchmark

CPU: Intel(R) Core(TM) Ultra 7 265K. Best tier: x86-64 AVX2+FMA assembly. Patch 0.4.0.

Not a real-time thread: worst cases include scheduler noise (upper bounds);
the ~7 µs floor of the AVX2 worst cases is that noise, not the kernels.

Reproduce: `cargo run --release -p patch-bench -- docs/bench.md`. Every tier
passes the same test suite: `PATCH_DSP=reference|sse2|avx2 cargo test`.

The report also includes the complete Level 1 analysis pipeline on a fixed
synthetic 30-second DJ track. It reports tracks/second, throughput relative to
audio duration, result heap estimate and serialized cache bytes. The fixture
isolates analysis CPU from file decoding and disk I/O; it is not a substitute
for measuring representative compressed library tracks on their target CPU.

Tiers: `sse2` has assembly for VU ballistics and K-weighting (double
precision filters); everything else at that tier falls back to the
reference, which is why it tracks the reference closely. `avx2` has
assembly for every hot kernel.

## Kernels behind the ABI (`omarchy-patch optimize`)

2026-09-27, Intel Core Ultra 7 265K (SSE2 SSE4.1 AVX AVX2 FMA), release
build, thread pinned to one CPU, governor `powersave`, 21 runs per size
after warm-up, median ns per frame (stereo). Every implementation passed
the correctness harness (bit-exact against the reference over 24 lengths,
1–8 channels, unaligned starts, silence, full scale, impulses, sines,
noise, denormals, NaN and ±Inf, with canaries) before it was timed.

| op | implementation | 64 frames | 256 | 1024 | vs reference | kept |
|---|---|---|---|---|---|---|
| mix_ramp | reference (Rust) | 5.766 | 5.518 | 5.459 | 1.00× | fallback |
| mix_ramp | avx2 (asm) | 0.187 | 0.170 | 0.168 | 31.3× | **selected** |
| peak | reference (Rust) | 0.476 | 0.701 | 0.848 | 1.00× | fallback |
| peak | avx2 (asm) | 0.098 | 0.078 | 0.073 | 6.8× | **selected** |
| interleave2 | reference (Rust) | 0.114 | 0.088 | 0.086 | 1.00× | **selected** |
| interleave2 | avx2 (asm) | 0.114 | 0.114 | 0.114 | 0.92× | **removed** |

The mix reference is slow on a baseline x86-64 build because
`f32::mul_add` (needed for bit-exact FMA semantics) becomes a call to
`fmaf`; the AVX2 kernel does the same fused operations in registers. The
interleave reference is already vectorised by the compiler; the
hand-written version wasn't faster, so it failed the acceptance gate and
was deleted. Reproduce: `omarchy-patch optimize` (add `--dry-run` to keep
the saved profile).

## Engine cycle (complete real-time callback)

Budget = frames / rate. Load = worst cycle / budget.

| tier | rate | frames | channels | budget µs | avg µs | p99 µs | worst µs | worst load |
|---|---|---|---|---|---|---|---|---|
| reference | 48 kHz | 32 | 3 | 666.67 | 0.80 | 0.81 | 2.59 | 0.388 % |
| reference | 48 kHz | 32 | 8 | 666.67 | 1.92 | 1.92 | 5.22 | 0.782 % |
| reference | 48 kHz | 32 | 16 | 666.67 | 3.71 | 4.07 | 15.81 | 2.372 % |
| reference | 48 kHz | 64 | 3 | 1333.33 | 1.61 | 1.68 | 9.78 | 0.734 % |
| reference | 48 kHz | 64 | 8 | 1333.33 | 4.02 | 4.29 | 12.72 | 0.954 % |
| reference | 48 kHz | 64 | 16 | 1333.33 | 7.96 | 11.56 | 16.72 | 1.254 % |
| reference | 48 kHz | 128 | 3 | 2666.67 | 3.29 | 3.60 | 45.72 | 1.714 % |
| reference | 48 kHz | 128 | 8 | 2666.67 | 7.89 | 11.04 | 38.93 | 1.460 % |
| reference | 48 kHz | 128 | 16 | 2666.67 | 15.07 | 21.75 | 36.46 | 1.367 % |
| reference | 48 kHz | 256 | 3 | 5333.33 | 6.31 | 7.34 | 14.26 | 0.267 % |
| reference | 48 kHz | 256 | 8 | 5333.33 | 16.08 | 23.04 | 121.95 | 2.287 % |
| reference | 48 kHz | 256 | 16 | 5333.33 | 30.27 | 38.10 | 44.93 | 0.842 % |
| reference | 48 kHz | 512 | 3 | 10666.67 | 12.47 | 19.27 | 20.36 | 0.191 % |
| reference | 48 kHz | 512 | 8 | 10666.67 | 30.50 | 38.10 | 45.79 | 0.429 % |
| reference | 48 kHz | 512 | 16 | 10666.67 | 60.34 | 71.43 | 86.16 | 0.808 % |
| reference | 48 kHz | 1024 | 3 | 21333.33 | 25.03 | 32.52 | 40.40 | 0.189 % |
| reference | 48 kHz | 1024 | 8 | 21333.33 | 60.94 | 70.20 | 78.92 | 0.370 % |
| reference | 48 kHz | 1024 | 16 | 21333.33 | 119.63 | 134.90 | 163.96 | 0.769 % |
| reference | 96 kHz | 32 | 3 | 333.33 | 0.84 | 0.90 | 7.94 | 2.383 % |
| reference | 96 kHz | 32 | 8 | 333.33 | 2.16 | 2.17 | 10.71 | 3.212 % |
| reference | 96 kHz | 32 | 16 | 333.33 | 4.09 | 4.14 | 13.17 | 3.952 % |
| reference | 96 kHz | 64 | 3 | 666.67 | 1.62 | 1.68 | 9.64 | 1.446 % |
| reference | 96 kHz | 64 | 8 | 666.67 | 3.94 | 4.14 | 12.01 | 1.801 % |
| reference | 96 kHz | 64 | 16 | 666.67 | 7.64 | 10.73 | 15.90 | 2.384 % |
| reference | 96 kHz | 128 | 3 | 1333.33 | 3.20 | 3.38 | 11.56 | 0.867 % |
| reference | 96 kHz | 128 | 8 | 1333.33 | 7.75 | 10.55 | 16.18 | 1.213 % |
| reference | 96 kHz | 128 | 16 | 1333.33 | 15.01 | 21.73 | 23.74 | 1.780 % |
| reference | 96 kHz | 256 | 3 | 2666.67 | 6.34 | 7.32 | 14.18 | 0.532 % |
| reference | 96 kHz | 256 | 8 | 2666.67 | 15.77 | 22.25 | 25.26 | 0.947 % |
| reference | 96 kHz | 256 | 16 | 2666.67 | 30.03 | 37.53 | 53.01 | 1.988 % |
| reference | 96 kHz | 512 | 3 | 5333.33 | 12.53 | 19.30 | 21.94 | 0.411 % |
| reference | 96 kHz | 512 | 8 | 5333.33 | 31.36 | 39.97 | 84.27 | 1.580 % |
| reference | 96 kHz | 512 | 16 | 5333.33 | 60.00 | 70.45 | 78.46 | 1.471 % |
| reference | 96 kHz | 1024 | 3 | 10666.67 | 25.36 | 32.94 | 123.01 | 1.153 % |
| reference | 96 kHz | 1024 | 8 | 10666.67 | 61.98 | 73.14 | 83.92 | 0.787 % |
| reference | 96 kHz | 1024 | 16 | 10666.67 | 120.13 | 134.48 | 165.83 | 1.555 % |
| reference | 192 kHz | 32 | 3 | 166.67 | 0.88 | 0.89 | 8.09 | 4.853 % |
| reference | 192 kHz | 32 | 8 | 166.67 | 2.11 | 2.13 | 10.33 | 6.199 % |
| reference | 192 kHz | 32 | 16 | 166.67 | 4.03 | 4.13 | 12.07 | 7.241 % |
| reference | 192 kHz | 64 | 3 | 333.33 | 1.61 | 1.69 | 9.58 | 2.873 % |
| reference | 192 kHz | 64 | 8 | 333.33 | 4.06 | 4.20 | 12.05 | 3.615 % |
| reference | 192 kHz | 64 | 16 | 333.33 | 7.71 | 10.61 | 17.42 | 5.226 % |
| reference | 192 kHz | 128 | 3 | 666.67 | 3.20 | 3.59 | 11.65 | 1.747 % |
| reference | 192 kHz | 128 | 8 | 666.67 | 7.70 | 9.16 | 15.59 | 2.338 % |
| reference | 192 kHz | 128 | 16 | 666.67 | 15.02 | 21.72 | 22.80 | 3.420 % |
| reference | 192 kHz | 256 | 3 | 1333.33 | 6.34 | 7.22 | 14.56 | 1.092 % |
| reference | 192 kHz | 256 | 8 | 1333.33 | 15.62 | 22.13 | 25.95 | 1.946 % |
| reference | 192 kHz | 256 | 16 | 1333.33 | 30.12 | 37.70 | 42.26 | 3.170 % |
| reference | 192 kHz | 512 | 3 | 2666.67 | 12.73 | 19.36 | 25.86 | 0.970 % |
| reference | 192 kHz | 512 | 8 | 2666.67 | 30.78 | 38.94 | 46.86 | 1.757 % |
| reference | 192 kHz | 512 | 16 | 2666.67 | 59.86 | 70.20 | 117.12 | 4.392 % |
| reference | 192 kHz | 1024 | 3 | 5333.33 | 25.04 | 32.05 | 40.19 | 0.753 % |
| reference | 192 kHz | 1024 | 8 | 5333.33 | 61.39 | 72.02 | 92.61 | 1.737 % |
| reference | 192 kHz | 1024 | 16 | 5333.33 | 118.98 | 133.57 | 147.19 | 2.760 % |
| sse2 | 48 kHz | 32 | 3 | 666.67 | 0.84 | 0.88 | 8.77 | 1.315 % |
| sse2 | 48 kHz | 32 | 8 | 666.67 | 2.03 | 2.11 | 41.07 | 6.161 % |
| sse2 | 48 kHz | 32 | 16 | 666.67 | 3.94 | 4.16 | 11.85 | 1.777 % |
| sse2 | 48 kHz | 64 | 3 | 1333.33 | 1.61 | 1.67 | 9.48 | 0.711 % |
| sse2 | 48 kHz | 64 | 8 | 1333.33 | 3.98 | 4.25 | 11.99 | 0.899 % |
| sse2 | 48 kHz | 64 | 16 | 1333.33 | 8.08 | 11.84 | 16.52 | 1.239 % |
| sse2 | 48 kHz | 128 | 3 | 2666.67 | 3.21 | 3.38 | 11.35 | 0.426 % |
| sse2 | 48 kHz | 128 | 8 | 2666.67 | 7.79 | 10.29 | 20.58 | 0.772 % |
| sse2 | 48 kHz | 128 | 16 | 2666.67 | 15.40 | 21.90 | 24.30 | 0.911 % |
| sse2 | 48 kHz | 256 | 3 | 5333.33 | 6.47 | 8.30 | 14.96 | 0.280 % |
| sse2 | 48 kHz | 256 | 8 | 5333.33 | 15.66 | 22.11 | 37.16 | 0.697 % |
| sse2 | 48 kHz | 256 | 16 | 5333.33 | 30.04 | 37.39 | 44.06 | 0.826 % |
| sse2 | 48 kHz | 512 | 3 | 10666.67 | 12.55 | 19.30 | 23.19 | 0.217 % |
| sse2 | 48 kHz | 512 | 8 | 10666.67 | 31.01 | 38.76 | 46.16 | 0.433 % |
| sse2 | 48 kHz | 512 | 16 | 10666.67 | 60.23 | 70.38 | 89.26 | 0.837 % |
| sse2 | 48 kHz | 1024 | 3 | 21333.33 | 25.71 | 33.22 | 44.23 | 0.207 % |
| sse2 | 48 kHz | 1024 | 8 | 21333.33 | 61.95 | 73.15 | 201.56 | 0.945 % |
| sse2 | 48 kHz | 1024 | 16 | 21333.33 | 118.80 | 133.97 | 163.48 | 0.766 % |
| sse2 | 96 kHz | 32 | 3 | 333.33 | 0.83 | 0.87 | 8.94 | 2.682 % |
| sse2 | 96 kHz | 32 | 8 | 333.33 | 2.03 | 2.12 | 9.46 | 2.838 % |
| sse2 | 96 kHz | 32 | 16 | 333.33 | 3.90 | 4.08 | 11.82 | 3.545 % |
| sse2 | 96 kHz | 64 | 3 | 666.67 | 1.60 | 1.70 | 9.59 | 1.438 % |
| sse2 | 96 kHz | 64 | 8 | 666.67 | 3.95 | 4.18 | 11.83 | 1.774 % |
| sse2 | 96 kHz | 64 | 16 | 666.67 | 7.67 | 9.71 | 15.58 | 2.336 % |
| sse2 | 96 kHz | 128 | 3 | 1333.33 | 3.16 | 3.38 | 11.05 | 0.829 % |
| sse2 | 96 kHz | 128 | 8 | 1333.33 | 7.72 | 10.22 | 15.63 | 1.172 % |
| sse2 | 96 kHz | 128 | 16 | 1333.33 | 14.96 | 21.72 | 25.69 | 1.927 % |
| sse2 | 96 kHz | 256 | 3 | 2666.67 | 6.48 | 8.21 | 14.91 | 0.559 % |
| sse2 | 96 kHz | 256 | 8 | 2666.67 | 15.53 | 22.11 | 53.57 | 2.009 % |
| sse2 | 96 kHz | 256 | 16 | 2666.67 | 30.81 | 38.74 | 45.16 | 1.693 % |
| sse2 | 96 kHz | 512 | 3 | 5333.33 | 12.63 | 19.32 | 21.45 | 0.402 % |
| sse2 | 96 kHz | 512 | 8 | 5333.33 | 30.89 | 38.68 | 50.30 | 0.943 % |
| sse2 | 96 kHz | 512 | 16 | 5333.33 | 60.14 | 70.82 | 76.45 | 1.433 % |
| sse2 | 96 kHz | 1024 | 3 | 10666.67 | 25.13 | 32.30 | 45.28 | 0.425 % |
| sse2 | 96 kHz | 1024 | 8 | 10666.67 | 61.24 | 70.95 | 81.84 | 0.767 % |
| sse2 | 96 kHz | 1024 | 16 | 10666.67 | 119.49 | 134.39 | 233.32 | 2.187 % |
| sse2 | 192 kHz | 32 | 3 | 166.67 | 0.84 | 0.88 | 7.86 | 4.715 % |
| sse2 | 192 kHz | 32 | 8 | 166.67 | 2.03 | 2.12 | 10.03 | 6.017 % |
| sse2 | 192 kHz | 32 | 16 | 166.67 | 3.94 | 4.19 | 12.46 | 7.474 % |
| sse2 | 192 kHz | 64 | 3 | 333.33 | 1.62 | 1.82 | 8.79 | 2.635 % |
| sse2 | 192 kHz | 64 | 8 | 333.33 | 3.95 | 4.18 | 12.01 | 3.602 % |
| sse2 | 192 kHz | 64 | 16 | 333.33 | 7.73 | 9.46 | 16.09 | 4.826 % |
| sse2 | 192 kHz | 128 | 3 | 666.67 | 3.15 | 3.38 | 11.07 | 1.661 % |
| sse2 | 192 kHz | 128 | 8 | 666.67 | 8.00 | 11.73 | 16.50 | 2.475 % |
| sse2 | 192 kHz | 128 | 16 | 666.67 | 15.22 | 21.80 | 23.85 | 3.577 % |
| sse2 | 192 kHz | 256 | 3 | 1333.33 | 6.34 | 7.64 | 14.38 | 1.078 % |
| sse2 | 192 kHz | 256 | 8 | 1333.33 | 15.51 | 22.16 | 24.23 | 1.817 % |
| sse2 | 192 kHz | 256 | 16 | 1333.33 | 30.79 | 38.36 | 50.17 | 3.763 % |
| sse2 | 192 kHz | 512 | 3 | 2666.67 | 12.60 | 19.26 | 150.11 | 5.629 % |
| sse2 | 192 kHz | 512 | 8 | 2666.67 | 31.28 | 39.39 | 76.30 | 2.861 % |
| sse2 | 192 kHz | 512 | 16 | 2666.67 | 59.77 | 69.62 | 100.97 | 3.786 % |
| sse2 | 192 kHz | 1024 | 3 | 5333.33 | 25.20 | 31.99 | 39.29 | 0.737 % |
| sse2 | 192 kHz | 1024 | 8 | 5333.33 | 62.34 | 73.15 | 93.96 | 1.762 % |
| sse2 | 192 kHz | 1024 | 16 | 5333.33 | 120.24 | 134.49 | 169.74 | 3.183 % |
| avx2 | 48 kHz | 32 | 3 | 666.67 | 0.20 | 0.21 | 7.26 | 1.089 % |
| avx2 | 48 kHz | 32 | 8 | 666.67 | 0.32 | 0.34 | 7.19 | 1.078 % |
| avx2 | 48 kHz | 32 | 16 | 666.67 | 0.64 | 0.67 | 8.29 | 1.244 % |
| avx2 | 48 kHz | 64 | 3 | 1333.33 | 0.23 | 0.24 | 7.31 | 0.549 % |
| avx2 | 48 kHz | 64 | 8 | 1333.33 | 0.39 | 0.41 | 7.42 | 0.556 % |
| avx2 | 48 kHz | 64 | 16 | 1333.33 | 0.64 | 0.64 | 7.75 | 0.581 % |
| avx2 | 48 kHz | 128 | 3 | 2666.67 | 0.33 | 0.38 | 7.43 | 0.279 % |
| avx2 | 48 kHz | 128 | 8 | 2666.67 | 0.52 | 0.56 | 8.53 | 0.320 % |
| avx2 | 48 kHz | 128 | 16 | 2666.67 | 0.82 | 0.89 | 8.47 | 0.318 % |
| avx2 | 48 kHz | 256 | 3 | 5333.33 | 0.58 | 0.63 | 7.63 | 0.143 % |
| avx2 | 48 kHz | 256 | 8 | 5333.33 | 0.88 | 0.95 | 8.89 | 0.167 % |
| avx2 | 48 kHz | 256 | 16 | 5333.33 | 1.41 | 1.52 | 8.51 | 0.160 % |
| avx2 | 48 kHz | 512 | 3 | 10666.67 | 1.07 | 1.17 | 9.13 | 0.086 % |
| avx2 | 48 kHz | 512 | 8 | 10666.67 | 1.66 | 1.81 | 14.41 | 0.135 % |
| avx2 | 48 kHz | 512 | 16 | 10666.67 | 2.57 | 2.87 | 10.49 | 0.098 % |
| avx2 | 48 kHz | 1024 | 3 | 21333.33 | 2.05 | 2.11 | 9.87 | 0.046 % |
| avx2 | 48 kHz | 1024 | 8 | 21333.33 | 3.24 | 3.47 | 11.14 | 0.052 % |
| avx2 | 48 kHz | 1024 | 16 | 21333.33 | 5.13 | 5.96 | 13.06 | 0.061 % |
| avx2 | 96 kHz | 32 | 3 | 333.33 | 0.18 | 0.19 | 0.24 | 0.071 % |
| avx2 | 96 kHz | 32 | 8 | 333.33 | 0.33 | 0.34 | 7.23 | 2.170 % |
| avx2 | 96 kHz | 32 | 16 | 333.33 | 0.56 | 0.58 | 32.89 | 9.868 % |
| avx2 | 96 kHz | 64 | 3 | 666.67 | 0.23 | 0.24 | 7.29 | 1.094 % |
| avx2 | 96 kHz | 64 | 8 | 666.67 | 0.38 | 0.41 | 7.36 | 1.104 % |
| avx2 | 96 kHz | 64 | 16 | 666.67 | 0.64 | 0.67 | 8.06 | 1.209 % |
| avx2 | 96 kHz | 128 | 3 | 1333.33 | 0.33 | 0.37 | 7.38 | 0.553 % |
| avx2 | 96 kHz | 128 | 8 | 1333.33 | 0.52 | 0.57 | 7.48 | 0.561 % |
| avx2 | 96 kHz | 128 | 16 | 1333.33 | 0.82 | 0.89 | 8.64 | 0.648 % |
| avx2 | 96 kHz | 256 | 3 | 2666.67 | 0.56 | 0.62 | 7.66 | 0.287 % |
| avx2 | 96 kHz | 256 | 8 | 2666.67 | 0.88 | 0.94 | 8.48 | 0.318 % |
| avx2 | 96 kHz | 256 | 16 | 2666.67 | 1.41 | 1.52 | 9.22 | 0.346 % |
| avx2 | 96 kHz | 512 | 3 | 5333.33 | 1.05 | 1.11 | 8.05 | 0.151 % |
| avx2 | 96 kHz | 512 | 8 | 5333.33 | 1.71 | 1.87 | 9.98 | 0.187 % |
| avx2 | 96 kHz | 512 | 16 | 5333.33 | 2.69 | 2.92 | 10.65 | 0.200 % |
| avx2 | 96 kHz | 1024 | 3 | 10666.67 | 2.07 | 2.21 | 10.13 | 0.095 % |
| avx2 | 96 kHz | 1024 | 8 | 10666.67 | 3.22 | 3.39 | 11.17 | 0.105 % |
| avx2 | 96 kHz | 1024 | 16 | 10666.67 | 5.11 | 5.86 | 13.11 | 0.123 % |
| avx2 | 192 kHz | 32 | 3 | 166.67 | 0.19 | 0.20 | 7.45 | 4.470 % |
| avx2 | 192 kHz | 32 | 8 | 166.67 | 0.33 | 0.34 | 8.28 | 4.970 % |
| avx2 | 192 kHz | 32 | 16 | 166.67 | 0.55 | 0.58 | 7.52 | 4.512 % |
| avx2 | 192 kHz | 64 | 3 | 333.33 | 0.23 | 0.23 | 7.18 | 2.155 % |
| avx2 | 192 kHz | 64 | 8 | 333.33 | 0.39 | 0.39 | 7.40 | 2.219 % |
| avx2 | 192 kHz | 64 | 16 | 333.33 | 0.64 | 0.64 | 7.67 | 2.300 % |
| avx2 | 192 kHz | 128 | 3 | 666.67 | 0.33 | 0.38 | 7.31 | 1.097 % |
| avx2 | 192 kHz | 128 | 8 | 666.67 | 0.51 | 0.56 | 7.48 | 1.122 % |
| avx2 | 192 kHz | 128 | 16 | 666.67 | 0.81 | 0.89 | 7.89 | 1.183 % |
| avx2 | 192 kHz | 256 | 3 | 1333.33 | 0.56 | 0.62 | 7.54 | 0.565 % |
| avx2 | 192 kHz | 256 | 8 | 1333.33 | 0.88 | 0.94 | 8.81 | 0.660 % |
| avx2 | 192 kHz | 256 | 16 | 1333.33 | 1.40 | 1.51 | 9.57 | 0.718 % |
| avx2 | 192 kHz | 512 | 3 | 2666.67 | 1.09 | 1.18 | 9.19 | 0.345 % |
| avx2 | 192 kHz | 512 | 8 | 2666.67 | 1.72 | 1.91 | 9.29 | 0.348 % |
| avx2 | 192 kHz | 512 | 16 | 2666.67 | 2.67 | 2.90 | 10.61 | 0.398 % |
| avx2 | 192 kHz | 1024 | 3 | 5333.33 | 2.06 | 2.20 | 9.09 | 0.171 % |
| avx2 | 192 kHz | 1024 | 8 | 5333.33 | 3.22 | 3.36 | 11.28 | 0.211 % |
| avx2 | 192 kHz | 1024 | 16 | 5333.33 | 5.12 | 5.93 | 13.19 | 0.247 % |

## Channel scaling (mix with gain ramp + sample peak, per cycle of 256 frames)

| tier | channels | avg µs | p99 µs | worst µs | ns per channel |
|---|---|---|---|---|---|
| reference | 2 | 1.80 | 1.80 | 9.78 | 901 |
| reference | 8 | 7.11 | 8.09 | 16.64 | 889 |
| reference | 32 | 28.84 | 36.62 | 39.23 | 901 |
| reference | 64 | 57.06 | 66.39 | 82.81 | 892 |
| reference | 128 | 115.26 | 129.22 | 208.74 | 900 |
| sse2 | 2 | 1.80 | 1.79 | 9.51 | 898 |
| sse2 | 8 | 7.42 | 10.87 | 16.33 | 927 |
| sse2 | 32 | 28.55 | 36.12 | 45.68 | 892 |
| sse2 | 64 | 57.81 | 67.99 | 79.45 | 903 |
| sse2 | 128 | 115.19 | 128.94 | 160.00 | 900 |
| avx2 | 2 | 0.08 | 0.09 | 0.14 | 40 |
| avx2 | 8 | 0.25 | 0.26 | 7.37 | 31 |
| avx2 | 32 | 0.96 | 0.99 | 8.54 | 30 |
| avx2 | 64 | 2.24 | 2.42 | 10.48 | 35 |
| avx2 | 128 | 3.87 | 4.07 | 11.82 | 30 |

## Beat FX (one deck channel, stereo, 512-frame blocks at 48 kHz)

2026-09-28, same machine, release build, governor `powersave`, not
pinned. Reproduce: `cargo test --release -p patch-engine fx_cost -- --ignored --nocapture`.
Share is of one 512-frame cycle (10.7 ms).

| effect | ns/frame | share of cycle |
|---|---|---|
| echo | 9.67 | 0.05 % |
| reverb | 16.61 | 0.08 % |
| flanger | 14.75 | 0.07 % |
| trans | 3.16 | 0.02 % |
| crush | 1.43 | 0.01 % |
| roll | 1.86 | 0.01 % |
| brake | 1.67 | 0.01 % |
| pingpong | 2.73 | 0.01 % |
| phaser | 11.30 | 0.05 % |
| noise | 6.23 | 0.03 % |
| sweep | 15.15 | 0.07 % |
| tremolo | 2.72 | 0.01 % |
| pan | 1.73 | 0.01 % |
| drive | 3.28 | 0.02 % |
| echo+sweep+reverb (chain) | 37.53 | 0.18 % |

Switching effects: the worst 64-frame block over 40 changes is 2.5 µs
(it was 1.7 ms, a memset of the delay lines inside the callback, more
than a whole 64-frame cycle of 1.33 ms). A channel whose effects are off
and whose tails are silent costs nothing.

## Analysis kernels (one second of 48 kHz stereo, in 1024-frame blocks)

× real time = how many seconds of audio one second of CPU processes.

| tier | kernel | avg ms per s of audio | worst ms | × real time |
|---|---|---|---|---|
| reference | VU ballistics (IEC 60268-17) | 0.114 | 0.123 | 8768× |
| reference | K-weighting + 4× true peak (BS.1770-4) | 6.221 | 6.379 | 161× |
| reference | 24-bit TPDF conversion | 0.367 | 0.401 | 2722× |
| reference | peak + energy (stats) | 0.109 | 0.129 | 9135× |
| sse2 | VU ballistics (IEC 60268-17) | 0.079 | 0.090 | 12605× |
| sse2 | K-weighting + 4× true peak (BS.1770-4) | 6.262 | 6.536 | 160× |
| sse2 | 24-bit TPDF conversion | 0.369 | 0.409 | 2713× |
| sse2 | peak + energy (stats) | 0.113 | 0.129 | 8862× |
| avx2 | VU ballistics (IEC 60268-17) | 0.083 | 0.091 | 12104× |
| avx2 | K-weighting + 4× true peak (BS.1770-4) | 0.208 | 0.218 | 4806× |
| avx2 | 24-bit TPDF conversion | 0.040 | 0.048 | 24866× |
| avx2 | peak + energy (stats) | 0.007 | 0.007 | 149744× |

## Auto Mix field (one frame, `transition_active` fixture, release build)

`cargo test --release -p patch-surfaces aivis_cost -- --ignored --nocapture`.
60 consecutive frames on the software renderer (UI thread), before upload
to Slint. At 60 fps the frame budget is 16.7 ms; the panel steps quality
down after three frames over 6 ms.

| view | size | quality | median ms | p95 ms | worst ms |
|---|---|---|---|---|---|
| compact | 320×200 | full | 0.08 | 0.09 | 0.19 |
| operator | 1280×720 | full | 0.51 | 0.68 | 1.63 |
| operator | 1280×720 | reduced | 0.41 | 0.43 | 0.43 |
| operator | 1280×720 | minimal | 0.36 | 0.38 | 0.39 |
| performance | 1920×1080 | full | 0.94 | 0.97 | 2.87 |
| performance | 1920×1080 | reduced | 0.85 | 0.87 | 0.87 |
| performance | 1920×1080 | minimal | 0.78 | 0.80 | 0.81 |

At 1080p most of the cost is clearing and writing the 8 MB frame, not
drawing; the internal resolution cap (1600 / 1100 / 700 px) bounds it on
larger screens.

## Master limiter

`cargo test --release -p patch-engine limiter_cost -- --ignored --nocapture`:
5.5 µs per 1024-frame stereo cycle with the signal constantly over the
ceiling (the worst case), 0.03 % of the 21.3 ms cycle at 48 kHz.

## Master EQ

`cargo test --release -p patch-engine master_eq_cost -- --ignored --nocapture`:
18 µs per 1024-frame stereo cycle with all eight bands active (0.08 % of
the cycle); flat or bypassed it processes nothing. The EQ window
(`cargo test --release -p patch-surfaces eq_render_cost -- --ignored --nocapture`):
1.1 ms per response redraw at 1200×480 (only when a band changes),
0.09 ms per spectrum frame at 600×240 (15 fps while shown).

## Tempo against BPM tags

`patch-bench --tempo-tags <list>` reads "path<TAB>tagged BPM" lines, detects
each track's tempo in parallel and classifies it as exact (±0.5 %), octave,
3:2 or other, listing every miss. Tags are weak labels (some are wrong or
use a half-time convention), so read it as a regression check, not ground
truth. On a 956-track DJ library (house, techno, disco, some drum & bass and
hip-hop): 90.3 % exact before kick-grid checks at fitted tempos in 20 s
windows, 93.5 % after (3:2 misreadings 28 → 3, octave 21 → 18), 78 s on 20
threads.
