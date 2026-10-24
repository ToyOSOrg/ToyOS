---
status: open
kind: track
opened: 2026-10-12
---

# The timer is a LAPIC count re-armed every pass, never a TSC deadline

Every scheduler pass ends by handing its timer an absolute deadline
(`kernel/pure/sched/cpu.rs:1912-1918`), and the kernel turns it back into a
span from now (`kernel/src/hw.rs:47-51`) that `apic::arm_one_shot` converts to
LAPIC ticks by the boot's calibration against the TSC-derived clock
(`kernel/src/arch/x86_64/apic.rs`) and writes into `TIMER_INIT`, one APIC
register write since #1061 set the divide and the LVT once per CPU: an MMIO
store under xAPIC, a `wrmsr` under x2APIC, which the
T14 runs (`apic=X2apic`,
https://github.com/ToyOSOrg/ToyOS/pull/861#issuecomment-6113257693). A timer fire from Ring 3 writes
`TIMER_INIT` once more before its handler runs
(`kernel/src/arch/x86_64/idt/timer.rs:23-24`, `apic.rs:394-396`), and the
calibration spins the boot for 10 ms (`apic.rs:315-320`, from
`kernel/src/arch/x86_64/boot.rs:135`). TSC-deadline mode (CPUID.01H:ECX[24])
takes the absolute deadline in TSC units as one `IA32_TSC_DEADLINE` write,
with no calibration of a second timebase (SDM 325462-093, Vol. 3A §13.5.4.1).
Whether the T14 enumerates it is ToyOS's own read to make, and no boot prints
CPUID.01H:ECX yet. AMD documents no such mode: its
CPUID reference has CPUID.01H:ECX[24] reserved (APM 24594 rev. 3.36, Vol. 3,
§E.3.2), and so does the AMD bench's own (PPR 55922 rev. 3.06,
`CPUID_Fn00000001_ECX`), so the AMD bench keeps the count, its calibration
and its spin.

**Its speed reading decides it.** The SDM defines the mode and recommends it
over the count nowhere, and AMD documents none, so nothing published covers
the choice. Intel's specification update for the T14's processor lists no
TSC, TSC-deadline or APIC-timer erratum (631123-034, the UP3 line, CPUID
000806C1h). Nothing but speed is its purpose: a deadline removes no path,
since a CPU without one keeps the count.

**Measured before #1061**, when the arm was three register writes and a u128
multiply and divide, so these re-arm figures are the old code's and a twin
compares against the arm read again (`wt/toyos-scout-tlb` at `d4c2f8ebe`, `main`'s `177e72a9e` plus
the scout; https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6119443198),
p50 in TSC ticks per direction of a user ping-pong between CPUs 4 and 5:

| | T14 (x2APIC, 2419 MHz) | AMD bench (xAPIC, 1796 MHz) |
|---|---|---|
| the re-arm, one record's cost subtracted | 518 and 522 (214 and 215 ns) | 18, its TSC's one-quantum floor |
| the timer stop on idle entry | 138 (57 ns; 203 less one 65-tick record) | below its TSC's resolution |
| the same pass's pick and switch, the re-arm inside switch | 2558 + 1866 and 2658 + 1670 | 1260 + 918 and 1224 + 936 |
| whole idle wake | 12029 and 11854 | 5796 and 5724 |

On the T14 the re-arm is 11.7 % and 12.1 % of the pass's pick and switch,
which a pass with no idle wake runs too, and 27.8 % and 31.3 % of the switch
it sits in; the
re-arm and the stop together are 5.5 % and 5.6 % of an idle wake (derived).
The scout's re-arm opens at `arm_one_shot`'s first statement, after
`set_timer`'s `now()` (`kernel/src/hw.rs:50`), an `rdtsc` and the tick
conversion: the count reads the clock to turn
the deadline into a span, and a deadline written as an absolute
`IA32_TSC_DEADLINE` value reads none. Of what the segment times, each figure
is an upper bound on what deadline mode saves, which still writes once to arm
and once to stop; on the AMD bench both sit at its TSC's floor.

**Next: a TSC-deadline twin on the T14**, `main` plus the scout with the
one-shot armed and stopped through `IA32_TSC_DEADLINE`, beside `main` plus
the scout in the same session. Each arm reads the 1 ms timer lateness of
`issues/no-published-run-compares-toyos-with-linux-on-one-machine.md` and the
split above, its re-arm segment opening at `set_timer` (`kernel/src/hw.rs:49`)
in both. Each arm runs 11 times; a run's figure is the median lateness of
its 1 ms timers, and an arm's is the median of its 11 with their spread,
lowest to highest. The deadline measures better only where its median is the
lower and the two spreads do not overlap; where they overlap, the count, the
path every CPU has, is kept. The split is reported. The twin is a measurement
arm, deleted once it has answered.

**Exit**: a boot line printing CPUID.01H:ECX, and a T14 boot carrying it
cited for bit 24; the twin's reading, posted, and the twin deleted. Where the
deadline's lateness measures better, a deadline is armed on every CPU that
enumerates TSC-deadline as one `IA32_TSC_DEADLINE` write of the absolute
value, and the split above is read again after it; where it does not, this
file becomes `rejected` with both readings, and the boot's 10 ms
calibration spin stays the count's price.
