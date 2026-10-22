---
status: open
kind: defect
opened: 2026-09-23
---

# netstack's socket ids are numbers any client can name

A stream is its connection and is named by no number, but a listener and a
datagram socket are each named by a `u32` id netstack hands out, and every
request that acts on one (close, accept, send, receive) names it by that id
alone (`Sockets::listener` and `Sockets::datagram`,
`system/netstack/src/serve.rs`). netstack checks that the id is in its table and
nothing else: **it does not ask whether the client naming it is the one it was
issued to.** Clients open a fresh connection per request
(`toyos::net::NetstackConn::connect`), so there is no client identity for the
check to be made against. Any process holding the `netstack` connector can
close or accept on any other process's listener, or receive on its datagram
socket, by guessing its number.

**How it was found.** A service swap (`toyos-swap`) replaced netd, netstack's
predecessor, while its clients still held ids from the netd before. sshd, on
noticing, dropped its listener, which closed the listener's old id, a number
that in the new netd was `logd`'s stream connect still waiting for its SYN-ACK,
and netd's next pass panicked in smoltcp (`tests/swapcase`,
`swap_crash_rolls_back`, one run of four). A client's words crashed netd.

**What bounds it now.** A stream, a pending connect among them, is not an id
any more, and the table starts at a random id, so two netstacks' ranges overlap
only by chance. None of that is the fix: an id is
still ambient authority over somebody else's socket.

**Exit condition.** A listener and a datagram socket are each named by
something only their owner holds (a per-socket handle, or the connection that
made it, as a stream already is), and a request naming a socket its connection
does not hold is refused. That is an ABI change to `toyos::net` and netstack's
wire protocol, and lands on its own.
