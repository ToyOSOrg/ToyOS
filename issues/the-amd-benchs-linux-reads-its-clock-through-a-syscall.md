---
status: open
kind: tooling
opened: 2026-10-27
---

# The AMD bench's Linux reads its clock through a syscall

On 2026-10-24, osbench's `time.now` (`Instant::now` a million times a round)
read 1,212,895 ps a call under the AMD bench's Ubuntu (Linux 6.8.0-147).
Under the T14's it read 14,302 ps, with the same static binary. The AMD's
Linux keeps time on the HPET: 2.6 s into the one boot whose log was read,
its clocksource watchdog found the TSC 1,698,571 ns ahead of the HPET over
502,290,025 ns, marked it unstable and switched. At that cost each timestamp
is a syscall, an inference from the cost, and every per-operation timing of
that Linux carries about 1.2 µs that ToyOS's, which reads the TSC, does not.
That is the baseline's own behaviour: osbench records `clocksource`, and the
README treats a Linux forced onto the TSC as an experiment, never the
baseline.

What the watchdog's interval was is unknown. Its own figures put the TSC at
1802.69 MHz across it. Over 10 minutes later in the boot the TSC counts
1796.6248 MHz against the HPET and against the ACPI PM timer, in every idle
and busy phase, worst residual 23 µs, and the CPU states an invariant TSC
(2026-10-28, `tsc-drift`, PR #1079's evidence). A steady average can still
hide a step. The candidates are:
- Linux's calibration at that point;
- a reference read delayed inside the interval;
- an offset between the CPUs the two reads ran on;
- the TSC's or the HPET's rate or state changing around that time in boot.

**Exit**: the interval's cause is known, from per-CPU offsets and steps
measured across idle and load and from the watchdog's figures in more than
one boot. Then either the bench's Linux keeps its default and this records
why, or the cause is a ToyOS-relevant TSC defect, filed against ToyOS's
clock.

**Owner**: the orchestrator, which runs the benches. A change to the bench's
Linux boot configuration is the owner's.
