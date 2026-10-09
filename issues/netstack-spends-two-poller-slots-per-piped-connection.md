---
status: open
kind: defect
opened: 2026-09-26
---

# netstack spends two poller slots per piped connection

A stream registers both its pipes on every pass, for as long as the node holds
them: its send pipe `READABLE` while the stack has room for its bytes and
`OTHER_END_GONE` until the kernel has answered that, and its receive pipe
`OTHER_END_GONE` and, while the stack holds bytes the pipe refused, `WRITABLE`
(`userland/netstack/src/serve.rs`, `Sockets::watch`, from the node's
`Node::watches`). `MAX_PLACES` (`userland/netstack/src/main.rs`) divides the
batch one poller can carry (`Poller::MAX_HANDLES`, less the fixed
registrations, the pending connections and the lookups' clients) by
`serve::WATCHES_PER_PLACE = 2`, so the ceiling on the node's places is half
what one registration each would give.

The memory budget (an eighth of memory at what a listener can be made to hold,
`PLACE_BYTES`) binds first only on a machine whose eighth holds fewer places
than that ceiling.

**Exit condition**: one registration per connection, and
`WATCHES_PER_PLACE` deleted. One `OP_WATCH` on a joined `Connection`
carries `READABLE | WRITABLE` for both its pipes, and is refused
`OTHER_END_GONE`: `ops::pipe_end_watch` (`kernel/src/object/ops.rs`) answers a
pipe end alone, and a connection has two other ends for the one bit. So the
exit also needs the kernel to say of a connection which of its directions has
no holder left.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.
