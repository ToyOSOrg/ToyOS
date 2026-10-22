---
status: open
kind: defect
opened: 2026-10-01
---

# libc answers EINVAL for every error it does not name

Read from the code, not run. `errcode::of` (`sdk/libc/src/errcode.rs`), which
`posix_io.rs`'s `set_errno` asks,
sends `Unknown`, `BadAddress`, `ResourceExhausted` and `NotSupported` through
`_ => EINVAL`, so each reaches a C caller as `EINVAL`, which POSIX spells
"invalid argument". libc's own descriptor table answers `EMFILE` when it is
full (`fd_errno`, `sdk/libc/src/posix_io.rs`), but a full kernel handle table
(`kernel/src/object/handle.rs`) refuses the `open` or `pipe` beneath it first
with `ResourceExhausted`, which reaches the caller as `EINVAL` where POSIX has
`EMFILE`; and an operation a filesystem lacks `EINVAL` where it has
`ENOTSUP`. A refusal that is truly `EINVAL` reads the same as each of them, so
a case asserting `EINVAL` passes whichever happened.

**Exit**: `errcode::of` matches every `SyscallError` by name, with no catch-all,
each call that can meet `ResourceExhausted` answering the POSIX errno its own
page names for it, and a host test holds each call's mapping.
