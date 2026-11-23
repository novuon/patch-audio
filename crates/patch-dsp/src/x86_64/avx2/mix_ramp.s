# Patch DSP kernel ABI — mix_ramp, AVX2 + FMA (System V AMD64, Intel syntax)
#
# int32_t patch_k_mix_ramp_avx2(const float *const *in, float *const *out,
#                               uint32_t frames, uint32_t channels,
#                               const MixRamp *params, void *state)
#
#   out[c][n] += in[c][n] · (g0 + n·dg)      c < channels, n < frames
#
# Semantics and rounding: identical to reference::mix_ramp — the gain is
# one FMA (n·dg + g0) and the accumulate one FMA (in·g + out), in the same
# order, so the result is bit-exact. 8 frames per iteration; the scalar
# tail uses the same two FMAs. Any `frames` (0 too); unaligned buffers.
#
# In:  rdi = in, rsi = out, edx = frames, ecx = channels, r8 = params,
#      r9 = state (unused)
# Out: eax = 0
# Uses: rax, rcx, rdx, r10, r11, ymm0–ymm11 (all caller-saved);
#       rbx, r12–r15 saved and restored. vzeroupper before return.
# Requires: AVX2, FMA (checked by the registry before use).

    .text
    .globl  patch_k_mix_ramp_avx2
    .p2align 5
patch_k_mix_ramp_avx2:
    push        rbx
    push        r12
    push        r13
    push        r14
    push        r15
    mov         r12, rdi                    # in
    mov         r13, rsi                    # out
    mov         r14d, edx                   # frames
    mov         r15d, ecx                   # channels
    vbroadcastss ymm2, dword ptr [r8]       # g0
    vbroadcastss ymm3, dword ptr [r8 + 4]   # dg
    # lane offsets 0..7, and 1.0 / 8.0 steps
    mov         eax, 0x3f800000
    vmovd       xmm4, eax                   # 1.0 (scalar step)
    vxorps      xmm5, xmm5, xmm5
    vinsertps   xmm5, xmm5, xmm4, 0x10      # 0,1,0,0
    mov         eax, 0x40000000
    vmovd       xmm6, eax
    vinsertps   xmm5, xmm5, xmm6, 0x20      # 0,1,2,0
    mov         eax, 0x40400000
    vmovd       xmm6, eax
    vinsertps   xmm5, xmm5, xmm6, 0x30      # 0,1,2,3
    mov         eax, 0x40800000
    vmovd       xmm6, eax
    vbroadcastss xmm6, xmm6
    vaddps      xmm7, xmm5, xmm6            # 4,5,6,7
    vinsertf128 ymm5, ymm5, xmm7, 1         # 0..7
    mov         eax, 0x41000000
    vmovd       xmm6, eax
    vbroadcastss ymm6, xmm6                 # 8.0 × 8
    xor         ebx, ebx                    # c = 0

.Lch:
    cmp         ebx, r15d
    jae         .Ldone
    mov         r10, [r12 + rbx*8]          # in[c]
    mov         r11, [r13 + rbx*8]          # out[c]
    mov         edx, r14d                   # frames
    vxorps      ymm8, ymm8, ymm8            # n (float) for the block base
    mov         ecx, edx
    shr         ecx, 3
    jz          .Ltail

    .p2align 4
.Lloop:
    vaddps      ymm9, ymm8, ymm5            # n + 0..7
    vfmadd213ps ymm9, ymm3, ymm2            # g = n·dg + g0
    vmovups     ymm10, [r10]
    vmovups     ymm11, [r11]
    vfmadd231ps ymm11, ymm10, ymm9          # out += in·g
    vmovups     [r11], ymm11
    vaddps      ymm8, ymm8, ymm6            # base += 8
    add         r10, 32
    add         r11, 32
    dec         ecx
    jnz         .Lloop

.Ltail:
    and         edx, 7
    jz          .Lnext
.Ltail_loop:
    vmovaps     xmm9, xmm8
    vfmadd213ss xmm9, xmm3, xmm2            # g = base·dg + g0
    vmovss      xmm10, dword ptr [r10]
    vmovss      xmm11, dword ptr [r11]
    vfmadd231ss xmm11, xmm10, xmm9
    vmovss      dword ptr [r11], xmm11
    vaddss      xmm8, xmm8, xmm4            # base += 1
    add         r10, 4
    add         r11, 4
    dec         edx
    jnz         .Ltail_loop

.Lnext:
    inc         ebx
    jmp         .Lch

.Ldone:
    vzeroupper
    xor         eax, eax
    pop         r15
    pop         r14
    pop         r13
    pop         r12
    pop         rbx
    ret
