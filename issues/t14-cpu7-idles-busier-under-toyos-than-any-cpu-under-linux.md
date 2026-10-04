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

The readbacks put a stick write inside the second; that it is cpu7's excess
is not yet shown. Every counters boot so far places the
`/log` fileserver on cpu7, and the row's second begins about 8 ms after
logkeeper starts, while it is still writing the boot so far to the stick: on
both arms of pull request #728's run the stick's first sync is a record
stamped 9 to 10 ms inside the measured second (`usb-storage: disk 0 does not
implement SYNCHRONIZE CACHE`, at 1.182 s against `idle0` at 1.173 s), and cpu7
read 1.40% with the row's own print moved after the second and 1.43% without.
A scout that discarded a warm-up second first read cpu7 at 0.12% in seconds
nothing printed into. The row now waits for the log to hold its own line
before `idle0`. Owner: the orchestrator, which holds the T14. **Exit**: a T14
reading of the row with that wait in which cpu7's idle busy fraction is within
cpu0 to cpu6's; then this folds to the row's doc.
