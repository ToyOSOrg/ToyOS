---
status: open
kind: defect
opened: 2026-10-22
---

# libc answers st_nlink 0

libc's `stat`, `lstat` and `fstat` (`sdk/libc/src/posix_io.rs`, `answered`)
zero the whole `struct stat` and never set `st_nlink`, so every file has no
link. Neither the kernel's `/tmp` nor the file server has hard links, so the
true count is 1.
SQLite checks every database it opens (`verifyDbFile` in `os_unix.c`): a
count of 0 is logged as a file unlinked while open, and one past 1 as a file
with several names.

**Exit**: a file and a directory answer `st_nlink` 1 on `/tmp`, `/system` and a
served directory, in a C corpus case.
