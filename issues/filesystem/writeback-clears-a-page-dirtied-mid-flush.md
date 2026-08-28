---
status: open
kind: defect
opened: 2026-08-28
---

# `clear_dirty` clears every page the flush snapshotted, so a write that lands mid-flush is marked clean and silently never reaches the device

`Vfs::flush_taken` drops the FILE_CACHE lock across each page's device write, and then clears the dirty bit of every page it snapshotted at the start — including a page a concurrent writer re-dirtied inside that window. Those bytes are never written, never re-offered to a later flush, and eviction is then free to drop the page, so a read-back returns the pre-write contents.

## Mechanism

`Vfs::flush_file` (`kernel/src/vfs.rs:339-350`) calls `file_cache::take_dirty`
(`kernel/src/file_cache.rs:338-343`), which under one FILE_CACHE acquisition
clears `dirty_meta` (`:341`) and collects the indices of every page whose
`dirty` bit is set (`:342`). `flush_taken` then loops that snapshot
(`kernel/src/vfs.rs:367-372`) with **no** lock held over the pair:
`file_cache::copy_page_out` takes and releases FILE_CACHE inside itself
(`kernel/src/file_cache.rs:328-334`), and `fs.write_page`
(`kernel/src/vfs.rs:370`) is real block I/O —
`kernel/src/fat32_adapter.rs:702-715` and
`kernel/src/bcachefs_adapter.rs:238-250`. Only after the whole loop does it call
`file_cache::clear_dirty` once (`kernel/src/vfs.rs:373`).

`clear_dirty` (`kernel/src/file_cache.rs:358-367`) executes
`page.dirty = false` (`:363`) for every index in the set it was handed, with no
comparison against the page's current state. There is nothing to compare
against: `CachedPage` is `{ data, dirty, referenced }`
(`kernel/src/file_cache.rs:17-22`) and `CachedFile`
(`:24-36`) carries no generation counter, write epoch, or flush-in-progress
flag. Its own doc line (`:357`) claims a page dirtied while the lock was
dropped is protected; that holds only for a page that was *clean* at
`take_dirty` and is therefore absent from the set. A page already in the set and
written again afterwards is cleared regardless.

Nothing blocks that writer. `SYS_WRITE` takes no VFS lock:
`arch/syscall/io.rs:47-61` enters only the calling process's own
`Arc<Lock<ProcessData>>` (`kernel/src/process.rs:806-810`), reaches
`ops::try_write` (`kernel/src/object/ops.rs:375-407`) and
`file_cache::write_page` at `:387`, whose only acquisitions are FILE_CACHE's own
(`kernel/src/file_cache.rs:262`, `:289`). `apply_write`
(`kernel/src/file_cache.rs:305-323`) re-sets `page.dirty = true` (`:314`) and
`file.dirty_meta = true` (`:317`) under that independent acquisition. Meanwhile
the flusher holds the VFS lock and spins without yielding
(`kernel/src/sync.rs:55-79`), so it pins one CPU while the other seven run
userland — the two genuinely overlap.

## Impact

The write is lost on the device, permanently and without a single error return.
The next `take_dirty` filters on `p.dirty` (`kernel/src/file_cache.rs:342`), so
the page is never re-offered; that flush clears `dirty_meta` again and writes
metadata only, after which `ops::fsync` short-circuits at
`kernel/src/object/ops.rs:498-500`. In memory the bytes still look right — until
`evict_one`, which skips only dirty pages (`if page.dirty { continue; }`,
`kernel/src/file_cache.rs:510`), takes the now-clean page at `:517`; the next
read re-fetches the stale contents through the backing (`:222-228`). The write
reverts silently. `/bin/logd`'s durability claim rests on `SYS_FSYNC`
(`kernel/src/object/ops.rs:488-490`), and this is a way for that call to report
success over bytes that never left the page cache.

## Precondition

Two ordinary unprivileged processes with handles to one path on a writable
disk-backed mount. `/log` is FAT32 mounted `UserAccess::ReadWrite`
(`kernel/src/main.rs:440`) and `/home` is bcachefs `ReadWrite` when a volume is
present (`kernel/src/main.rs:421`). Two opens of one name share one `FileId` and
one `CachedFile`: `kernel/src/fat32_adapter.rs:617-621` and
`kernel/src/bcachefs_adapter.rs:148-157`. Process A writes page P and calls
`SYS_FSYNC`; process B writes page P — any offset in the same 4 KiB page —
between A's `copy_page_out` for P (`kernel/src/vfs.rs:369`) and A's single
`clear_dirty` (`:373`). With N dirty pages the first page's exposure is N device
writes wide. Needs `--smp` above 1; the drain path
(`kernel/src/writeback.rs:104-115`) can substitute for A when a re-open adopts a
queued file mid-drain, but the `SYS_FSYNC` pair needs no such timing.

`/tmp` and any tmpfs `/home` fallback are immune: `kernel/src/tmpfs.rs:76`
creates files non-evictable, so `is_cache()` is false
(`kernel/src/file_cache.rs:40-42`) and the sweep skips the file (`:538`), and
`kernel/src/tmpfs.rs:111-113` makes `write_page` a no-op because the cache is
the canonical store. `/boot` is `KernelOnly` (`kernel/src/main.rs:435`).

## Fix direction

Make the clear conditional on the page not having changed since it was copied.
The smallest shape: give `CachedPage` a monotonically increasing write counter
bumped in `apply_write` beside `page.dirty = true`
(`kernel/src/file_cache.rs:314`); have `copy_page_out` return the counter value
it copied, and have `clear_dirty` take `(index, counter)` pairs and clear
`dirty` only where the page's current counter still matches. A page written
during the window then keeps its dirty bit, stays un-evictable, and the next
flush delivers it — which is what the code already believes it does. `take_dirty`
alone cannot supply the counter, because the value that matters is the one read
at `copy_page_out`, not at snapshot time.

The alternative is per-file exclusion between a flush and writes to the pages it
is flushing, which costs a sleeping lock on the write path and is a larger
change than this defect justifies.

Per the high-risk rule this lands with two checks. The negative control: a
harness test running two processes against one `/log` file — one looping
`write()` + `fsync()`, the other looping `write()` at a distinct offset in the
same page — which reads the bytes back after forcing eviction and reds when the
whole fix is reverted onto the same base. The independent oracle: the file's
bytes read off the volume after shutdown by `toyos-fat32-check`, not by the same
kernel that wrote them.
