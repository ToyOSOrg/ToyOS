---
status: open
kind: defect
opened: 2026-09-26
---

# Assembly outside an arch module: two framebuffers and the guest probes

The owner's ruling of 2026-09-26 puts every `asm!`, `global_asm!`,
`naked_asm!`, naked function and `core::arch::*` intrinsic inside an
architecture's own module: `kernel/src/arch/<arch>/`, the bootloader's
`src/arch/`, and `toyos-abi`'s per-arch syscall entry. `src/sourcegate.rs`'s
`ARCH_RULES` enforces it. The kernel and the loader now hold none outside
those; what is left is declared in that table as an exception, each row
pointing here:

- `userland/toyos-window/src/framebuffer.rs` and `userland/metalprobe/src/fb.rs`:
  `_mm_sfence` after writing a write-combining framebuffer. Userland has no
  portable way to say "drain my stores to the scanout"; the SDK (`toyos/src`)
  owes one, and it is also only changed under an ABI brief.
- Seventeen guest probes in `tests/toyos-rust-tests/src/bin/`, whose subject
  is an x86 instruction (`rdgsbase`, `fxsave64`, `int1`, x87 control words)
  or the raw `syscall` gate with arguments no SDK call will pass. They run in
  the x86-64 suite, which is the only suite until the harness gains its arch
  axis (the port's stage 8).
- The userland drivers `netd` (`virtio_net.rs`, `i219.rs`) and `soundd`
  (`virtio.rs`) order their DMA rings with `fence(Release)`/`fence(Acquire)`.
  That is not assembly and no rule reds on it, but it is the same missing
  interface: on AArch64 a `fence` is `dmb ish`, which orders nothing a device
  outside the inner-shareable domain observes (the kernel's `arch::barrier`
  says why, and `Mmio` carries `writel`/`readl` ordering there).

**Exit condition**: the SDK gains a per-architecture module (as `toyos-abi`'s
syscall entry and libc's `arch/` are) holding a scanout flush and DMA barriers; the guest probes move under a per-arch
directory the harness selects by `Arch`; and every row in `ARCH_RULES` that
cites this file is deleted.
