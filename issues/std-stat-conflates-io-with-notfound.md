---
status: open
kind: defect
opened: 2026-08-08
---

# `std::sys::fs::toyos::stat` re-creates the conflation the kernel just removed

Filed out of the `vfs::FileSystem` error-channel entry when that closed.

`sdk/std/sys/fs.rs`'s `stat` discards `syscall::open`'s error
and returns a hardcoded `io::ErrorKind::NotFound`, so `fs::metadata` on a volume
that would not answer reports "no such file" — the exact conflation the kernel
half of that task removed. `File::open`, `fs::read` and `fs::read_dir` propagate
correctly; it is `stat`/`lstat` alone.

Three lines in that file.
