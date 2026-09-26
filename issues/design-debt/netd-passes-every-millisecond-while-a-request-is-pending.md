---
status: open
kind: defect
opened: 2026-09-26
---

# netd passes every millisecond while a request is pending

netd's loop (`userland/netd/src/main.rs`, `main`) waits at most 1 ms whenever
a UDP receive, a DNS query or a piped connect is pending, whether or not
anything is due. It is a flat wait standing in for events those requests do
not have:

- a pending UDP receive's datagram wakes the NIC, but a receive whose socket
  another request closes is answered only by some later pass;
- a pending connect's `timeout_ms` deadline is not folded into the loop's
  timeout the way the handshake sweep's is;
- a DNS query is answered by whichever pass finds its result.

A piped connection no longer holds the loop to 1 ms: its peer's bytes wake the
NIC, its client's bytes wake its send pipe's watch while the socket has room,
held bytes wake its receive pipe's watch, and smoltcp's timers are in
`poll_delay`, whose zero is a pass at once.

Exit condition: the loop's timeout is the earliest real deadline, each pending
request is answered on its own event, and no 1 ms constant remains.
