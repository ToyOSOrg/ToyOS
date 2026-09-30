---
status: open
kind: defect
opened: 2026-09-30
---

# libc's readdir calls every entry a regular file

`readdir` (`userland/libc/src/posix_io.rs`) answers `d_type` `DT_REG` for every
entry, a directory's too. libc ships no `dirent.h` yet
(`issues/build/libc-headers-are-written-by-hand-and-drift-from-its-definitions.md`);
once one defines `DTTOIF`, LLVM takes an entry's type from `d_type`
(`llvm/lib/Support/Unix/Path.inc`, `direntType`) and reads a directory as a
file.

**Exit**: `readdir` answers each entry's type, or `DT_UNKNOWN` where it does
not know it, and a guest C case reads a directory holding a file and a
directory.
