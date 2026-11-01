---
status: open
kind: defect
opened: 2026-10-01
---

# libc refuses what ToyOS cannot yet answer

These answer failure in their POSIX form and do nothing
(`sdk/libc/src/refused.rs`, `posix_io.rs`), each asserted by
`tests/corpus/tinycc/206_libc_refusals.c`. Each waits on what it names:

- `symlink` of a link the kernel would hold, `ENOSYS`: `SYS_SYMLINK`
  displaces what its name held
  (`issues/a-symbolic-link-on-tmp-displaces-its-name-and-lists-nowhere.md`). A
  file server makes one and refuses a name that exists, as POSIX has it.
- `chmod`, `fchmod` and `fchmodat`, `ENOSYS`: a file has no mode bits to set.
- `statvfs` and `fstatvfs`, `ENOSYS`: no call answers a filesystem's size or
  free space.
- `getrlimit` and `setrlimit`, `ENOSYS`: no call answers a process's limits.
  LLVM's `getDefaultStackSize` (`llvm/lib/Support/ProgramStack.cpp`) ignores
  the failure and reads the `rlimit` it did not get.
- `gethostname`, `ENOSYS`: no host name is published to a process.
- `msync`, `ENOSYS`: no mapping is of a file, so there is nothing to write back.
- `mmap` of a file, `ENODEV`, and `mmap` or `mprotect` of executable memory,
  `ENOTSUP`: the kernel maps neither.
- `fcntl` (`sdk/libc/src/fdreq.rs`): a record lock, `EINVAL`, POSIX's answer
  for a file that supports no locking; `F_GETFL`, `F_SETFL`, `F_GETOWN` and
  `F_SETOWN`, `ENOSYS`.

Ruled out, and owed nothing while the ruling stands: `execv` and `execve`, no
call replacing a process's image; `setsid` and `getsid`, no POSIX session;
`fchown`, no owner; `getpwnam_r` and `getpwuid_r`, no user database.

**Exit**: each above either does what POSIX says, asserted by a guest C case
that reads its effect back, or is ruled out by the owner and moved to the list
above.
