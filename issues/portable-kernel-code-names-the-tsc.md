---
status: open
kind: defect
opened: 2026-09-29
---

# Portable kernel code names the TSC

On AArch64 the CPU's counter is the generic timer's `CNTVCT_EL0`, and the
portable kernel still calls it the TSC: `kernel/src/clock.rs`'s
`tsc_deadline`, `TSC_BOOT` and `TSC_PERIOD_FS`, `kernel/src/deadline.rs`'s
`AT_TSC`, and the xHCI driver's, `hardlockup`'s and `panic_reboot`'s waits
and comments read it by that name. `clock::counter_ticks`, which the AArch64
timer reads, is renamed; the rest reads as x86-64's on both machines.

**Exit condition**: no item or comment outside `kernel/src/arch/x86_64/`
names the TSC for the counter `crate::arch::cpu::counter` reads.
