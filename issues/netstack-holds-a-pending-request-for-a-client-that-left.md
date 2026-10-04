---
status: open
kind: defect
opened: 2026-09-26
---

# netd holds a pending request for a client that left

A UDP receive with no datagram yet and a piped connect waiting for its SYN-ACK
keep their client's connection in `pending_udp_recvs` and
`pending_piped_connects` (`userland/netd/src/main.rs`) until what they wait
for happens. Neither watches the connection, so a client that hangs up is
noticed only when netd writes its answer: a receive on a socket nothing sends
to holds its entry for the life of the socket, and a connect with no timeout
holds its socket and its piped-connection slot until smoltcp gives the
handshake up.

A lookup's client is watched and let go at once (`resolve::Resolver::let_go`,
reached from the `TOKEN_LOOKUP_BASE` watches in `main`).

Exit condition: both pending lists watch their clients' connections the same
way, and a guest test that hangs up a pending receive and a pending connect
sees the socket and the slot freed without any traffic.
