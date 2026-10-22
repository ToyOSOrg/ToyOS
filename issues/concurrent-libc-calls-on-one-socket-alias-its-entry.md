---
status: open
kind: defect
opened: 2026-10-22
---

# Concurrent libc calls on one socket alias its entry

libc keeps each socket's state in `static mut SOCKETS` (`sdk/libc/src/socket.rs`),
and every socket call reaches its entry as `&mut Option<SocketEntry>` through
`Socket::slot`. A descriptor held for the call keeps `release_socket` off the
entry, but nothing serializes two calls on one socket: a `recv` blocked on one
thread holds a reference to the entry while a `connect`, `bind`, `setsockopt`
or a second `recv` on another thread makes its own `&mut` to it. Two live
`&mut` to one place are undefined behaviour in Rust, and a `connect` that
writes `stream` or `local_port` under a reader is a data race on plain memory.

**What it costs.**
- Two concurrent `connect`s on one socket each open a stream and each write `entry.stream`. The second write overwrites the first, which leaks that stream's handle, its region and its netstack connection for the life of the process.
- A `recv` that reads `stream` while a `connect` writes it is a real concurrent access to plain memory, and not only a rule of the type system.

Read from the code, not reproduced.

Owner: the orchestrator.

Exit:
- A socket's entry is read once into each call and written only under a lock of its own.
- That lock is never held across a blocking call, so one thread's `recv` does not serialize another's `send`.
- libc's socket calls make no `&mut` into `SOCKETS` that another call can hold at the same time.
- Two concurrent `connect`s on one socket leave one stream and refuse the other, as POSIX's `EALREADY` or `EISCONN`.
