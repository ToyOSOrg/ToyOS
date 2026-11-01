---
status: open
kind: defect
opened: 2026-10-22
---

# Concurrent libc calls on one socket alias its entry

libc keeps the state of each socket that is no stream yet in `static mut SOCKETS`
(`sdk/libc/src/socket.rs`), and every call on such a socket reaches its entry as
`&mut Option<SocketEntry>` through `Socket::slot`. A descriptor held for the call keeps
`release_socket` off the entry, but nothing serializes two calls on one socket: a
`connect`, `bind`, `accept` or `setsockopt` on one thread makes its own `&mut` to the entry
while another call on another thread holds one. Two live `&mut` to one place are undefined
behaviour in Rust, and a write to the entry under another call's read is a data race on
plain memory. A connected socket is its stream's handle and no entry, so `send`, `recv` and
`read` on one touch no entry.

**What it costs.**
- Two concurrent `connect`s on one socket each open a stream, and the second's
  `fd::replace` lets go of the first's: one connect answers 0 for a stream it no longer
  holds, where POSIX answers the second `EALREADY` or `EISCONN`.
- A `bind` or `setsockopt` racing a `connect` reads and writes the entry the `connect` is
  reading.

Read from the code, not reproduced.

Owner: the orchestrator.

Exit:
- A socket's entry is read once into each call and written only under a lock of its own.
- That lock is never held across a blocking call, so one thread's `recv` does not serialize another's `send`.
- libc's socket calls make no `&mut` into `SOCKETS` that another call can hold at the same time.
- Two concurrent `connect`s on one socket leave one stream and refuse the other, as POSIX's `EALREADY` or `EISCONN`.
