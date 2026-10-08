---
status: open
kind: defect
opened: 2026-10-08
---

# libc writes a whole sockaddr into a shorter buffer

Read from the code, not run. `fill_sockaddr`
(`userland/libc/src/socket.rs`), which `accept`, `recvfrom`, `getpeername`
and `getsockname` answer through, writes sixteen bytes at the caller's
address and sets `*addrlen` to 16 without reading what `*addrlen` held. A
caller whose buffer is shorter has the bytes past it overwritten. POSIX: "If
the actual length of the address is greater than the length of the supplied
sockaddr structure, the stored address shall be truncated", with `*addrlen`
set to the address's own length.

**Exit**: each of the four calls writes at most `*addrlen` bytes and answers
the address's length, and a host test of the write holds a buffer shorter
than the address.

**Owner**: libc, under `issues/toyos-has-its-own-network-stack.md`.
