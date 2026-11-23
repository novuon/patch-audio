# Patch VU metering unit — x86-64 kernels (System V ABI, Intel syntax,
# which is the default for Rust's global_asm!).
#
# All kernels read interleaved stereo float32 (L R L R …).
# They never allocate, never branch on sample values, and never touch
# memory beyond `frames * 8` bytes of input and their small output struct.

    .text

# ---------------------------------------------------------------------------
# void patch_meter_stats_avx2(const float *samples, size_t frames, Stats *out)
#
#   out->peak[2]   (f32)  max |x| per channel
#   out->sum_sq[2] (f64)  Σ x² per channel, accumulated in double precision
#
# Requires AVX2 + FMA. 8 frames (16 floats) per iteration, two max and four
# FMA accumulators for instruction-level parallelism. Lanes stay in L R L R
# order throughout, so channels never mix.
#   rdi = samples, rsi = frames, rdx = out
# ---------------------------------------------------------------------------
    .globl  patch_meter_stats_avx2
    .p2align 5
patch_meter_stats_avx2:
    vpcmpeqd    ymm15, ymm15, ymm15
    vpsrld      ymm15, ymm15, 1             # 0x7fffffff: float abs mask
    vxorps      ymm0, ymm0, ymm0            # max accumulators
    vxorps      ymm1, ymm1, ymm1
    vxorpd      ymm2, ymm2, ymm2            # Σx² accumulators (f64)
    vxorpd      ymm3, ymm3, ymm3
    vxorpd      ymm4, ymm4, ymm4
    vxorpd      ymm5, ymm5, ymm5
    mov         rcx, rsi
    shr         rcx, 3                      # blocks of 8 frames
    jz          .Lstats_reduce

    .p2align 4
.Lstats_loop:
    vmovups     ymm6, [rdi]                 # frames 0..3
    vmovups     ymm7, [rdi + 32]            # frames 4..7
    vandps      ymm8, ymm6, ymm15
    vandps      ymm9, ymm7, ymm15
    vmaxps      ymm0, ymm0, ymm8
    vmaxps      ymm1, ymm1, ymm9
    vcvtps2pd   ymm10, xmm6                 # L0 R0 L1 R1 → f64
    vextractf128 xmm11, ymm6, 1
    vcvtps2pd   ymm11, xmm11                # L2 R2 L3 R3
    vcvtps2pd   ymm12, xmm7
    vextractf128 xmm13, ymm7, 1
    vcvtps2pd   ymm13, xmm13
    vfmadd231pd ymm2, ymm10, ymm10
    vfmadd231pd ymm3, ymm11, ymm11
    vfmadd231pd ymm4, ymm12, ymm12
    vfmadd231pd ymm5, ymm13, ymm13
    add         rdi, 64
    dec         rcx
    jnz         .Lstats_loop

.Lstats_reduce:
    vmaxps      ymm0, ymm0, ymm1
    vextractf128 xmm1, ymm0, 1
    vmaxps      xmm0, xmm0, xmm1            # [L R L R]
    vmovhlps    xmm1, xmm0, xmm0
    vmaxps      xmm0, xmm0, xmm1            # [L R . .]
    vaddpd      ymm2, ymm2, ymm3
    vaddpd      ymm4, ymm4, ymm5
    vaddpd      ymm2, ymm2, ymm4            # [L R L R]
    vextractf128 xmm3, ymm2, 1
    vaddpd      xmm2, xmm2, xmm3            # [L R]

    and         rsi, 7                      # remaining frames
    jz          .Lstats_store
.Lstats_tail:
    vmovsd      xmm6, [rdi]                 # one frame: L R 0 0
    vandps      xmm7, xmm6, xmm15
    vmaxps      xmm0, xmm0, xmm7
    vcvtps2pd   xmm8, xmm6
    vfmadd231pd xmm2, xmm8, xmm8
    add         rdi, 8
    dec         rsi
    jnz         .Lstats_tail

.Lstats_store:
    vmovsd      [rdx], xmm0                 # peak[2]  (2 × f32)
    vmovupd     [rdx + 8], xmm2             # sum_sq[2] (2 × f64)
    vzeroupper
    ret

# ---------------------------------------------------------------------------
# void patch_meter_ballistics_sse2(const float *samples, size_t frames,
#                               double state[4], double coeff, double max[2])
#
# VU ballistics per IEC 60268-17 approximated as a critically damped
# second-order response on the rectified signal: two cascaded one-pole
# low-pass filters sharing coefficient `coeff`:
#
#     y1 += coeff · (|x| − y1)
#     y2 += coeff · (y1 − y2)
#
# state = { y1L, y1R, y2L, y2R }, updated in place. max[] receives the
# largest y2 seen in the block (the needle's highest point).
# Pure SSE2 (x86-64 baseline), double precision, L and R as the two lanes.
# The operation order matches the Rust reference exactly, so results are
# bit-identical.
#   rdi = samples, rsi = frames, rdx = state, xmm0 = coeff, rcx = max
# ---------------------------------------------------------------------------
    .globl  patch_meter_ballistics_sse2
    .p2align 5
patch_meter_ballistics_sse2:
    unpcklpd    xmm0, xmm0                  # coeff in both lanes
    movupd      xmm1, [rdx]                 # y1
    movupd      xmm2, [rdx + 16]            # y2
    pcmpeqd     xmm7, xmm7
    psrlq       xmm7, 1                     # 0x7fff…: double abs mask
    movapd      xmm6, xmm2                  # running max of y2
    test        rsi, rsi
    jz          .Lvu_store

    .p2align 4
.Lvu_loop:
    cvtps2pd    xmm3, qword ptr [rdi]       # L R → f64
    andpd       xmm3, xmm7                  # |x|
    subpd       xmm3, xmm1                  # |x| − y1
    mulpd       xmm3, xmm0
    addpd       xmm1, xmm3                  # y1 += c(|x| − y1)
    movapd      xmm4, xmm1
    subpd       xmm4, xmm2                  # y1 − y2
    mulpd       xmm4, xmm0
    addpd       xmm2, xmm4                  # y2 += c(y1 − y2)
    maxpd       xmm6, xmm2
    add         rdi, 8
    dec         rsi
    jnz         .Lvu_loop

.Lvu_store:
    movupd      [rdx], xmm1
    movupd      [rdx + 16], xmm2
    movupd      [rcx], xmm6
    ret

# ---------------------------------------------------------------------------
# void patch_kweight_sse2(const float *samples, size_t frames,
#                         double state[8], const double coeffs[10],
#                         double sum_sq[2])
#
# ITU-R BS.1770-4 K-weighting: two biquads in transposed direct form II,
# double precision, L and R as the two SSE2 lanes. Accumulates the mean-square
# numerator Σ y² per channel into sum_sq (added to, not overwritten).
#
#   coeffs = { b0 b1 b2 a1 a2 } stage 1 (high shelf), then stage 2 (high-pass)
#   state  = { z1a[L,R], z2a[L,R], z1b[L,R], z2b[L,R] }
#
# Per stage:  y = b0·x + z1;  z1 = b1·x − a1·y + z2;  z2 = b2·x − a2·y
# The Rust reference performs the same operations in the same order.
#   rdi = samples, rsi = frames, rdx = state, rcx = coeffs, r8 = sum_sq
# ---------------------------------------------------------------------------
    .globl  patch_kweight_sse2
    .p2align 5
patch_kweight_sse2:
    sub         rsp, 168                    # spill space for broadcast coeffs
    # broadcast the 10 coefficients into 16-byte slots on the stack
    xor         eax, eax
.Lkw_bcast:
    movsd       xmm0, qword ptr [rcx + rax*8]
    unpcklpd    xmm0, xmm0
    mov         r9, rax
    shl         r9, 4
    movupd      [rsp + r9], xmm0
    inc         rax
    cmp         rax, 10
    jne         .Lkw_bcast

    movupd      xmm8,  [rdx]                # z1a
    movupd      xmm9,  [rdx + 16]           # z2a
    movupd      xmm10, [rdx + 32]           # z1b
    movupd      xmm11, [rdx + 48]           # z2b
    movupd      xmm12, [r8]                 # Σy²
    test        rsi, rsi
    jz          .Lkw_store

    .p2align 4
.Lkw_loop:
    cvtps2pd    xmm0, qword ptr [rdi]       # x
    # --- stage 1 ---
    movupd      xmm1, [rsp + 0]             # b0
    mulpd       xmm1, xmm0
    addpd       xmm1, xmm8                  # y1 = b0·x + z1a
    movupd      xmm2, [rsp + 16]            # b1
    mulpd       xmm2, xmm0
    movupd      xmm3, [rsp + 48]            # a1
    mulpd       xmm3, xmm1
    subpd       xmm2, xmm3
    addpd       xmm2, xmm9                  # z1a = b1·x − a1·y1 + z2a
    movapd      xmm8, xmm2
    movupd      xmm2, [rsp + 32]            # b2
    mulpd       xmm2, xmm0
    movupd      xmm3, [rsp + 64]            # a2
    mulpd       xmm3, xmm1
    subpd       xmm2, xmm3                  # z2a = b2·x − a2·y1
    movapd      xmm9, xmm2
    # --- stage 2 (input y1) ---
    movupd      xmm4, [rsp + 80]            # b0
    mulpd       xmm4, xmm1
    addpd       xmm4, xmm10                 # y2 = b0·y1 + z1b
    movupd      xmm2, [rsp + 96]            # b1
    mulpd       xmm2, xmm1
    movupd      xmm3, [rsp + 128]           # a1
    mulpd       xmm3, xmm4
    subpd       xmm2, xmm3
    addpd       xmm2, xmm11                 # z1b = b1·y1 − a1·y2 + z2b
    movapd      xmm10, xmm2
    movupd      xmm2, [rsp + 112]           # b2
    mulpd       xmm2, xmm1
    movupd      xmm3, [rsp + 144]           # a2
    mulpd       xmm3, xmm4
    subpd       xmm2, xmm3                  # z2b = b2·y1 − a2·y2
    movapd      xmm11, xmm2
    # --- mean-square numerator ---
    mulpd       xmm4, xmm4
    addpd       xmm12, xmm4
    add         rdi, 8
    dec         rsi
    jnz         .Lkw_loop

.Lkw_store:
    movupd      [rdx], xmm8
    movupd      [rdx + 16], xmm9
    movupd      [rdx + 32], xmm10
    movupd      [rdx + 48], xmm11
    movupd      [r8], xmm12
    add         rsp, 168
    ret

# ---------------------------------------------------------------------------
# float patch_truepeak_avx2(const float *x, size_t groups, const float *coeffs)
#
# 4× oversampled peak of a mono signal, ITU-R BS.1770-4 Annex 2 style:
# a 48-tap polyphase interpolator, 4 phases × 12 taps. For every input
# position t the four interpolated samples are
#
#     y[4t + p] = Σ_{k=0..11} coeffs[p·12 + k] · x[t + k]
#
# `x` must hold `groups·8 + 11` samples (11 samples of history/look-ahead
# supplied by the caller). Eight consecutive t are computed at once with
# broadcast coefficients and unaligned loads, 48 FMAs per 8 input samples.
# Returns max |y| over the block.
#   rdi = x, rsi = groups (of 8 input samples), rdx = coeffs (48 floats)
# ---------------------------------------------------------------------------
    .globl  patch_truepeak_avx2
    .p2align 5
patch_truepeak_avx2:
    vpcmpeqd    ymm15, ymm15, ymm15
    vpsrld      ymm15, ymm15, 1             # abs mask
    vxorps      ymm14, ymm14, ymm14         # running max
    test        rsi, rsi
    jz          .Ltp_done

    .p2align 4
.Ltp_group:
    vxorps      ymm0, ymm0, ymm0            # phase 0..3 accumulators
    vxorps      ymm1, ymm1, ymm1
    vxorps      ymm2, ymm2, ymm2
    vxorps      ymm3, ymm3, ymm3
    xor         eax, eax                    # k
.Ltp_tap:
    vmovups     ymm4, [rdi + rax*4]         # x[t+k .. t+k+7]
    vbroadcastss ymm5, dword ptr [rdx + rax*4]
    vfmadd231ps ymm0, ymm4, ymm5
    vbroadcastss ymm5, dword ptr [rdx + rax*4 + 48]
    vfmadd231ps ymm1, ymm4, ymm5
    vbroadcastss ymm5, dword ptr [rdx + rax*4 + 96]
    vfmadd231ps ymm2, ymm4, ymm5
    vbroadcastss ymm5, dword ptr [rdx + rax*4 + 144]
    vfmadd231ps ymm3, ymm4, ymm5
    inc         rax
    cmp         rax, 12
    jne         .Ltp_tap
    vandps      ymm0, ymm0, ymm15
    vandps      ymm1, ymm1, ymm15
    vandps      ymm2, ymm2, ymm15
    vandps      ymm3, ymm3, ymm15
    vmaxps      ymm0, ymm0, ymm1
    vmaxps      ymm2, ymm2, ymm3
    vmaxps      ymm0, ymm0, ymm2
    vmaxps      ymm14, ymm14, ymm0
    add         rdi, 32
    dec         rsi
    jnz         .Ltp_group

.Ltp_done:
    vextractf128 xmm0, ymm14, 1
    vmaxps      xmm0, xmm0, xmm14
    vmovhlps    xmm1, xmm0, xmm0
    vmaxps      xmm0, xmm0, xmm1
    vshufps     xmm1, xmm0, xmm0, 0x55
    vmaxps      xmm0, xmm0, xmm1            # scalar max in xmm0[0]
    vzeroupper
    ret

# ---------------------------------------------------------------------------
# void patch_meter_stereo_avx2(const float *samples, size_t blocks,
#                              double out[5])
#
#   out = { ΣL, ΣR, ΣL², ΣR², ΣL·R } over `blocks` × 4 frames, in double
#   precision: the sums behind phase correlation, balance, M/S width, RMS
#   and DC offset. Requires AVX2 + FMA.
#
#   Per 4 frames: two f64 vectors [L0 R0 L1 R1], [L2 R2 L3 R3]; Σx and Σx²
#   accumulate lane-wise (channels never mix); the pair-swapped copy
#   [R0 L0 R1 L1] gives L·R in every lane (both lanes of a pair hold the
#   same product, so one lane of the reduction is the answer).
#   rdi = samples, rsi = blocks, rdx = out
# ---------------------------------------------------------------------------
    .globl  patch_meter_stereo_avx2
    .p2align 5
patch_meter_stereo_avx2:
    vxorpd      ymm0, ymm0, ymm0            # Σx   (two accumulators)
    vxorpd      ymm1, ymm1, ymm1
    vxorpd      ymm2, ymm2, ymm2            # Σx²
    vxorpd      ymm3, ymm3, ymm3
    vxorpd      ymm4, ymm4, ymm4            # ΣL·R
    vxorpd      ymm5, ymm5, ymm5
    test        rsi, rsi
    jz          .Lstereo_reduce

    .p2align 4
.Lstereo_loop:
    vcvtps2pd   ymm6, xmmword ptr [rdi]      # L0 R0 L1 R1
    vcvtps2pd   ymm7, xmmword ptr [rdi + 16] # L2 R2 L3 R3
    vaddpd      ymm0, ymm0, ymm6
    vaddpd      ymm1, ymm1, ymm7
    vfmadd231pd ymm2, ymm6, ymm6
    vfmadd231pd ymm3, ymm7, ymm7
    vpermilpd   ymm8, ymm6, 0x5              # R0 L0 R1 L1
    vpermilpd   ymm9, ymm7, 0x5
    vfmadd231pd ymm4, ymm6, ymm8
    vfmadd231pd ymm5, ymm7, ymm9
    add         rdi, 32
    dec         rsi
    jnz         .Lstereo_loop

.Lstereo_reduce:
    vaddpd      ymm0, ymm0, ymm1
    vextractf128 xmm1, ymm0, 1
    vaddpd      xmm0, xmm0, xmm1             # ΣL ΣR
    vmovupd     xmmword ptr [rdx], xmm0
    vaddpd      ymm2, ymm2, ymm3
    vextractf128 xmm3, ymm2, 1
    vaddpd      xmm2, xmm2, xmm3             # ΣL² ΣR²
    vmovupd     xmmword ptr [rdx + 16], xmm2
    vaddpd      ymm4, ymm4, ymm5
    vextractf128 xmm5, ymm4, 1
    vaddpd      xmm4, xmm4, xmm5             # ΣL·R ΣL·R
    vmovsd      qword ptr [rdx + 32], xmm4
    vzeroupper
    ret
