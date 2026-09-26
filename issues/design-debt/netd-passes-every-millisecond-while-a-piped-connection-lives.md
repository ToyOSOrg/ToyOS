---
status: open
kind: defect
opened: 2026-09-26
---

# netd passes every millisecond while a piped connection lives

netd's loop (`userland/netd/src/main.rs`, `main`) waits at most 1 ms whenever
any piped connection or pending asynchronous request exists, whether or not
anything is due. It is a flat wait: every event a piped connection needs has
a wake of its own now — the NIC's interrupt, a send pipe turning readable, a
receive pipe that was holding bytes back turning writable, and smoltcp's own
`poll_delay` for its timers — so by reading of the code the tick moves nothing
an event would not; that is unmeasured.
While it stays, it also hides a missing wake: removing the receive pipe's
`WRITABLE` watch leaves every netd test green, because the next tick moves
the held bytes anyway.

Pending UDP receives are the one path with no wake of its own: a datagram
whose client pipe was full waits in `UdpPipes::owed` for the next pass, and
UDP sockets have no cap, so their pipes cannot be given poller slots the way
piped connections are.

Exit condition: the loop's timeout is the earliest real deadline (smoltcp's
`poll_delay`, a pending connect's deadline, the handshake sweep, mDNS), a
pending UDP delivery has a wake, and a negative control shows a netd test
going red when a watch is removed.
