---
status: assigned
kind: defect
opened: 2026-10-22
---

# A libc socket closed under another thread's call ends its process

libc's `sock_from_fd` (`sdk/libc/src/socket.rs`) holds a socket's descriptor
only for its lookup in `toyos::fd::with`. Every socket call then works from
the entry's raw handles after that hold has ended: `send` and `recv` call
`toyos::net::stream::write` and `read` with the stream's connection handle;
`accept` blocks reading a listener's wake pipe; `recvfrom` reads a datagram
socket's receive pipe. `poll` resolves the same handles through `stream_of`
and `watched_of` and holds none of them.

So a `close` on another thread meanwhile is the descriptor's last reference.
The table releases the socket at once (`release_socket` → `let_go` →
`stream::let_go` → `syscall::close`). The thread still in the call then makes
its next syscall on a closed handle: a drain, a ring, or `sys_read`'s loop
re-resolving the handle after the hang-up wakes it. The kernel answers that
with `HandleError::Stale` as a handle fault, which ends the **whole process**
(`kernel/src/object/handle.rs`). Linux leaves the reader blocked, and the
socket goes once its call returns. A server that closes a connection from one
thread while another reads it dies.

Reported by the socket-transfer design's review r4 from the code; reproduced
by `tests/guest/rust-tests/src/bin/libc_recv_beside_close.rs`.

Owner: the socket-transfer implementer, on
`wt/libc-socket-close-mid-call`.

Exit: every libc socket call and `poll` holds its descriptors until it
returns, and that test passes, red with the hold dropped before the call.
