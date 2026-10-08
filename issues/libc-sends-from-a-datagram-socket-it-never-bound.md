---
status: open
kind: defect
opened: 2026-10-08
---

# libc sends from a datagram socket it never bound

Read from the code, not run. netstack holds a datagram socket from `bind`
(`toyos::net::udp_bind`), which is also where libc's socket entry gets its two
pipe ends. `sendto` (`userland/libc/src/socket.rs`) does not bind a socket
that was never bound: it writes the datagram to the entry's `tx_fd`, which is
0 and names whatever handle 0 is in the process, and then asks netstack to
send from socket id 0. POSIX binds an unbound datagram socket to an ephemeral
port at its first send, and a C program that broadcasts, `socket`,
`setsockopt(SO_BROADCAST)`, `sendto`, does exactly this. std has no such
socket: `UdpSocket::bind` is its only constructor.

That program does not compile either: `userland/libc/include/netinet/in.h`
defines no `INADDR_BROADCAST`.

**Exit**: the first `sendto` on an unbound datagram socket binds it to a port
netstack chooses, handing over a `SO_BROADCAST` already set as `bind` does,
`netinet/in.h` defines `INADDR_BROADCAST`, and a guest C case sends from a
socket it never bound.

**Owner**: libc, in a libc stage of its own under `issues/toyos-has-its-own-network-stack.md`: it touches only `userland/libc` and guest C cases, and lands before the guest tests that follow the move of netd onto the node. The C case the track's broadcast line names for the sequence without
`bind` waits on this.
