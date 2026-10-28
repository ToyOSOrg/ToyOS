---
status: open
kind: defect
opened: 2026-10-28
---

# The kernel's 50 ms TSC calibration misses the rate by tens of ppm

The kernel measures the TSC against the HPET once, over 50 ms
(`kernel/src/arch/x86_64/hpet.rs`), and keeps time at that rate for the
whole boot. On the AMD bench (Ryzen 7 4800U) that rate is consistently low.
Over 10 minutes, the TSC counted 1796.6248 MHz against the same HPET and
against the ACPI PM timer, the same in every idle and busy phase
(`tsc-drift` under the bench's Linux, 2026-10-28, PR #1079's evidence). The
kernel's calibrations in the session's AMD boot logs read 1796.506 to
1796.551 MHz, and 1796.519 to 1796.532 MHz in most of them: 40 to 66 ppm
low. None reads above the true rate; the highest, once, is 1796.597 MHz, 15
ppm low. At 58 ppm a ToyOS clock on that machine loses about 5 s a day.
Every interval it times reads that much short.

The error is a bias, not noise: every reading falls on one side. It is not
the window's resolution either: one step of the femtosecond period the
kernel stores is 3,227 Hz there, 1.8 ppm. Where it comes from, the HPET
reads' cost against the window or the order of the reads, is unread.

The T14 states its TSC's rate (CPUID 15H, 2,419,200,000 Hz), and the kernel
logs its own measurement beside it. The session's T14 boot logs hold two
clusters, 285 ppm under and 59 ppm over, in 344 and 250 log files (some of
them copies of one boot), and a few readings between and beyond them. That the two clusters are two kernel
revisions is an inference no one has checked. On that machine a stated rate
is there to be used, and the kernel keeps its measurement instead.

**Exit**: on the T14 and the AMD bench, the rate the kernel keeps is within a
bound the fix derives and logs. That bound is set by its window, its reads'
cost and the reference's own accuracy, against the rate measured over minutes
against the same reference: `tsc-drift` on the AMD bench, and CPUID 15H or
`tsc-drift` on the T14.

Whether the TSC may keep time at all, and whether it still does, is
`issues/the-system-clock-takes-the-tsc-on-one-reading-and-never-checks-it-again.md`.

**Owner**: the kernel's x86-64 clock (`kernel/src/arch/x86_64/hpet.rs`).
