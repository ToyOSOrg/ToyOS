---
status: open
kind: defect
opened: 2026-10-08
---

# std holds a stream's nodelay per duplicate

Read from the code, not run. `TcpStream::try_clone`
(`sdk/std/sys/net/connection.rs`, `duplicate`) shares the one netstack socket
between the stream and its duplicate and gives the duplicate its own copy of
`nodelay`. `set_nodelay` on either changes the socket both name and stores the
value only in the one it was called on, so `nodelay()` on the other answers
what was true before. A host's getter reads the kernel's value and cannot
differ. Two threads setting opposite values on one stream can also reach
netstack in one order and the store in the other. `UdpSocket`'s `broadcast`
is one value shared by every duplicate and locked across its request.

**Exit**: `nodelay` is one value shared by every duplicate of a stream and
locked across the request that changes it, and a guest test sets it through a
duplicate and reads it from the original.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`, with the
reset-stream option test of
`issues/a-stream-its-peer-reset-refuses-the-option-requests-a-host-answers.md`,
which reads the same value.
