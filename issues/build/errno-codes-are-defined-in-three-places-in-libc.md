---
status: open
kind: defect
opened: 2026-09-29
---

# errno codes are defined in three places in libc

`userland/libc/src/misc.rs`, `posix_io.rs` and `socket.rs` each declare their
own private `const E*: i32` values, duplicating `include/errno.h`'s numbering
by hand in each file. The codes do not have one owner, so the three lists can
drift out of agreement with `errno.h` and with each other.

**Exit**: `errno.rs` (or a module it names) defines each code once, and
`misc.rs`, `posix_io.rs` and `socket.rs` use that copy.
