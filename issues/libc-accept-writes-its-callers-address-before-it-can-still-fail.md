---
status: open
kind: defect
opened: 2026-10-09
---

# libc's accept writes its caller's address before it can still fail

`accept` (`userland/libc/src/socket.rs`) fills the caller's `addr` and
`addrlen` with the peer's address as soon as netstack answers, and only then
asks `alloc_socket` for a descriptor. With the table of `MAX_SOCKETS` full
that fails: `accept` closes the connection and returns -1 with `ENOMEM`,
having already written the caller's buffer and its length. Read from the
code, not run: no test fills the table, and no host was asked what it leaves
in the buffer of an `accept` it refuses for want of a descriptor.

It is the one failure of `accept` left that touches the caller's buffer, now
that it asks netstack nothing after its answer. The word is not POSIX's
either, which names `EMFILE` for a process out of descriptors.

**Exit condition**: `accept` takes its descriptor before it writes `addr`,
and a guest C case that fills the socket table reads the buffer and its
length unchanged after the refused `accept`, with `EMFILE`.

**Owner**: libc, `userland/libc`; whoever next changes its `accept`.
