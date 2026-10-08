---
status: open
kind: defect
opened: 2026-10-08
---

# netstack keeps the datagram socket of a client that sent no close

A UDP socket is three things in netstack (`userland/netstack/src/main.rs`):
the smoltcp socket with its two 64 KiB buffers, the id's entry in `sockets`,
and the two pipe ends in `udp_pipes`, each keeping a 2 MiB kernel pipe alive.
A close request removes all three. Nothing else looks at the pipes: a client
that dies, or drops its ends without the request, leaves the socket and its
bound port for the life of the process, and a restarted program that binds
the same port is answered `ERR_ADDR_IN_USE`. The one other way out is
`deliver_datagram`, which ends the socket when a receive that was already
pending writes into a pipe with no reader.

A listener's owner is heard, and only on a pass something else causes:
`serve_piped_listeners` writes zero bytes to every notify pipe each pass and
ends the listener the kernel answers `Gone` for, and no watch wakes a pass for
it, so an idle netstack keeps a dead owner's port until other traffic arrives.

A piped TCP connection is the shape to copy: `bridge_piped` ends it on what
the kernel says of the client's pipe ends, and the watch on its send pipe is
what wakes that pass.

Read from the code, not measured.

**Exit condition**: a datagram socket and a listener whose owner's pipe ends
are closed leave the socket set and the table with no other traffic, and a
guest test on `tests/netcase` that binds each, drops it without a close
request, and reads `net.sockets.udp` and `net.sockets.listeners` back at what
they were.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.
