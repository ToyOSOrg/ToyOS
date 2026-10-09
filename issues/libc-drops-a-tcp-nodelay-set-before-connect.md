---
status: open
kind: defect
opened: 2026-10-08
---

# libc drops a TCP_NODELAY set before connect

Read from the code, not run. `setsockopt` (`userland/libc/src/socket.rs`)
sends `TCP_NODELAY` to netstack only for a socket netstack holds, and a stream
socket is one only from `connect` or `accept`. Set on a socket not yet
connected, the option answers 0, is stored nowhere and is not sent when the
connection is made; `getsockopt` then answers 0 for it, which is true of what
netstack holds and not of what the program was told. Linux keeps the option
across `connect`. `SO_BROADCAST` on a datagram socket not yet bound is kept in
the socket's entry and sent at `bind`; this is the same case with no such
path.

**Exit**: a `TCP_NODELAY` set before `connect` reaches netstack with the
connection or is refused by name, and a guest C case reads it back after
`connect`.

**Owner**: libc, in a libc stage of its own under `issues/toyos-has-its-own-network-stack.md`: it touches only `userland/libc` and guest C cases, and lands before the guest tests that follow the move of netd onto the node.
