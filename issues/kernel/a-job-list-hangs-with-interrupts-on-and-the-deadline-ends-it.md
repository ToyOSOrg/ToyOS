---
status: open
kind: defect
opened: 2026-09-08
---

# A job list hangs with interrupts on, and only the deadline ends it

T14 run 25 (`/Users/jan/.claude/jobs/2280e09e/tmp/t14-run25-metal.log`, lines
26-47). The `ccorpus` image — 118 C cases that had run in 49 s on the previous
tip — came back after **187 s**, so its boot spent the whole
`boot-deadline=120000` and the deadline is what ended it.

**That narrows it past the hang this branch already fixed.** A deadline that
fires is a machine where some CPU was still taking a timer interrupt, so this is
not the all-CPUs-deaf shape of
`kernel/src/hw.rs`'s missing re-arm; and `crate::hardlockup` sealed nothing, so
no CPU sat with `IF` clear for half the bound either. What is left is a CPU
spinning with interrupts enabled, or a wait that never returns, while the timer
goes on ticking — a lock nobody releases, a retry loop with no end, or a device
wait that re-arms its own bound.

It is intermittent **across job lists**: `ccorpus` had passed, `testcases-mkdir`
hung in run 22, `metaldevicecase` in run 20 showed the same span of
`LOCK CONTENTION ... at src/vfs.rs:32` under stick writes.

**The record exists and could not be read.** The deadline seals `EXPIRED` and
the tail of the log ring into the black box, and the next loader pass prints it
into `loader.log` on the stick — but this reset cut a bulk transfer in its data
phase and left the stick unenumerable, so there was no readback at all. The stop
now waits a transfer out first (`xhci/stop.rs`'s `settle_transfers`), so the
next occurrence should leave a stick the driver can read.

**Exit condition**: a `loader.log` from a T14 boot that ran to its deadline,
with the ring tail naming what the machine was doing when it stopped.
