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
interrupt for a `K`th of the least round trip. On a 200 µs path that round
trip reads 1 ms, so the cadence asks for 31.25 µs where a `K`th of the true
one, 6.25 µs, would give the 30 µs floor. The I219 keeps three leading bits
and applies both as 28.7 µs, so on that part the tick costs such a path
nothing today; a card that applies an interval exactly would hold 31.25 µs
for 30. Either is about 15% of the round trip, not a `K`th: under about
960 µs the floor sets the interval, and the tick only moves it above. These
figures are derived from `K = 32`, the tick and the rounding. No machine has
measured a sub-millisecond path.

**Exit**: a stream on a sub-millisecond path reports its round trip to within
a `K`th of the path's own, read by a host test that delivers echoes on such a
path.
