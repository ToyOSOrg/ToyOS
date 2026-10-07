---
status: open
kind: defect
opened: 2026-08-24
---

# `std::os::toyos::io` names its raw-handle traits `AsRawFd` and `FromRawFd`

`os::toyos::*` is ToyOS's own extension API and speaks ToyOS, and "fds belong
only in libc jargon" (owner, 2026-08-19). The `rust/` fork's
`sdk/std/os/io.rs` re-exports `std::os::fd`, so
`std::os::toyos::io::{AsRawFd, FromRawFd}` still speak POSIX.
`tests/toyos-rust-tests/src/bin/std_fs.rs` is their one caller in this
repository.

**Exit**: the next time the trait is touched in the fork, `AsRawFd` and
`FromRawFd` in `std::os::toyos::io` are renamed to `os::toyos`'s own word for
a raw handle, and `std_fs.rs` uses the new names (owner, 2026-09-30).
