---
status: open
kind: defect
opened: 2026-09-27
---

# A C program names no file a file server holds

`/apps`, `/config`, `/home`, `/state`, `/log` and `/boot` are served by
`/system/bin/fsd` through directory capabilities in each program's namespace,
and std reaches them through `toyos::fs` (`rust/library/std/src/sys/fs/toyos.rs`).
`userland/libc` does not: `open`, `stat`, `opendir` and every other path call
in `userland/libc/src/posix_io.rs` and `userland/libc/src/stdio.rs` go to the
kernel's `SYS_OPEN` family, and the kernel serves ROOT and `/tmp` only. So a C
program — tinycc, doomgeneric, anything built with `toyos-cc` — is refused
every path under those six directories with `ENOENT`, and a file it writes
where a user's files live cannot be written at all.

**Exit**: libc resolves a path under a directory capability through
`toyos::fs` as std does — open, read, write, seek, stat, readdir, mkdir,
unlink, rename, fsync — with a C test in the corpus that writes a file under
`/home`, reads it back, and lists it.
