---
status: open
kind: defect
opened: 2026-10-10
---

# x86-64 reads the CMOS for a wall clock the loader already hands it

The loader asks firmware's `GetTime` before `ExitBootServices` on every
architecture and hands the instant, with the counter it was true at, in
`KernelArgs::wall_clock_*` (`bootloader/src/wallclock.rs`). AArch64 anchors
its wall clock on that (`kernel/src/arch/aarch64/rtc.rs`); x86-64 ignores it
and reads the CMOS itself (`kernel/src/arch/x86_64/rtc.rs`), with the FADT's
century register (`kernel/src/drivers/acpi.rs`), so the two architectures keep
two readers of one clock, and on x86-64 the field is written and never read.
Whether the T14's firmware answers `GetTime` from the same CMOS is not
measured.

Exit condition: x86-64 anchors on the loader's reading, and
`arch/x86_64/rtc.rs`, `acpi::rtc_century_register` and their callers are
deleted with it, against a metal row reading the T14's wall clock; or a
measurement on the T14 that its firmware's `GetTime` answers wrongly where the
CMOS answers right, recorded at `arch/x86_64/rtc.rs`, and this file deleted.
