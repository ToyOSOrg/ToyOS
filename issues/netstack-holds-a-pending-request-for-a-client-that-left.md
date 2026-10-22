---
status: open
kind: defect
opened: 2026-09-26
---

# netstack holds a pending receive for a client that left

A UDP receive with no datagram yet keeps its client's connection in
`Sockets::receiving` (`system/netstack/src/serve.rs`) until a datagram arrives.
Nothing watches that connection (`Sockets::watch` watches each stream's
connection, each listener's wake pipe, each datagram socket's pipe and each
waiting lookup's connection), so a client that hangs up is noticed only when
netstack writes its answer: a receive on a socket nothing sends to holds its
entry for the life of the socket.

A lookup's client is watched and let go at once (`TOKEN_LOOKUP`), and so is a
stream's from its connect: a stream is the connection it was asked on, watched
for its hang-up.

Exit condition: a waiting receive's connection is watched the same way, and a
test of `serve.rs` that hangs up a pending receive sees its entry freed without
any traffic.
