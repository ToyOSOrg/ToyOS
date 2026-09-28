---
status: open
kind: defect
opened: 2026-09-27
---

# A killed shutdown caller panics on its second drain backoff

`writeback::drain_all` (`kernel/src/writeback.rs`) retries a refused drain
through `block::between_attempts`, whose park is cancellable, and discards what
it answers. A thread whose kill bit is set gets `Cancelled` from that park at
once, retries, and parks again; `TaskHandle::take_cancel` asserts on the second
cancel reported to one thread, so the kernel panics. `ops::until_answered`
reads the kill bit before it parks and does not have this; `drain_all` does not.

The caller is the shutdown syscall (`syscall/machine.rs`), which a kill posted
before `quiesce::stop` can still reach, and it takes a second backoff only when
the device refuses two drain attempts on budget. Not reproduced.

Exit condition: `drain_all`'s retry either stops parking once its caller is
killed or parks uncancellably, with a test that kills a caller parked in a
refused drain.
