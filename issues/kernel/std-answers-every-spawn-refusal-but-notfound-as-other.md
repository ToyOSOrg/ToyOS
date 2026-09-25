---
status: open
kind: defect
opened: 2026-09-25
---

# std answers every spawn refusal but `NotFound` as `Other`

The std fork's direct spawn (`rust/library/std/src/sys/process/toyos.rs`, the
`spawned.map_err` in `Command::spawn`, at fork `3f0bda148507`) maps
`SyscallError::NotFound` to `io::ErrorKind::NotFound` and every other
`SyscallError` to `io::ErrorKind::Other`, and keeps no raw code. So a
`Command::current_dir` the kernel refuses as not absolute — `InvalidArgument`
from `SYS_SPAWN` — reaches the caller as `Other`, the same word as a refused
endowment, an exhausted table or a bad ELF, and `{e}` prints `other error`.
`issues/kernel/a-spawn-of-echo-was-refused-with-an-error-nothing-names.md` is
a failure this mapping already made unreadable.

`tests/toyos-rust-tests/src/bin/spawn_cwd.rs` checks the relative and empty
refusals by name only on its raw `SYS_SPAWN` arm for this reason.

**Exit**: every `SyscallError` `SYS_SPAWN` can answer has its own
`io::ErrorKind` (or a raw code `raw_os_error` returns), and a `Command` whose
relative or empty directory reaches the kernel is asserted refused as
`InvalidInput` by name.
