---
status: open
kind: defect
opened: 2026-10-01
---

# libc answers EINVAL for every error it does not name

Read from the code, not run. `set_errno` (`userland/libc/src/posix_io.rs`)
sends `Unknown`, `BadAddress`, `ResourceExhausted` and `NotSupported` through
`_ => EINVAL`, so each reaches a C caller as `EINVAL`, which POSIX spells
"invalid argument". A full handle table (`kernel/src/object/handle.rs`)
answers `dup`, `open` and `pipe` `EINVAL` where POSIX has `EMFILE`, a full
bcachefs volume (`kernel/src/bcachefs_adapter.rs`) `write` `EINVAL` where it
has `ENOSPC`, and an operation a filesystem lacks `EINVAL` where it has
`ENOTSUP`. A refusal that is truly `EINVAL` reads the same as each of them, so
a case asserting `EINVAL` passes whichever happened.

**Exit**: `set_errno` matches every `SyscallError` by name, with no catch-all,
each call that can meet `ResourceExhausted` answering the POSIX errno its own
page names for it, and a host test holds each call's mapping.
