---
status: open
kind: defect
opened: 2026-09-27
---

# The panic lock spin limit is a count its comment calls a second

`PANIC_LOCK_SPIN_LIMIT` in `kernel/src/drivers/serial.rs` bounds two waits:
`panic_flush`'s for `BackendGuard`, and `flush_final`'s for the console wire.
It is 100,000,000 iterations of a `try_lock` and a `pause`, and its comment
calls that "~1s of spin". Nothing converts it to time, so the wait lasts
whatever that many `pause`s cost on the CPU running them.

**Evidence:** the code.

**Exit condition:** both waits are bounded by time against the counter the
panic path already times its bound with, and the comment says what is measured.
