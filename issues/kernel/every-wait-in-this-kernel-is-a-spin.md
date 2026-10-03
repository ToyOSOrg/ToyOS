---
status: assigned
kind: track
opened: 2026-08-12
---

# Every wait in this kernel is a spin, and a killed task dies by having its stack discarded

**The heading is the state at opening, not the state of the tree.** What exists
now: the completion core, where every wait in
the kernel rechecks one predicate and a waiter lends a watch to the object it
waits on (`aaddf38a^:kernel/src/completion/mod.rs:1-8`, since folded into `kernel/src/watch.rs`); typed durations; and a sleep
lock a contender parks on (`kernel/src/sleeplock.rs`). What has not moved is the
disk path: BOT runs one command in flight per controller and `with_disk`
(`kernel/src/drivers/xhci/mod.rs`) holds the controller lock for the whole
of it (`kernel/src/drivers/xhci/wait/msc.rs:1-4`), which is why
`kernel/CLAUDE.md:12` still reads "No disk wait in this kernel can park".

Held by `issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`,
whose step 10 moves the xHCI to usbd; `issues/hardware/xhci-waits-are-spins.md`
carries that path's exits.

**The commitment: one completion primitive, one inbox, one park site, one
recheck predicate, and a kill answered by `Cancelled` at the park rather than by
discarding the stack.** This kernel does not unwind, so a killed task holding a
live kernel stack must be schedulable at every safe point and must die by
returning through that stack.

None of `drain_zero_handles`'s three drain sites can park, so no
`on_zero_handles` hook may take a sleep lock.

Measured, and worth carrying: a 2 ms-per-transfer stick takes the worst wake
from **7,117 µs to 165,948 µs** at smp=1, and 6,174 µs to 250,912 µs under load
at smp=8. The audio period is 2.902 ms against a 23.219 ms pipeline. The
scheduler migration cost about **70 defects** in code whose own suites were
green, which is the calibration for how this one is landed.
