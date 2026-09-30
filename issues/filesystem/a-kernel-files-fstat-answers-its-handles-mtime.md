---
status: open
kind: defect
opened: 2026-09-29
---

# A kernel file's `fstat` answers its handle's mtime, not the file's

`ops::fstat` answers `OpenFileState::mtime` (`kernel/src/object/ops.rs`), which
is the file's stamp at the open, moved only by a write or `ftruncate` through
that handle. A write through another handle moves the file's own stamp
(`file_cache::touch`) and not this one. So on `/tmp` a file opened at `t0` and
written through a second handle at `t1` answers `t0` through the first and `t1`
after a reopen, while the size it answers beside it is the file's.

**Mechanism read off the code; not reproduced.**

**Exit condition.** `fstat` of a kernel file answers the stamp the file holds,
with a guest test that writes through one handle and `fstat`s another opened
before the write.
