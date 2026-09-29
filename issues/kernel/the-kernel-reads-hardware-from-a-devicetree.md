---
status: open
kind: track
opened: 2026-09-29
---

# The kernel reads hardware from a devicetree

Owner ruling, 2026-09-29: devicetree support matters on its own, not only for
the Raspberry Pi (`issues/kernel/toyos-runs-on-the-raspberry-pi.md`). EBBR
boards describe their hardware by devicetree rather than ACPI tables, and
RK3588 boards' SMMUv3 is described only in mainline devicetree — their
community UEFI's IORT does not name it. ToyOS discovers hardware from a
devicetree as well as from ACPI; which one a machine hands it is a boot-time
fact, not a build-time choice, and the two are read by one kernel binary.

This supersedes `issues/kernel/toyos-runs-on-arm64.md`'s 2026-09-26 "ACPI
only, no device tree path" ruling for boards that need one; that file is not
rewritten here, only cited, and is updated when the ARM64 track is next
picked up.

**The devicetree crossed a trust boundary before the kernel reads it** — a
boot loader assembles it, and on U-Boot it can come from removable media —
so it is untrusted input like any other: malformed structure is refused, and
nothing it contains ever panics the kernel.

What ToyOS reads from it, to start: the memory map, the interrupt controller
(GICv2 or GICv3, `issues/kernel/toyos-runs-on-the-raspberry-pi.md` needs the
former), the UART, PCIe host bridges, and the SMMU. Its first two users are
EBBR boards booting through U-Boot and RK3588 boards' SMMUv3.

Exit: the kernel decodes a devicetree blob for these five and boots an EBBR
board on it; a malformed or truncated blob is refused rather than trusted,
checked by a test that feeds it a corrupted tree and asserts refusal, not a
panic.
