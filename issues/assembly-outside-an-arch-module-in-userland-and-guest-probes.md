---
status: open
kind: defect
opened: 2026-09-26
---

# Assembly outside an arch module: the guest probes

The owner's ruling of 2026-09-26 puts every `asm!`, `global_asm!`,
`naked_asm!`, naked function and `core::arch::*` intrinsic inside an
architecture's own module: `kernel/src/arch/<arch>/`, the bootloader's
`bootloader/src/arch/`, and `toyos-abi`'s per-arch syscall entry. The kernel and
the loader now hold none outside those; what is left:

- Guest probes in `tests/toyos-rust-tests/src/bin/`, whose subject
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
- `usbd` (`hc.rs`'s `Ring` and doorbell, `bus.rs`'s EP0 transfers) has the
  same gap twice over: a TRB's body is ordered before its control word with
  `fence`, and nothing orders the control word before the doorbell's MMIO
  store.

**Exit condition**: the SDK gains a per-architecture module (as `toyos-abi`'s
syscall entry and libc's `arch/` are) holding DMA barriers; the guest probes
move under a per-arch directory the harness selects by `Arch`.
