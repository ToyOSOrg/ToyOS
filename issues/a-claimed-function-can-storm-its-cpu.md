---
status: open
kind: defect
severity: [kernel-from-userland]
opened: 2026-10-04
---

# A claimed function can storm its CPU

A claim's message is masked from the message its handler takes until its
holder reads the record (`kernel/src/pcidev/record.rs`), so a function
interrupts at most once per read. That bounds it per read and not per unit of
time, and three things remain.

**A holder paces cpu0.** A holder that loops on forcing a cause through its
own registers and reading its record, on any CPU, still raises one cpu0
interrupt per iteration. Each one now also costs cpu0 an invalidation
completion wait with interrupts off: VT-d's interrupt-entry-cache
invalidation, AMD-Vi's completion wait, or the ITS's `SYNC`. Both are charged
to whatever cpu0 interrupted, not to the holder.

**A refused message is recorded nowhere.** While masked, a message is target
aborted (AMD-Vi) or blocked with fault processing disabled (VT-d), and
neither the unit nor the claim's record counts it. On AArch64 a disabled LPI
stays pending and folds into one.

**No T14 row shows cpu0's other sources served under a holder that
provokes its function.** `i219_msi`'s storm phase shows a forced storm on
an unread claim stopped at the entry, and not the timer.

Owner: the IOMMU track. Exit: cpu0's interrupts from a claim are bounded in
time, and the work they cost is charged to their holder, by a bound the
kernel derives and states; a message refused past it is counted against
the claim; and a T14 row with a holder that loops force-and-read shows
cpu0's timer and other sources still served.
