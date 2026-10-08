---
status: open
kind: defect
opened: 2026-10-08
---

# netstack cuts a departed client's unsent tail at the ceiling

A piped connection whose client holds neither pipe end has `OWNERLESS_LIFE`,
100 seconds from the client's leaving, to finish on the wire
(`userland/netstack/src/main.rs`, `ownerless`). The bound is absolute: at the
ceiling the socket is reset whatever it still owes its peer. A program that
writes and exits is the ordinary case of a client that is gone, so when its
peer takes longer than 100 seconds to acknowledge what was written, the peer's
stream ends in a reset with the tail undelivered: whatever is left of the
64 KiB send buffer, and whatever the client's send pipe still held, up to the
pipe's 2 MiB.

The cut is loud at both ends: netstack logs it and the peer reads a reset, not
a short stream. Nobody is left to tell on the client's side.

Hosts do otherwise for an orphaned connection with data still to send: it is
cut on no progress or under orphan pressure and not on a clock from the close.
From knowledge of Linux's `tcp_orphan_retries` and `tcp_max_orphans`, not
measured here.

The bound is absolute because the slot table has no per-client share
(`issues/netstack-lookup-slots-have-no-per-client-share.md` records the same of
the lookups): a peer that acknowledges a byte at a time would hold a slot of a
client that is gone for as long as it liked, and
`max_piped_connections` such peers hold every slot.

Read from the code, not measured: the bench rows that reach the ceiling
(`userland/netstack/src/listen/tests.rs`) cut a connection whose peer
acknowledges nothing.

**Exit condition**: a connection with no client whose peer keeps
acknowledging new bytes is kept while it makes progress, under a bound on how
many such connections one peer or one departed client may hold; and a bench
row whose peer acknowledges one segment a second past the ceiling sees the
whole tail and a FIN.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.
