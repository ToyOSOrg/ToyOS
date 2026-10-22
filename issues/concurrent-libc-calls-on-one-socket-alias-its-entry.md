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

Read from the code, not reproduced: no test observes a miscompile, and the
fields a call reads after its lookup are ones a concurrent call rarely writes.

Owner: the orchestrator.

Exit: a socket's entry is reached only under a lock of its own, or its fields
are read once into the call and written only under that lock; and libc's
socket calls make no `&mut` to `SOCKETS` that another call can hold at the
same time.
