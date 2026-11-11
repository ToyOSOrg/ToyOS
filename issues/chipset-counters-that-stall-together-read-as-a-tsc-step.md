---
status: open
kind: defect
opened: 2026-11-11
---

# Chipset counters that stall together read as a TSC step

Where the TSC and the HPET disagree, x86-64's clock asks the ACPI PM timer
which moved (`kernel/src/arch/x86_64/clocksource.rs`). A PM timer that sides
with the HPET, or a machine with no counting PM timer, is taken as the TSC
having stepped. Nothing tells that from the chipset's counters standing
still together while a sound TSC counts on. The first forward step of a boot
is slewed out of the clock; a second, or one backward, demotes the TSC, and
every process's reading of now then asks the kernel, which counts by the
HPET.

**Owner:** the clock.

**What a sound TSC loses.** The disagreement is found within a second and
logged with every counter's span, and a demotion says `clock: DEMOTED`. What
is wrong is whose it is said to be:

- (a) The HPET and the PM timer stall together twice in one boot. The first
  stall is slewed away, so the clock stands the stall's length behind; the
  second demotes the TSC to the HPET that stalls.
- (b) They stall together once, after the boot's one slew is spent. The AMD
  bench spends it in every boot
  (`issues/the-amd-benchs-tsc-steps-1-7-ms-forward-once-a-boot.md`), so there
  one joint stall demotes.
- (c) The HPET alone stalls twice on a machine with no counting PM timer.

**What the documents allow.** An invariant TSC runs at a constant rate in
every P-, C- and T-state (Intel SDM Vol. 3B, "Invariant TSC"). The HPET's
main counter may stop in S1 or S2, not in S0 (IA-PC HPET Specification 1.0a,
§2.3.3). The PM timer counts through S0 (ACPI 6.5 §4.8.3.3), and the same
section makes it optional. So (a) and (b) take the HPET and the PM timer both
out of specification, and (c) takes the HPET alone.

**Evidence.** Until #1213 the RTC's update-ended edges, off a crystal of
their own, chose between a step and a joint stall, and a span they read as a
stall was left out. Across the 309 branches of the evidence repository on
2026-11-11 that verdict was written in no boot of either bench or of QEMU:
the edges confirmed the step or could not tell, and both took the action the
clock takes now
(https://github.com/ToyOSOrg/evidence/tree/3ae2cecf2b74a953ef15aab6238181433d7165e3/r1,
`rtc-verdicts-published.txt`). Both benches and QEMU have a PM timer that
counts. On the AMD bench the HPET and the PM timer run off one 14.318 MHz
clock by their periods, so their agreement against the TSC there is one
witness.

**Not measured.** A machine whose chipset counters stall in S0. None is on a
bench.

**Exit:** a boot of a supported machine that writes `clock: TSC STEP` or
`clock: DEMOTED` for a span its TSC did not step in reopens the choice, and
the clock then takes a reference outside the chipset's clock for that
machine; or the owner rules that the loss stands and this record becomes
`rejected`.
