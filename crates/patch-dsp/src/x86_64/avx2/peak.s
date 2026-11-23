# Patch DSP kernel ABI — peak, AVX (System V AMD64, Intel syntax)
#
# int32_t patch_k_peak_avx2(const float *const *in, float *const *out,
#                           uint32_t frames, uint32_t channels,
#                           const void *params, void *state)
#
#   out[c][0] = max |in[c][n]|   (0 when frames = 0)
#
# |x| clears the sign bit (vandps with 0x7fffffff). The running maximum is
# the *second* operand of every vmaxps/vmaxss, which is what those return
# when the first is NaN: NaN samples are ignored, exactly like the
# reference (a NaN never compares greater). ±Inf reads +Inf. Two
# independent 8-lane accumulators (16 frames per iteration) hide the
# latency of vmaxps; then 8 at a time, then a scalar tail. Bit-exact.
#
# In:  rdi = in, rsi = out, edx = frames, ecx = channels
# Out: eax = 0
# Uses: rax, rcx, rdx, r8–r11, ymm0–ymm5 (caller-saved). vzeroupper.
# Requires: AVX (the registry files it under AVX2 with the other kernels).

    .text
    .globl  patch_k_peak_avx2
    .p2align 5
patch_k_peak_avx2:
    mov         eax, 0x7fffffff
    vmovd       xmm0, eax
    vbroadcastss ymm0, xmm0                 # abs mask
    xor         r8d, r8d                    # c = 0
    mov         r9d, edx                    # frames

.Lpk_ch:
    cmp         r8d, ecx
    jae         .Lpk_done
    mov         r10, [rdi + r8*8]           # in[c]
    mov         r11, [rsi + r8*8]           # out[c]
    vxorps      ymm1, ymm1, ymm1            # acc0
    vxorps      ymm2, ymm2, ymm2            # acc1
    mov         edx, r9d
    mov         eax, edx
    shr         eax, 4
    jz          .Lpk_eight

    .p2align 4
.Lpk_loop16:
    vandps      ymm3, ymm0, [r10]
    vandps      ymm4, ymm0, [r10 + 32]
    vmaxps      ymm1, ymm3, ymm1            # NaN in ymm3 → keep ymm1
    vmaxps      ymm2, ymm4, ymm2
    add         r10, 64
    dec         eax
    jnz         .Lpk_loop16

.Lpk_eight:
    test        edx, 8
    jz          .Lpk_fold
    vandps      ymm3, ymm0, [r10]
    vmaxps      ymm1, ymm3, ymm1
    add         r10, 32

.Lpk_fold:
    vmaxps      ymm1, ymm2, ymm1            # acc1, acc0 (no NaN in either)
    vextractf128 xmm2, ymm1, 1
    vmaxps      xmm1, xmm2, xmm1
    vmovhlps    xmm2, xmm2, xmm1
    vmaxps      xmm1, xmm2, xmm1
    vshufps     xmm2, xmm1, xmm1, 0x55
    vmaxss      xmm1, xmm2, xmm1
    and         edx, 7
    jz          .Lpk_store
.Lpk_tail:
    vmovss      xmm3, dword ptr [r10]
    vandps      xmm3, xmm3, xmm0
    vmaxss      xmm1, xmm3, xmm1            # NaN → keep the running max
    add         r10, 4
    dec         edx
    jnz         .Lpk_tail

.Lpk_store:
    vmovss      dword ptr [r11], xmm1
    inc         r8d
    jmp         .Lpk_ch

.Lpk_done:
    vzeroupper
    xor         eax, eax
    ret
