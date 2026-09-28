---
status: open
kind: defect
opened: 2026-09-28
---

# std answers an mtime of 1970 for what it did not stat, and sets none it is asked to

The fork's `library/std/src/sys/fs/toyos.rs` reads a file's mtime only off
`SYS_FSTAT`. Everywhere else it invents one: `DirEntry::metadata`, `stat` of a
directory and `lstat` of a symlink all answer `mtime: 0`, which
`Metadata::modified` reports as `UNIX_EPOCH` — a real-looking instant, not an
error. A program walking a tree through `read_dir` and comparing
`entry.metadata()?.modified()?` sees every file as written in 1970.

And `File::set_times`, `fs::set_times` and `set_times_nofollow` return `Ok(())`
having set nothing, so a tool that stamps an output (`touch`, a build system's
restat) is told it did.

**Exit condition.** Each answers the mtime the kernel keeps for that name or
an error saying it has none, and a time-setting call sets the stamp through the
kernel or is refused as `Unsupported`.
