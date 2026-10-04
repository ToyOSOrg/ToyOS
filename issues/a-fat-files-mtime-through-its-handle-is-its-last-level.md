---
status: open
kind: defect
opened: 2026-09-29
---

# A FAT file's mtime through an open handle is its entry's last level, not its last write

fsd's FAT volume answers a file's mtime off its directory entry
(`userland/fsd/src/fat.rs`, `node_meta` and `lstat`), and an entry is stamped
only when it is brought level: at the last close, an fsync or a sync. A write
moves nothing an `fstat` reads, so a writer that writes and `fstat`s reads the
stamp of the entry's last level, and the same file closed and opened again reads
the level that close made: one file, two mtimes.

**Mechanism read off the code; not reproduced.**

**Exit condition.** For a FAT file, the mtime `fstat` answers through a handle
after a write is the one it answers after the file is closed and opened again,
checked by a test that writes, `fstat`s, reopens and `fstat`s again.
