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
(`kernel/src/arch/x86_64/apic.rs:307-336`) and writes as three APIC register
writes, `TIMER_DIVIDE`, `LVT_TIMER` and `TIMER_INIT` (`apic.rs:355-368`):
MMIO stores under xAPIC, `wrmsr` under x2APIC (`apic.rs:49-64`), which the
T14 runs (`apic=X2apic`,
https://github.com/ToyOSOrg/ToyOS/pull/861#issuecomment-6113257693). A timer fire from Ring 3 writes
`TIMER_INIT` once more before its handler runs
(`kernel/src/arch/x86_64/idt/timer.rs:23-24`, `apic.rs:394-396`), and the
calibration spins the boot for 10 ms (`apic.rs:315-320`, from
`kernel/src/arch/x86_64/boot.rs:135`). TSC-deadline mode (CPUID.01H:ECX[24])
takes the absolute deadline in TSC units as one `IA32_TSC_DEADLINE` write,
with no calibration of a second timebase (SDM 325462-093, Vol. 3A §13.5.4.1).
The T14 enumerates it (`tsc_deadline_timer`,
`kernel/cpuvuln/fixtures/t14/cpuinfo.txt:20`). AMD documents no such mode: its
CPUID reference has CPUID.01H:ECX[24] reserved (APM 24594 rev. 3.36, Vol. 3,
§E.3.2), so the AMD bench keeps the count, its calibration and its spin.

**The published state of the art backs it.** Linux has armed its one-shot
through `IA32_TSC_DEADLINE` on every CPU that enumerates the mode since v3.8:
279f1461432c, by Intel's Suresh Siddha, "enables us to skip the APIC
calibration during boot. Also, in xapic mode, this enables us to skip the
uncached apic access to re-arm the APIC timer". At v7.2 its
`calibrate_APIC_clock` returns at once where the mode is, and
`setup_APIC_timer` registers `lapic-deadline` there
(`arch/x86/kernel/apic/apic.c`); its errata table, `deadline_match`, does not
name the T14's family 6 model 140. Since v7.1 the deadline is handed over in
TSC cycles "without reading the TSC at all" (f246ec3478cf), the absolute
write this stage takes. The stage follows them, and its figure is reported
and decides nothing.

**Measured** (`wt/toyos-scout-tlb` at `d4c2f8ebe`, `main`'s `177e72a9e` plus
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
`set_timer`'s `now()` (`kernel/src/hw.rs:50`), an `rdtsc` and a u128 multiply
and divide (`kernel/pure/clock.rs:10-11`): the count reads the clock to turn
the deadline into a span, and a deadline written as an absolute
`IA32_TSC_DEADLINE` value reads none. Of what the segment times, each figure
is an upper bound on what deadline mode saves, which still writes once to arm
and once to stop; on the AMD bench both sit at its TSC's floor.

**Next: a TSC-deadline twin on the T14**, `main` plus the scout with the
one-shot armed and stopped through `IA32_TSC_DEADLINE`, timed by the same
split beside `main` in the same session, with the re-arm segment opening at
`set_timer` (`kernel/src/hw.rs:49`) in both arms, its reading reported. Its
clock control: each arm reads APERF and MPERF on CPUs 4 and 5 before and
after the split, and where the two arms' ratios of APERF's advance to MPERF's
differ by more than 1 % on either CPU, both arms' readings are void and are
taken again.

**Exit**: the twin's reading, posted; a deadline armed on every CPU that
enumerates TSC-deadline as one `IA32_TSC_DEADLINE` write of the absolute
value; and the T14's 1 ms timer lateness of
`issues/no-program-measures-toyos-against-linux-on-one-machine.md` no worse
than the count's.
