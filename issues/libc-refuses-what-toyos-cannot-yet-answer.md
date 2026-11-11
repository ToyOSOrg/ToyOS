---
status: open
kind: defect
opened: 2026-10-01
---

# libc refuses what ToyOS cannot yet answer

These answer failure in their POSIX form and do nothing
(`sdk/libc/src/refused.rs`, `posix_io.rs`), each asserted by
`tests/corpus/tinycc/206_libc_refusals.c`. Each waits on what it names:

- `chmod`, `fchmod` and `fchmodat`, `ENOSYS`: a file has no mode bits to set.
- `statvfs` and `fstatvfs`, `ENOSYS`: no call answers a filesystem's size or
  free space.
- `getrlimit` and `setrlimit`, `ENOSYS`: no call answers a process's limits.
  LLVM's `getDefaultStackSize` (`llvm/lib/Support/ProgramStack.cpp`) ignores
  the failure and reads the `rlimit` it did not get.
- `gethostname`, `ENOSYS`: no host name is published to a process.
- `msync`, `ENOSYS`: a file is mapped read-only only, so no mapping holds
  anything to write back; it answers once 7b's step 5 maps files writable.
- `mmap` of a file other than `PROT_READ`, `ENODEV`: the kernel maps a file's
  pages read-only and shared, and a private one shares them until it is written,
  which waits for 7b's write faults (step 5); a private copy range of a file
  (`PROT_WRITE` or `PROT_EXEC` with `MAP_PRIVATE`, 7b's R4) is not built either.
  A file whose server maps no pages answers `ENODEV` too.
- `fcntl` (`sdk/libc/src/fdreq.rs`): a record lock, `EINVAL`, POSIX's answer
  for a file that supports no locking; `F_GETFL`, `F_SETFL`, `F_GETOWN` and
  `F_SETOWN`, `ENOSYS`.

Ruled out, and owed nothing while the ruling stands: `execv` and `execve`, no
call replacing a process's image; `setsid` and `getsid`, no POSIX session;
`fchown`, no owner; `getpwnam_r` and `getpwuid_r`, no user database.

Refused by design, and owed nothing: `mmap` or `mprotect` of memory writable
and executable at once, `ENOTSUP`. A program writes its code and then
protects it read and execute, as the SDK's loader does; no page is both
(orchestrator, with the userland loader, which made read and execute a
protection a program may ask for).

**Exit**: each above either does what POSIX says, asserted by a guest C case
that reads its effect back, or is ruled out by the owner and moved to the list
above.
