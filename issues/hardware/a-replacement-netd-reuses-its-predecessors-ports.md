---
status: open
kind: defect
opened: 2026-09-24
---

# A replacement netd opens its first connection on its predecessor's port

netd's ephemeral ports start at 49152 in every process (`NetDaemon::new`,
`next_local_port`). A swapped netd is a second process on the same address, and
the connections the first one held end with it: no FIN, no reset, and the peer
still has them established. So the new netd's first connect, which is `logd`
asking for its record stream again, goes out on the same address and port pair
the peer's stale connection has.

Measured on `wt/toyos-swap` (QEMU, e1000e, slirp): `logd`'s reopening
connect answered `Ok(49152)` 714 ms after it was asked. The SYN hits slirp's
still-established socket, and only a reset from the new stack clears it before
a retransmitted SYN gets through. A peer that treats a SYN on an established
connection differently decides whether and when the stream comes back, not
netd.

Before `metaltalk::Stream` ended an old connection when a new one arrived, the
collision was also what let `lan_swap` pass. A reopened stream on a fresh port
(forced by starting the second netd's ports elsewhere) left the host reading the
dead connection for ever: 3 of 3 red with the CI failure's exact words.

RFC 6056's randomised ephemeral port selection is the usual remedy. The
alternative is a restarted stack that keeps quiet before it opens anything.
