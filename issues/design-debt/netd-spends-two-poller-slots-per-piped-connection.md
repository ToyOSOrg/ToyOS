---
status: open
kind: defect
opened: 2026-09-26
---

# netd spends two poller slots per piped connection

A piped connection registers its send pipe `READABLE` and, while it holds
bytes its receive pipe refused, its receive pipe `WRITABLE` — both in one pass,
because an ssh receiver sends `WINDOW_ADJUST` while its own receive is held.
`MAX_PIPED_SLOTS` (`userland/netd/src/main.rs`) divides the batch one poller
can carry (`Poller::MAX_HANDLES`, less the two fixed registrations and the
pending connections) by `POLL_HANDLES_PER_PIPED = 2`, so the ceiling on live
piped connections halved from 222 to 111.

The memory budget (an eighth of memory at 4 MiB a connection) is the lower
bound only below 3552 MiB (111 × 4 MiB × 8), so every machine with 4 GiB
or more is held to 111.

Exit condition: one registration per connection. netd's side of a
connection is one kernel `Connection` object, which `read_source` and
`write_source` both resolve (`kernel/src/object/ops.rs`), so one `OP_WATCH`
carries `READABLE | WRITABLE` (`kernel/src/inbox.rs`, `process_watch`), and
`POLL_HANDLES_PER_PIPED` is deleted.
