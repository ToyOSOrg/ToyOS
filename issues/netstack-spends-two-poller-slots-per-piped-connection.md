---
status: open
kind: defect
opened: 2026-09-26
---

# netstack spends two poller slots per piped connection

A piped connection registers both its pipes on every pass, for as long as
netstack holds them: its send pipe `READABLE` while the socket has send room
and `OTHER_END_GONE` until the kernel has answered that, and its receive pipe
`OTHER_END_GONE` and, while it holds bytes the pipe refused, `WRITABLE`
(`userland/netstack/src/main.rs`, the loop in `main`). `MAX_PIPED_SLOTS`
divides the batch one poller can carry (`Poller::MAX_HANDLES`, less the fixed
registrations, the pending connections and the lookups' clients) by
`POLL_HANDLES_PER_PIPED = 2`, so the ceiling on live piped connections is half
what one registration each would give.

The memory budget (an eighth of memory at 4 MiB a connection) binds first only
on a machine whose eighth holds fewer connections than that ceiling.

**Exit condition**: one registration per connection, and
`POLL_HANDLES_PER_PIPED` deleted. One `OP_WATCH` on a joined `Connection`
carries `READABLE | WRITABLE` for both its pipes, and is refused
`OTHER_END_GONE`: `ops::pipe_end_watch` (`kernel/src/object/ops.rs`) answers a
pipe end alone, and a connection has two other ends for the one bit. So the
exit also needs the kernel to say of a connection which of its directions has
no holder left.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.
