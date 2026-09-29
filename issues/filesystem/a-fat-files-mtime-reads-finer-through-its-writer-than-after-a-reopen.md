---
status: open
kind: defect
opened: 2026-09-28
---

# A FAT file's mtime reads finer through its writer than after a reopen

A handle's `fstat` answers the mtime the handle holds (`kernel/src/object/ops.rs`),
which a write sets to `clock::mtime_now` in nanoseconds. A FAT volume stores a
write time in two-second units (`kernel/src/fat32_adapter.rs`'s
`stamp`), so the same file opened again answers the even second: one file,
two mtimes.

**Exit condition.** For a FAT file, the mtime `fstat` answers through a handle
equals the one it answers after the file is closed and opened again, checked by
a test that writes, `fstat`s, reopens and `fstat`s again; or the kernel mounts
no FAT volume a process writes.

**Untested.** `fat32_adapter.rs`'s flush stamps its own instant (`now()`), not
the flushing handle's `_mtime`, because the last handle to close may be a reader
that opened before the last write. No test closes a reader last and asserts the
stored time is the flush's: it waits on `epoch()` moving two seconds, never a
sleep.
