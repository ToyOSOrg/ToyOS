---
status: open
kind: defect
opened: 2026-10-28
---

# The kernel keeps time on a TSC it never asks is invariant

x86-64's clock is the TSC, measured once against the HPET over 50 ms at boot
(`kernel/src/arch/x86_64/hpet.rs`), and every timestamp after that converts
TSC ticks at the rate that one window found. Nothing reads CPUID
80000007H:EDX[8], which Intel (the SDM's "Invariant TSC") and
AMD (its PPRs' `CPUID_Fn80000007_EDX[TscInvariant]`) define as the TSC
counting at one rate across P-, C- and T-states. A CPU without it keeps a
wrong clock silently: every timeout, deadline and measurement on it is off
by however its rate moved, and no line says so.

The AMD bench (Ryzen 7 4800U) is why this was looked at. Its Linux's
clocksource watchdog measured the TSC at 1802.69 MHz against the HPET over
one 502 ms interval 2.6 s into boot, where ToyOS's calibration against the
same HPET reads 1796.52 MHz, about 3400 ppm apart; Linux marked the TSC
unstable and fell back to the HPET. Whether that CPU's TSC drifts is being
measured (`issues/the-amd-benchs-linux-reads-its-clock-through-a-syscall.md`),
and the bit is missing here whatever the measurement shows. The bit's
document citations above are the fix's to read at the site; this record
names them and has not quoted them.

**Exit**: the kernel reads the bit on every CPU before it keeps time on the
TSC, and a CPU that does not state it fails the boot by name or keeps time
on a counter that is invariant. Each answer covers a guest whose CPU model
leaves the bit clear. QEMU's TCG `qemu64` is one, an inference not yet
checked against QEMU's model.

**Owner**: the kernel's x86-64 clock (`kernel/src/arch/x86_64/hpet.rs`).
