---
status: open
kind: defect
opened: 2026-10-04
---

# The loader's lines carry no time on an x86 CPU that states no TSC rate

`bootloader/src/stamp.rs` converts the cycle counter at the rate CPUID leaves
15H/16H state (`toyos_tsc::stated_hz`), and a CPU that states neither gets
unstamped lines and one `Loader clock: this CPU states no counter rate` line.
QEMU's `qemu64`, which every x86-64 guest of the suite boots under TCG, is one:
`screen_loader_clears`'s console carries that line and no stamps. AMD parts
enumerate neither leaf either, so an AMD machine's loader is as blind.

The T14 states 2419200000 Hz in 15H, and the AArch64 loader reads `CNTFRQ_EL0`,
so neither is affected.

Exit: the loader's lines carry their milliseconds on every x86-64 machine the
suite and the metal loop boot, `qemu64` included.
