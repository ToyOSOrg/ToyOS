---
status: open
kind: defect
opened: 2026-10-11
---

# A receive flow with no TCP round trip is paced at the floor

The pass's cadence (`system/netstack/src/cadence.rs`) bounds the card's
interrupt interval by the least round trip of the TCP streams a batch moved,
and holds a batch that moved none to `FLOOR` (30 µs). A sustained flow that
moves no TCP stream gets that floor: UDP, which includes QUIC, and a flood of
frames for nobody. The I219 takes 30 µs as 28.7 µs, so on such a flow the card
may wake netstack about 35,000 times a second. Before the cadence the fixed
128 µs throttle held it to about 7,800. The floor is an estimate of one wake's
cost, so at the floor the wakes can take a whole CPU.

Netstack has no round trip for such a flow. QUIC's round trip is its
endpoint's, inside the app, and a flood has none. Pacing it at the ring's
bound would add up to that bound to every datagram's latency.

**Exit**: a sustained receive flow that moves no TCP stream costs no more
wakes per megabyte than a TCP download at the same rate, with its latency
bound stated. A T14 row reads both.
