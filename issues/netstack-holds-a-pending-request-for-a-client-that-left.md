---
status: open
kind: defect
opened: 2026-09-26
---

# netd holds a pending request for a client that left

A UDP receive with no datagram yet and a connect waiting for its handshake keep
their client's connection in `Sockets::receiving` and `Sockets::connecting`
(`userland/netstack/src/serve.rs`) until what they wait for happens. Neither
watches the connection, so a client that hangs up is noticed only when netstack
writes its answer: a receive on a socket nothing sends to holds its entry for
the life of the socket, and a connect with no timeout holds its stream and its
place until [tcp] gives the handshake up.

A lookup's client is watched and let go at once (`Node::let_go`, reached from
the `TOKEN_LOOKUP` watches in `Sockets::watch`).

Exit condition: both pending lists watch their clients' connections the same
way, and a guest test that hangs up a pending receive and a pending connect
sees the socket and the slot freed without any traffic.
