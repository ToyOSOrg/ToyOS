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
an event would not.

No test can see a missing wake. `netd_slow_reader`, whose held bytes are what
the receive pipe's `WRITABLE` watch exists for, stayed green (EXIT=0) with
that watch removed, with the tick removed, and with both removed — by reading,
because the peer's zero-window probes wake netd too and the bytes move a
probe interval late instead of never.

Pending UDP receives are the one path with no wake of its own: a datagram
whose client pipe was full waits in `UdpPipes::owed` for the next pass, and
UDP sockets have no cap, so their pipes cannot be given poller slots the way
piped connections are.

Exit condition: the loop's timeout is the earliest real deadline (smoltcp's
`poll_delay`, a pending connect's deadline, the handshake sweep, mDNS), a
pending UDP delivery has a wake, and a test that bounds how late held bytes
move goes red when the `WRITABLE` watch is removed.
