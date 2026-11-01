---
status: open
kind: defect
opened: 2026-11-01
---

# A kernel copy a short way above its source modulo 4 KiB stalls

The kernel's `memcpy` is `compiler_builtins`' one `rep movsb`
(`issues/kernel-forward-copies-and-fills-are-one-rep-movsb-or-stosb-on-every-cpu.md`).
`osbench copyat` (measurement only, branch `amd-memcpy-ab`) timed `rep movsb`
over 8 KiB with the destination `d` bytes above the source modulo 4096, p50:

| bench | d = 0 | d = 16 | d = 64 | d = 256 | d >= 1024 |
|---|---|---|---|---|---|
| AMD (Ryzen 7 4800U) | 80 ns | 2515 ns | 80 ns | 80 ns | 80-100 ns |
| T14 (i5-1135G7) | 58 ns | 751 ns | 696 ns | 191 ns | 53-92 ns |

AMD's Family 17h judges store-to-load forwarding by address bits 11:0 alone
(56305 rev. 3.02, §2.12), so each load matches an unrelated older store. The
kernel copies between its own buffers and the direct map's view of user
frames at whatever distance the two happen to have: a user copy, a pipe's
gather, a message's ring. Userland's `copy_forward` copies such a pair highest
first (`sdk/libc/src/arch/x86_64.rs`); the kernel's does not.

**Exit**: a kernel copy into a user buffer 16 bytes above its source modulo
4 KiB, measured on both benches, within twice the same copy at distance 0.

**Owner**: the kernel's copy (the sysroot's `compiler_builtins` `mem`).
