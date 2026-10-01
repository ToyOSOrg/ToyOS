---
status: open
kind: defect
opened: 2026-10-01
---

# A ring's completions are behind an IRQ lock no handler takes

`kernel/src/inbox/mod.rs` keeps what a completion writes, and the ring's page,
behind an `IrqLock`, which masks interrupts while it is held. That was for a
device's interrupt handler, whose post wrote a poll's completion in place.
Since a post only owes the poll a look (`kernel/src/inbox/polls.rs`), every
completion is written by the ring's own submitter or by the
`handler-post` actuator's `Staged` ring, both in thread context, and no
handler reaches the lock. Each completion written still masks interrupts for
the write, and `handler-post`'s middle arm (`in_a_ring`) stages a handler
inside a section no handler's post enters any more.

**Exit**: the completions sit behind a lock that leaves interrupts open, and
`handler-post` stages only sections a handler's post can reach.
