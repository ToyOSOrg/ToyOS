---
status: open
kind: defect
opened: 2026-08-08
---

# `SYS_FTRUNCATE` takes no VFS lock and every flusher does

Filed out of the check-and-act entry when that closed; one of its three
residuals, and the one still live. Re-derived against the write-back queue on
2026-08-24, at `739af0c2`.

`kernel/src/arch/syscall.rs:606` routes `ftruncate` to `ops::ftruncate`
(`kernel/src/object/ops.rs:720`, formerly `fd::ftruncate` in the deleted
`kernel/src/fd.rs`), which calls `file_cache::resize` under **no** VFS
acquisition — only the `FileObject`'s own `with`. Every flusher takes the VFS
lock: `fsync` (`object/ops.rs:670`) and, since the write-back queue landed,
`writeback::drain_one` (`kernel/src/writeback.rs:247`) as well.

The fabricated-zeros write is closed — `flush_file` skips a page
`copy_page_out` says is gone (`kernel/src/vfs.rs:574`). The window is not:
`flush_taken`'s `file_cache::size(file_id)` (`vfs.rs:580`) and
`fs.update_metadata(file_id, size, mtime)` (`:581`) are two steps, so a truncate
landing between them records the **older** size.

**What the queue changed is who can be in that window.** Until 2026-08-23 the
only flusher was `SYS_FSYNC`, on a thread of the calling process. Now `iod`
flushes on a kernel thread of its own, on a file whose last handle has already
closed, with no caller to notice — so the race is between a live process's
truncate and a thread that runs on nobody's behalf, and it widens with the
queue's depth rather than with how often userland calls `fsync`.

It still self-corrects: `file_cache::resize` sets the file's `dirty_meta`
(`kernel/src/file_cache.rs:498-504`), so a later flush records the right size.
That is a property of the next flush, not an invariant of this one, and `iod`'s
drain pops its entry — so "there is a next flush" is a claim about the queue
rather than about this code.

`set_size` still exists beside `resize` (`file_cache.rs:490`) and is the
*establishing* form a mount uses to state the size a file already has on disk;
it is deliberately not what a user truncate calls, and the two are not
interchangeable in a fix.

The other two residuals of that entry are not open: `file_cache::read_page` is
fallible now, and the pipe-count direction was declined on the entry's own
reasoning that it buys a named operation and not an unwritable one.
