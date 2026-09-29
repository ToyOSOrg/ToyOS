---
status: open
kind: defect
opened: 2026-09-28
---

# The last handle to close stamps the file with its own mtime, not the last write's

An mtime lives on the handle (`kernel/src/object/file.rs`'s
`OpenFileState::mtime`): it is the file's stored stamp at the open, and a write
or `ftruncate` through that handle moves it (`kernel/src/object/ops.rs`). A
close that leaves another handle open enqueues nothing
(`file_cache::release_to_writeback` answers `StillHeld`), and the last close
enqueues the flush with *its* handle's mtime (`kernel/src/writeback.rs`).

So a file opened for reading at `t0`, written through a second handle at `t1`,
closed by the writer and then by the reader, is flushed with `t0`: the stored
mtime says the file has not changed since before the write, and a build tool
that compares mtimes does not rebuild from it.

**Mechanism read off the code; not reproduced.**

**Exit condition.** The mtime is the file's and not a handle's — each write
moves the one stamp every flush of the file stores — with a guest test that
writes through one handle, closes a reader last, and reads the write's stamp
back after a reopen.
