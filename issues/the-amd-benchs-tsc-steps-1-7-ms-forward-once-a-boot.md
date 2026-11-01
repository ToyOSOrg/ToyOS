---
status: open
kind: defect
opened: 2026-10-31
---

# The AMD bench's TSC steps 1.7 ms forward once a boot

On the AMD bench (Lenovo 82A2, BIOS DMCN38WW; Ryzen 7 4800U, CPUID
signature 0x860f01, Family 17h Model 60h), the TSC jumps 1.69–1.73 ms ahead
once in every boot. The HPET, the ACPI PM timer and the RTC agree on it
within 37 µs. The RTC runs off its own 32.768 kHz crystal, and the other two
off the FCH's 14.318 MHz clock. Linux on the same machine sees the event in
5 of 30 boots.

**Owner:** the clock (`kernel/src/arch/x86_64/clocksource.rs`) and
acpiserver, which hands over the idle state the step lands in.

**Where it lands.** Inside one spell in which every CPU sits in port 0x415
(s1, the deepest state acpiserver hands over), early in the boot. One CPU
reads the HPET as each spell ends:
- in `benchcase`, spells 0 to 2 read no step (the third lasted 1.66 ms), and
  the TSC is 1.70 ms ahead from the end of spell 3;
- in `testcases`, the boot's first such spell already ends 1.72 ms ahead.

The stepping spell's TSC length includes the jump. No later spell steps.
`benchcase` reaches its first spells at about 17 s, when `osbench_sched`'s
all-core load ends; `testcases` reaches them at about 5 s, right after the
hand-over. The trigger is the package's deep idle with every core in 0x415.
Whether the TSC's jump across it is the platform's or the state's entry is
not known.

**What the clock does meanwhile.** Readers see the forward step: the clock
reads up to 1.7 ms ahead from the step until the sampler finds it, within a
second, and through the RTC's few seconds after. A forward step the HPET and
the PM timer agree on is then slewed away at no more than 500 ppm. This
happens whether the RTC confirms the step or cannot tell, and the correction
never steps back. A second step, or a backward one, demotes the TSC
(`issues/a-demoted-tsc-keeps-the-kernels-clock-apart-from-processes.md`).

**Not read.** AMD's *Processor Programming Reference for Family 17h Model 60h*
(55922), which would cover the TSC in deep C-states and the TSC's relation to
the FCH's clocks. amd.com refuses the build's user agent, so it has not been
fetched.

**Exit:** the cause found and removed, or documented by AMD, so that no AMD
boot takes a step.
