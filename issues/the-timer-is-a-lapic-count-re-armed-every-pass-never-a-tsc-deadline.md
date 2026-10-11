---
status: open
kind: track
opened: 2026-10-12
---

# The timer is a LAPIC count re-armed every pass, never a TSC deadline

Every scheduler pass ends by handing its timer an absolute deadline
(`kernel/pure/sched/cpu.rs:1912-1918`), and the kernel turns it back into a
span from now (`kernel/src/hw.rs:47-51`) that `apic::arm_one_shot` converts to
LAPIC ticks by the boot's calibration against the HPET and writes as three
MMIO stores, `TIMER_DIVIDE`, `LVT_TIMER` and `TIMER_INIT`
(`kernel/src/arch/x86_64/apic.rs:307-361`). A timer fire from Ring 3 writes
`TIMER_INIT` once more before its handler runs
(`kernel/src/arch/x86_64/idt/timer.rs:23-24`, `apic.rs:394-396`). TSC-deadline mode
(CPUID.01H:ECX[24]) takes the absolute deadline in TSC units as one
`IA32_TSC_DEADLINE` write, with no calibration of a second timebase. Whether
each proving machine enumerates it is unread, and what a re-arm costs is
unmeasured.

It waits on the T14's idle-exit split
(`issues/idle-is-hlt-so-every-wake-of-an-idle-cpu-is-an-ipi.md`), which
times the re-arm with it.

**Exit**: the re-arm's cost on the T14 and the AMD bench, posted, and each
bench's CPUID.01H:ECX[24] read. On every CPU that enumerates TSC-deadline, a
deadline is armed as one `IA32_TSC_DEADLINE` write of the absolute value,
and the T14's 1 ms timer lateness of
`issues/no-program-measures-toyos-against-linux-on-one-machine.md` is no worse
than the count's.
