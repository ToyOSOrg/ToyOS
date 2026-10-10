---
status: open
kind: defect
opened: 2026-10-01
---

# The kernel numbers no directory, symlink or listing entry

A file's identity is `toyos_abi::syscall::Stat`'s `volume` and `node`, which
libc answers as `st_dev` and `st_ino` and std as `MetadataExt::dev` and `ino`.
Every file the kernel opens, and every name a file server stats, has one. What
the kernel opens not has none: `SYS_OPEN` refuses a directory and a symlink, so
on `/system` and `/tmp` libc's `stat` of a directory and `lstat` of a symlink
answer `st_dev = st_ino = 0` (`userland/libc/src/posix_io.rs`), as std's do
(`sdk/std/sys/fs.rs`), and every such name is one file to a program that
asks. clang's `FileManager` keys directories on the pair too
(`getDirectoryRef`'s `UniqueRealDirs`), so every kernel directory is one
`DirectoryEntry` to it. `SYS_READDIR` answers no identity, so `readdir`'s
`d_ino` is the entry's offset in the listing, which no `st_ino` equals. Pipes,
consoles and the other kernel objects answer 0 as well.

**Exit**: `stat` of two kernel directories, and `lstat` of two kernel symlinks,
answer two nonzero pairs, `d_ino` is its entry's `st_ino`, and a guest C case
reads them for two directories and a link.
