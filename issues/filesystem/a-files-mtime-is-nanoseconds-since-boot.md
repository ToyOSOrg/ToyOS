---
status: open
kind: defect
opened: 2026-09-28
---

# A file's mtime is nanoseconds since boot, so no build tool can compare two

`toyos_abi::syscall::Stat::mtime` is documented as nanoseconds since boot, and
the kernel stamps it from `clock::nanos_since_boot` (`kernel/src/object/ops.rs`
at every open-to-create, write and `ftruncate`; `kernel/src/vfs.rs`'s flush of
a file a device view is taken over). DATA's bcachefs stores that number, so a
file written late in one boot reads as newer than one written early in the
next, and a build tool that decides a rebuild by comparing mtimes — ninja,
make — is misled by every reboot.

The units are not even one epoch across mounts. The FAT adapter answers
`file_mtime` in local Unix *seconds* off the directory entry while a handle
that writes carries since-boot nanoseconds; std's `Metadata::modified` reads every
one of them as nanoseconds since `UNIX_EPOCH`, and libc's `fstat` puts the
nanoseconds into `st_mtime`, which POSIX defines as seconds.

**Exit condition.** An mtime is nanoseconds since the Unix epoch in UTC, taken
off the wall clock at the write on every mount, stored to the precision the
filesystem keeps, and unchanged across a reboot — judged against the instant
the host stages in the RTC with `-rtc base=`.
