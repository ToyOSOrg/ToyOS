---
status: open
kind: defect
opened: 2026-09-28
---

# std calls undated what it did not stat, and sets no mtime it is asked to

The fork's `library/std/src/sys/fs/toyos.rs` reads a file's mtime only off
`SYS_FSTAT`. `DirEntry::metadata` answers `mtime: 0` without asking, so
`Metadata::modified` reports a file the kernel has a stamp for as undated: a
program walking a tree through `read_dir` and comparing
`entry.metadata()?.modified()?` fails on every entry.

And `File::set_times`, `fs::set_times` and `set_times_nofollow` return `Ok(())`
having set nothing, so a tool that stamps an output (`touch`, a build system's
restat) is told it did.

**Exit condition.** `DirEntry::metadata` answers the mtime the kernel keeps
for that name, and a time-setting call sets the stamp through the kernel or is
refused as `Unsupported`.
