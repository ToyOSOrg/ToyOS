---
status: open
kind: defect
opened: 2026-10-28
---

# The system clock takes the TSC on one reading and never checks it again

x86-64's system clock is the TSC. The boot measures it against the HPET
once, over 50 ms (`kernel/src/arch/x86_64/hpet.rs`), and `clock::set_counter`
publishes that one period for the machine's life. Three things the clock
then relies on go unchecked:

- **That the TSC may keep time at all.** CPUID 80000007H:EDX[8], the
  invariant TSC (Intel's SDM "Invariant TSC"; AMD's PPRs
  `CPUID_Fn80000007_EDX[TscInvariant]`), is read only to admit deep idle
  states (`kernel/src/arch/x86_64/idle.rs::facts`). The clock starts on a
  TSC without it the same way.
- **That every CPU's TSC agrees.** Each AP's reading at start is checked to
  fall inside a bracket of the BSP's, and that bracket is about 19.8 M cycles
  wide on the AMD bench, 11 ms (the `SMP:` lines). An offset under that
  passes unseen, and nothing compares the CPUs again.
- **That the rate stays the one measured.** No later reading is set against
  the HPET or another reference, so a step or a change of rate after boot
  goes into every timestamp, timeout and deadline unannounced.

On the AMD bench (Ryzen 7 4800U) the one calibration is also 40 to 66 ppm
off (`issues/the-kernels-50-ms-tsc-calibration-misses-the-rate-by-tens-of-ppm.md`).
That CPU states the invariant bit. Under its Linux, it counts one rate in
every phase over 10 minutes, and over 15 minutes across 16 CPUs and idle
gaps up to 1 s no CPU's TSC read behind another's and no step exceeded a
read's 1.2 µs. But in 5 of 30 boots' kernel logs that Linux's watchdog saw
the TSC some 1.7 ms ahead of the HPET over half a second, 2 to 3 s into
boot. Every time it was the same size, and it came with a calibration that
was right. The cause is unknown (`tsc-probe` and the boots' figures, PR
#1079's evidence). ToyOS's one calibration also runs early in boot. A step
inside its 50 ms would be a 3% error in every timestamp after it, and a
clock that never checks would not see it.

**Exit**: the kernel keeps time on the TSC only where every CPU states it
invariant. It bounds the CPUs' offsets at start to a figure it logs and
derives, and checks the rate and offsets against a reference while it runs,
failing clearly or falling back to a counter that holds when they leave
their bound. Each part is shown on the T14 and the AMD bench.

**Owner**: the kernel's x86-64 clock (`kernel/src/arch/x86_64/hpet.rs`,
`kernel/src/clock.rs`).
