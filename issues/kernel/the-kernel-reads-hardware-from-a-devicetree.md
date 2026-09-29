---
status: open
kind: track
opened: 2026-09-29
---

# The kernel reads hardware from a devicetree

**Parked, blocked on the ARM exit** (`issues/kernel/toyos-runs-on-arm64.md`,
"Exit"): nothing here starts before it.

Owner ruling, 2026-09-29: devicetree support matters on its own, not only for
the Raspberry Pi (`issues/kernel/toyos-runs-on-the-raspberry-pi.md`). ToyOS
discovers hardware from a devicetree as well as from ACPI.

**The devicetree crossed a trust boundary before the kernel reads it** — a
boot loader assembles it, and on U-Boot it can come from removable media —
so it is untrusted input like any other: malformed structure is refused, and
nothing it contains ever panics the kernel.

What ToyOS reads from it, to start: the memory map, the interrupt controller
(GICv2 or GICv3, `issues/kernel/toyos-runs-on-the-raspberry-pi.md` needs the
former), the UART, PCIe host bridges, and the SMMU. `KernelArgs` carries no
devicetree today.

Exit: on QEMU `virt`, with no ACPI tables, the kernel boots on the devicetree
its firmware passes and reads these five from it; a malformed or truncated
blob is refused rather than trusted, checked by a test that feeds it a
corrupted tree and asserts refusal, not a panic.
