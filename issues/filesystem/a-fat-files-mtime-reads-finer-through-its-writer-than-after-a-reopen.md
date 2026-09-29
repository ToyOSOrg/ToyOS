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

**Exit condition.** A handle holds the mtime its mount stores — rounded to the
mount's precision at the write — or the kernel mounts no FAT volume a process
writes.
