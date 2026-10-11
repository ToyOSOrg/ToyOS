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

The pass's cadence (`system/netstack/src/cadence.rs`) holds the card's
interrupt for a `K`th of the least round trip, never under its 160 µs floor,
so with `K = 32` every path under 5.12 ms is held to the floor and the tick
moves no interval. What it moves is the receive buffer's growth, twice what the
user read per round trip (`rx.rs`): a 200 µs path that reads 1 ms grows its
buffer to five times what it needs. These figures are derived from the floor,
`K`, the tick and the growth rule. No machine has measured a sub-millisecond
path.

**Exit**: a stream on a sub-millisecond path reports its round trip to within
a `K`th of the path's own, read by a host test that delivers echoes on such a
path.
