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

Where one CPU's TSC is apart from the others' (written or found behind),
processes' time holds at the furthest-ahead CPU's clock at the handover, or
up to the last retune's change of rate times the time since it past that,
until the HPET's count reaches it. An offset an AP's start proves demotes
before any process runs, so processes' time starts on the HPET's count.
Either way the kernel's clock on each CPU stays apart by that CPU's offset.

The handover floors each CPU along the line in force and the one it replaced,
which covers a process that read either. A process that read the page along
an older line, at least two retunes before the demotion, can read behind its
last reading. Retunes come a second apart by the sampling CPU's clock, so a
CPU that leads the previous sampler by λ samples about 1 s − λ after it. A
reading on a leading CPU is then covered only while the lead stays under
about a second, where consecutive samples come from CPUs that far apart.
Past that, the answer reads behind the process's last reading by at most the
change of rate times the lead: 0.5 ms per second of lead at a slew's
500 ppm.

**Owner:** the clock.

**Exit:** after a demotion, the kernel's clock follows the reference too, so
the kernel and processes read one clock again, and the handover floors along
every line a process could have read.
