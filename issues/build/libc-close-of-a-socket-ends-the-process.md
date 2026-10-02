---
status: open
kind: defect
opened: 2026-10-01
---

# libc's close of a socket ends the process

Read from the code, not run. A socket's descriptor is `1024` plus its index in
libc's socket table (`userland/libc/src/socket.rs`, `SOCKET_FD_BASE`), and no
handle. `close` (`userland/libc/src/posix_io.rs`) passes every descriptor to
`SYS_CLOSE`, and the kernel answers a handle its caller does not hold by
ending the caller (`kernel/src/syscall/handles.rs`, `sys_close`, through
`HandleError::refuse`). `close_socket`, which closes the socket's pipes and
tells netd, is exported under `no_mangle` and called by nothing; the header
gate excuses it by this file (`toyos-libc-copies/src/prototypes.rs`).

**Exit**: `close` of a socket's descriptor releases the socket and its pipes
and answers 0, `close_socket` is no export, and a guest C case closes a
connected socket and goes on.
