---
status: open
kind: defect
opened: 2026-10-11
---

# netstack's receiver round trip reads no finer than a millisecond

A downloading stream's round trip is the receiver's own estimate
(`system/netstack/shard/tcp/src/rx.rs`): the age of an echoed TSval. TSval
ticks once a millisecond, so an echo's age is a whole number of milliseconds
and never less than one tick. A path whose round trip is under a millisecond,
a LAN's, reads as at least 1 ms.

What that moves is the receive buffer's growth, twice what the user read per
round trip (`rx.rs`): a 200 µs path that reads 1 ms grows its buffer to five
times what it needs. That figure is derived from the tick and the growth rule.
No machine has measured a sub-millisecond path.

**Exit**: a stream on a sub-millisecond path grows its receive buffer to twice
what the user reads in the path's own round trip, read by a host test that
delivers echoes on such a path.
