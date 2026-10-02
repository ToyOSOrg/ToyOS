---
status: open
kind: defect
opened: 2026-10-01
---

# libc's stat answers one serial number for every file

`stat`, `fstat` and `lstat` (`userland/libc/src/posix_io.rs`) zero the whole
`struct stat` and fill in the size, the mtime and the mode, so `st_dev` and
`st_ino` are 0 for every file. POSIX names a file by that pair, so every file
is one file to a program that asks: LLVM's `sys::fs::equivalent` and
`UniqueID` (`llvm/lib/Support/Unix/Path.inc`) answer any two paths the same,
and clang's `FileManager` keys every file it opens on the pair. `readdir`'s
`d_ino` is the entry's offset in the listing, which no `st_ino` equals.

**Exit**: `st_dev` and `st_ino` tell two files apart and name one file the
same way through two paths, `d_ino` is its entry's `st_ino`, and a guest C case
reads them for two files and a link.
