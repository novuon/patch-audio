# Patch recording unit — x86-64 kernels (System V ABI, Intel syntax).

    .text

# ---------------------------------------------------------------------------
# void patch_rec_s24_avx2(const float *src, size_t n, uint8_t *dst,
#                         uint32_t rng[8], const float *dither, ConvStats *st)
#
# float32 → 24-bit little-endian PCM with TPDF dither, 8 samples per
# iteration (any channel count; `n` counts samples, must be a multiple of 8).
#
#   y = clamp(round_even(x · 2²³ + d · (u1 − u2)), −2²³, 2²³ − 1)
#
# u1, u2 ∈ [0, 1) come from eight independent xorshift32 generators (one per
# lane, state in rng[8], two steps per sample). *dither is the dither peak in
# LSB (1.0 = standard TPDF, 0.0 = none). Each 32-bit result is packed to 3
# bytes with vpshufb; every 16-byte store overlaps the next by 4 bytes, so
# `dst` needs 4 bytes of slack after n·3.
#
# st->clips  (u64) += samples with |x| > 1.0 before conversion
# st->peak   (f32)  = max(st->peak, |x|)
# The Rust reference performs the same float operations in the same order.
#   rdi = src, rsi = n, rdx = dst, rcx = rng, r8 = dither, r9 = st
# ---------------------------------------------------------------------------
    .globl  patch_rec_s24_avx2
    .p2align 5
patch_rec_s24_avx2:
    vmovdqu     ymm15, [rcx]                # rng state (8 × u32)
    vbroadcastss ymm14, dword ptr [r8]      # dither amount (LSB)
    mov         eax, 0x4b000000             # 8388608.0f
    vmovd       xmm13, eax
    vbroadcastss ymm13, xmm13               # scale 2²³
    mov         eax, 0x3f800000             # 1.0f
    vmovd       xmm12, eax
    vbroadcastss ymm12, xmm12               # 1.0
    mov         eax, 0x4afffffe             # 8388607.0f
    vmovd       xmm11, eax
    vbroadcastss ymm11, xmm11               # +max
    mov         eax, 0xcb000000             # -8388608.0f
    vmovd       xmm10, eax
    vbroadcastss ymm10, xmm10               # −max
    vpcmpeqd    ymm9, ymm9, ymm9
    vpsrld      ymm9, ymm9, 1               # abs mask
    vbroadcastss ymm8, dword ptr [r9 + 8]   # running peak
    xor         r10, r10                    # clip count
    # vpshufb mask: bytes 0,1,2, 4,5,6, 8,9,10, 12,13,14 of each lane; rest zero
    mov         rax, 0x0908060504020100
    vmovq       xmm7, rax
    mov         rax, 0xffffffff0e0d0c0a
    vpinsrq     xmm7, xmm7, rax, 1
    shr         rsi, 3
    jz          .Ls24_done

    .p2align 4
.Ls24_loop:
    vmovups     ymm0, [rdi]                 # x
    # --- stats ---
    vandps      ymm1, ymm0, ymm9            # |x|
    vmaxps      ymm8, ymm8, ymm1
    vcmpps      ymm2, ymm1, ymm12, 0x1e     # |x| > 1.0 (GT_OQ)
    vmovmskps   eax, ymm2
    popcnt      eax, eax
    add         r10, rax
    # --- u1: xorshift32 step, to float [0,1) ---
    vpslld      ymm2, ymm15, 13
    vpxor       ymm15, ymm15, ymm2
    vpsrld      ymm2, ymm15, 17
    vpxor       ymm15, ymm15, ymm2
    vpslld      ymm2, ymm15, 5
    vpxor       ymm15, ymm15, ymm2
    vpsrld      ymm3, ymm15, 9
    vpor        ymm3, ymm3, ymm12           # 1.mantissa
    vsubps      ymm3, ymm3, ymm12           # u1 ∈ [0,1)
    # --- u2 ---
    vpslld      ymm2, ymm15, 13
    vpxor       ymm15, ymm15, ymm2
    vpsrld      ymm2, ymm15, 17
    vpxor       ymm15, ymm15, ymm2
    vpslld      ymm2, ymm15, 5
    vpxor       ymm15, ymm15, ymm2
    vpsrld      ymm4, ymm15, 9
    vpor        ymm4, ymm4, ymm12
    vsubps      ymm4, ymm4, ymm12           # u2
    # --- y = x·2²³ + d·(u1 − u2), clamp, round ---
    vsubps      ymm3, ymm3, ymm4            # TPDF in (−1, 1)
    vmulps      ymm3, ymm3, ymm14
    vmulps      ymm0, ymm0, ymm13
    vaddps      ymm0, ymm0, ymm3
    vminps      ymm0, ymm0, ymm11
    vmaxps      ymm0, ymm0, ymm10
    vcvtps2dq   ymm0, ymm0                  # round to nearest even
    # --- pack 8 × 24-bit ---
    vpshufb     xmm5, xmm0, xmm7            # low lane: 12 bytes
    vextracti128 xmm6, ymm0, 1
    vpshufb     xmm6, xmm6, xmm7            # high lane: 12 bytes
    vmovdqu     [rdx], xmm5
    vmovdqu     [rdx + 12], xmm6
    add         rdi, 32
    add         rdx, 24
    dec         rsi
    jnz         .Ls24_loop

.Ls24_done:
    vmovdqu     [rcx], ymm15
    add         [r9], r10
    vextractf128 xmm1, ymm8, 1
    vmaxps      xmm8, xmm8, xmm1
    vmovhlps    xmm1, xmm8, xmm8
    vmaxps      xmm8, xmm8, xmm1
    vshufps     xmm1, xmm8, xmm8, 0x55
    vmaxss      xmm8, xmm8, xmm1
    vmovss      [r9 + 8], xmm8
    vzeroupper
    ret
