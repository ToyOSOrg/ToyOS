---
status: open
kind: defect
opened: 2026-10-04
---

# T14 cpu7 idles busier under ToyOS than any CPU under Linux

The `counters` metal row reads each CPU's MPERF over its stamp across the idle
second between its `idle0` and `idle1` reads. Linux's turbostat on the same
machine, idle (`tests/t14-linux/turbostat-idle.txt`), reads 0.11 to 0.36%
machine-wide per 10 s and no CPU above 0.69%.

cpu7 reads about four times that, on every boot so far: 1.45% at `ce1786ff0`
(pull request #705), 1.42% at `a059144e2`, and 1.86% and 1.85% on the two arms
of pull request #725's run.

cpu0 read 1.37% and 1.35% on the first two boots. That was the idle loop
spinning on an `irq_ring` record `xhci::poll_if_pending` left when a USB-stick
transfer held `XHCI`; since `XHCI` became an `OwedLock`, cpu0 reads 0.50% with
the change (`a65205a81`) and 1.77% with the whole change reverted
(`b0cae9a19`), cpu1 to cpu6 0.45 to 0.68% on both arms.

Not yet attributed: the row's own reader runs between the two reads, prints
its `idle0` lines and parks, and which CPUs it and the log's path ran on that
second is not recorded. Owner: the orchestrator, which holds the T14.
**Exit**: a reading that names what ran on cpu7 across that second; then the
cause is fixed, or, if it is the row's own work, folded to the row's doc.
