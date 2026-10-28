---
status: open
kind: defect
opened: 2026-10-28
---

# The kernel's 50 ms TSC calibration misses the TSC's rate

The kernel measures the TSC against the HPET once, over 50 ms
(`kernel/src/arch/x86_64/hpet.rs`), and keeps time at that rate for the
whole boot. On the AMD bench (Ryzen 7 4800U) that rate is consistently low.
The TSC counted 1796.6248 MHz against the same HPET over 10 minutes and
against the ACPI PM timer over 5, the same in every idle and busy phase
(`tsc-drift` under the bench's Linux, 2026-10-28, PR #1079's evidence). All
but one of the kernel's calibrations in the session's AMD boot logs read
1796.506 to 1796.551 MHz, 41 to 66 ppm low, and 1796.519 to 1796.532 MHz in
most; the one other reads 1796.597 MHz, 15 ppm low. A rate read low is a
period kept long (`hpet.rs` stores HPET femtoseconds per TSC cycle), so every
interval reads long: at 58 ppm a ToyOS clock on that machine gains about 5 s
a day.

The error is a bias, not noise: every reading falls on one side. It is not
the period's resolution either: one step of the femtoseconds the kernel
stores is 3,227 Hz there, 1.8 ppm, and truncation biases the other way. An
inference, not measured: the HPET is read before the first TSC read and after
the last, so the HPET interval brackets the TSC interval by up to two
uncached MMIO reads. About 1.5 µs over 50 ms is about 30 ppm low, the sign
and size seen.

The T14 states its TSC's rate (CPUID 15H, 2,419,200,000 Hz), and the kernel
logs its own measurement beside it. Over 5 minutes against the HPET under
its Linux, the TSC counted 2419.2002 MHz, the stated rate within 0.1 ppm,
worst residual 0.2 µs (`tsc-drift`, 2026-10-28). The session's T14 boot logs
hold two clusters, 285 ppm under and 59 ppm over, in 345 and 250 log files
(some of them copies of one boot), and a few readings between and beyond
them (404 to 440 and 759 to 766 ppm under). Those are the kernel's error: the
285 ppm cluster gains about 25 s a day and the 59 ppm one loses about 5. The
bracketing above explains neither, which needs 14 µs and the other sign.
That the two clusters are two kernel revisions is an inference no one has
checked. On that machine a stated rate is there to be used, and right, and
the kernel keeps its measurement instead.

**Exit**: on the T14 and the AMD bench, the rate the kernel keeps is within a
bound the fix derives and logs. That bound is set by its window, its reads'
cost and the reference's own accuracy, against the rate measured over minutes
against the same reference: `tsc-drift` on the AMD bench, and CPUID 15H or
`tsc-drift` on the T14.

Whether the TSC may keep time at all, and whether it still does, is
`issues/the-system-clock-takes-the-tsc-on-one-reading-and-never-checks-it-again.md`.

**Owner**: the kernel's x86-64 clock (`kernel/src/arch/x86_64/hpet.rs`).
