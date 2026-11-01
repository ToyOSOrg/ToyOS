---
status: open
kind: defect
opened: 2026-11-01
---

# A demoted TSC keeps the kernel's clock apart from processes'

When the clock demotes the TSC (`kernel/src/arch/x86_64/clocksource.rs`,
`demote`), processes' readings of now move to the HPET through
`SYS_CLOCK_NOW`. The kernel's own clock, its deadlines and its log stamps stay
on the TSC at the kept rate. Every later TSC step therefore moves the two
clocks apart, and a log line a process stamps no longer orders against one
the kernel stamps.

A backward step plays out like this:
1. Every reading, the kernel's and processes', goes back by the step.
2. The sampler, or a CPU's check, finds it within a second and demotes the
   TSC.
3. Processes' time jumps forward to the HPET's count from the last held
   sample, which is the true time, and stays monotonic from there.
4. The kernel's clock stays behind by the step.

After a forward second step, processes' time holds at the TSC's clock until
the HPET's count reaches it, then follows the HPET. The kernel's clock stays
ahead by the step.

Where one CPU's TSC is apart from the others' (written, found behind, or an
offset an AP's start proves), processes' time holds at the furthest-ahead
CPU's clock at the demotion until the HPET's count reaches it. The kernel's
clock on each CPU stays apart by that CPU's offset.

**Owner:** the clock.

**Exit:** after a demotion, the kernel's clock follows the reference too, so
the kernel and processes read one clock again.
