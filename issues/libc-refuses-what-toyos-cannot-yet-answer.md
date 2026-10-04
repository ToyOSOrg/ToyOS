---
status: open
kind: defect
opened: 2026-10-01
---

# libc refuses what ToyOS cannot yet answer

These answer failure in their POSIX form and do nothing
(`userland/libc/src/refused.rs`, `posix_io.rs`), each asserted by
`tests/testcases/tinycc/206_libc_refusals.c`. Each waits on what it names:

- `link`, `ENOSYS`: no call gives a file a second name.
- `symlink`, `ENOSYS`: `SYS_SYMLINK` displaces what its name held
  (`issues/a-symbolic-link-on-tmp-displaces-its-name-and-lists-nowhere.md`).
- `chmod` and `fchmod`, `ENOSYS`: a file has no mode bits to set.
- `statvfs` and `fstatvfs`, `ENOSYS`: no call answers a filesystem's size or
  free space.
- `getrlimit` and `setrlimit`, `ENOSYS`: no call answers a process's limits.
  LLVM's `getDefaultStackSize` (`llvm/lib/Support/ProgramStack.cpp`) ignores
  the failure and reads the `rlimit` it did not get.
- `gethostname`, `ENOSYS`: no host name is published to a process.
- `realpath`, `ENOSYS`: the kernel resolves a path by rules of its own (`..`
  read off the text, only the last name's link followed, a relative link read
  against its mount), and no call answers where a path leads.
- `msync` and `mprotect`, `ENOSYS`: no call answers which pages of a range are
  mapped, and a mapping's protection is fixed when it is made.
- `mmap` of a file, `ENODEV`, and of executable memory, `ENOTSUP`: the kernel
  maps neither.
- `madvise`'s `MADV_DONTNEED`, `ENOSYS`: no call discards a range's pages and
  keeps it mapped, so it cannot read back as zeros as Linux's does.
- `fcntl`: a record lock, `EINVAL`, POSIX's answer for a file that supports no
  locking; `F_DUPFD_CLOEXEC`, `F_GETFL`, `F_SETFL`, `F_GETOWN` and `F_SETOWN`,
  `ENOSYS`. `F_DUPFD` answers a duplicate at or above its argument, the first
  the kernel hands `dup`, where POSIX has the lowest free number, and refuses
  `EINVAL` an argument at or above the slots a handle table has
  (`RawHandle::MAX_SLOTS`). `F_GETFD` and `F_SETFD` of a descriptor the
  process does not hold answer 0, where POSIX has `EBADF`: the kernel ends a
  process that names a handle it does not hold, so libc asks it nothing.

Ruled out, and owed nothing while the ruling stands: `execv` and `execve`, no
call replacing a process's image; `setsid` and `getsid`, no POSIX session;
`fchown`, no owner; `getpwnam_r` and `getpwuid_r`, no user database.

**Exit**: each above either does what POSIX says, asserted by a guest C case
that reads its effect back, or is ruled out by the owner and moved to the list
above.
