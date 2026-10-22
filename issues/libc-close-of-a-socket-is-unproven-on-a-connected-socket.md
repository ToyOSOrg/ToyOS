---
status: open
kind: defect
opened: 2026-10-01
---

# libc's close of a socket is unproven on a connected socket

A socket's descriptor names an object of libc's own in the process's one
descriptor table (`toyos::fd::Object::opaque`, `sdk/libc/src/socket.rs`), so
`close` (`sdk/libc/src/posix_io.rs`) releases it like any descriptor, and its
release closes the socket's pipes and stream and tells netstack. Before the
table, a socket's number was `1024` plus its index, `close` passed it to
`SYS_CLOSE`, and the kernel ended the caller for naming a handle it did not
hold. `close_socket` is gone. Read from the code; no guest case has run it.

**Exit**: a guest C case closes a connected socket and goes on.
