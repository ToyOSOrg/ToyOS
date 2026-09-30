---
status: open
kind: defect
opened: 2026-10-01
---

# A symbolic link on /tmp displaces its name and lists nowhere

Read from the code, not run. The kernel's `/tmp` (`kernel/src/tmpfs.rs`)
makes a link over whatever file its name held, where POSIX's `symlink` refuses
a name that exists `EEXIST`; libc asks first (`userland/libc/src/posix_io.rs`,
`symlink`), so a file another process makes between the question and the call
is displaced. Its `list` answers files alone, so no `readdir` of a directory
shows a link it holds, and
`tests/testcases/tinycc/207_libc_names.c` lists a directory no link is in.

**Exit**: `SYS_SYMLINK` over a name that exists is refused `AlreadyExists`,
and a listing of `/tmp` names each link it holds, each asserted by a guest
case.
