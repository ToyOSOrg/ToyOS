---
status: open
kind: defect
opened: 2026-09-26
---

# Assembly outside an arch module: the guest probes

The owner's ruling of 2026-09-26 puts every `asm!`, `global_asm!`,
`naked_asm!`, naked function and `core::arch::*` intrinsic inside an
architecture's own module: `kernel/src/arch/<arch>/`, the bootloader's
`loader/src/arch/`, and `toyos-abi`'s per-arch syscall entry. The kernel and
the loader now hold none outside those; what is left:

- Guest probes in `tests/guest/rust-tests/src/bin/`, whose subject
  is an x86 instruction (`rdgsbase`, `fxsave64`, `int1`, x87 control words)
  or the raw `syscall` gate with arguments no SDK call will pass. They run in
  the x86-64 suite, which is the only suite until the harness gains its arch
  axis (the port's stage 8).
- The userland drivers order their DMA rings through
  `toyos_device_memory::DmaBuffers`'s `publish` and `observe`, which a claimed
  function's `Grant` (`lib/pci-claim/src/lib.rs`) answers with
  `fence(Release)`/`fence(Acquire)`; usbd's rings (`system/usbd/xhci/src/hc.rs`)
  put a TRB's body before its control word, and the control word before the
  doorbell, through `dma.publish()`. That is not assembly and no rule reds on
  it, but it is the same missing interface: on AArch64 a `fence` is `dmb ish`,
  which orders nothing a device outside the inner-shareable domain observes
  (the kernel's `arch::barrier` says why, and `Mmio` carries `writel`/`readl`
  ordering there).
- The NVMe driver writes its own `fence(Release)` between a submission entry
  and its doorbell (`submit`, `system/nvme/src/nvme.rs`), outside
  `DmaBuffers`, with the same gap.

**Exit condition**: the SDK gains a per-architecture module (as `toyos-abi`'s
syscall entry and libc's `arch/` are) holding DMA barriers; the guest probes
move under a per-arch directory the harness selects by `Arch`.
