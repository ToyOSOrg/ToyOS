---
status: open
kind: defect
opened: 2026-08-28
---

# A device refusal in `fsync`'s sync half leaves the file marked clean, so the next `fsync` returns success without touching the device

`SYS_FSYNC` promises two things (`kernel/src/object/ops.rs:488-490`): the file's
bytes on the device, and the device told to commit them. Its outer gate is one
per-file bit:

```
kernel/src/object/ops.rs:498    if !file_cache::dirty_meta(file_id) { return 0; }
```

The device-commit half is owed by the *mount*, not by the file, and nothing
records that it is still owed. So an `fsync` that failed on that half leaves the
file looking clean, and the next `fsync` on it answers success without issuing
anything.

The chain, on current main:

- `kernel/src/object/ops.rs:518-520` — one attempt is
  `vfs.flush_file(...).and_then(|()| vfs.sync_for_path(...))`: the file's pages
  first, then the mount's durability call.
- `kernel/src/vfs.rs:342` — `flush_file` opens with `file_cache::take_dirty`,
  which clears `dirty_meta` (`kernel/src/file_cache.rs:336-343`) before any work
  is attempted.
- `kernel/src/vfs.rs:345-348` — the flag is restored only when `flush_taken`
  itself failed. `file_cache::mark_dirty_meta`
  (`kernel/src/file_cache.rs:350-355`) has exactly one call site in the kernel,
  and that is it.
- So when `flush_file` returns `Ok` and the chained `sync_for_path`
  (`kernel/src/vfs.rs:496-506`) fails, `ops.rs:555` returns the device's word
  with `dirty_meta` already `false`.
- The next `SYS_FSYNC` on that file hits `ops.rs:498` and returns `0` at
  `ops.rs:499` — no `flush_file`, no `sync_for_path`, and no log line, since the
  gate precedes every log in the function. `ops.rs:533`'s comment
  ("`flush_file` already cleared the file's own `dirty_meta`") is the statement
  of the clear that is never undone.

The `WouldBlock` arm is not the hole; that one is closed and measured. A budget
refusal returns through `flush_file`'s `Err` arm with the flag restored, and
`ops.rs:538-552` retries on a fresh budget — the invariant
`tests/common/volumes.rs:1911-1923` calls the fsyncgate failure mode and asserts
against. The hole is every *terminal* exit of that same loop: `ops.rs:555` on a
device error, and `ops.rs:550` when the deadman ends the run of retries.

Nothing else re-attempts it. `kernel/src/writeback.rs:104-134`'s `drain_one`
calls `vfs.flush_file` and never `sync_for_path`, and it is gated on the same
`dirty_meta` (`writeback.rs:111`), so a file cleared this way is skipped by the
queue too. Only `Vfs::sync_all` (`kernel/src/vfs.rs:509-520`) reaches the device
again, from the shutdown path at `kernel/src/arch/syscall/machine.rs:53-54`.

**Impact.** `fsync` returns success while the mount was never told to commit,
against the contract `kernel/src/vfs.rs:62` states for every `FileSystem::sync`
("must not swallow a lower-level failure and report success: the log depends on
this call telling the truth about durability"). On `/log` (FAT32 on USB,
`kernel/src/main.rs:438-440`) the bytes are on the stick but its write cache was
not committed — `toyos-fat32/src/fs.rs:901-910`'s `dev.flush()` is what failed,
and `/bin/logd`'s `LOG_DURABLE_NS` is published off this call
(`ops.rs:490`, `vfs.rs:495`). On `/home` (bcachefs on NVMe,
`kernel/src/main.rs:421`) it is worse: `write_page` goes straight to the device
(`kernel/src/bcachefs_adapter.rs:238-250` → `page_cache::raw_block_write`,
`kernel/src/page_cache.rs:66-70`), but the extents, the size and the superblock
travel through the page cache and reach the device only inside `Mounted::sync`
(`bcachefs/src/fs.rs:832-838`) — the call that failed. The second `fsync`
therefore reports durable for a file whose metadata is still only in RAM.

**Reproduction, in tree, no hardware fault.** Boot with `usb-flush-fails`
(`kernel/src/actuator.rs:89`), which answers SYNCHRONIZE CACHE with HARDWARE
ERROR (`kernel/src/drivers/xhci/wait/msc.rs:316-324`) → `BlockError::Device`
(`msc.rs:296-301`) → `IoError::Device` (`kernel/src/fat32_adapter.rs:261-264`,
`101-106`) → `SyscallError::Io` (`fat32_adapter.rs:505-509`). A guest that
writes a file on `/log` and calls `fsync` twice gets `Io`, then `0`; only the
first call reached the device. `fsync-budget-spent` plus `fsync-deadman-now`
(`kernel/src/actuator.rs:97-101`) stages the same end state through the deadman
exit; `tests/common/volumes.rs`'s `log_flush_retry` already boots both and
asserts the first call's verdict — nothing asserts the second call's.

**Also true, and the same shape one level down.** `PageCache::sync` clears a
run's dirty bits as soon as `dev.write_blocks` returns
(`kernel/src/page_cache.rs:279-282`), before the single `dev.flush()` at
`page_cache.rs:293-296`, and a failed flush leaves them clean with the debt
recorded nowhere. It does not currently lose a flush, because the one
durability caller — `bcachefs::Mounted::sync` — writes the superblock to block 0
and to the last block (`bcachefs/src/superblock.rs:211-216`) through `write_new`
(`page_cache.rs:227-244`) on every call, so `page_cache.rs:252-254`'s
empty-`pending` early return cannot be reached from it. That is a property of a
caller's habit, not an invariant of the cache, and it should not be what holds.

**Fix direction.** Make the commit debt survive its own failure instead of being
inferred from a per-file bit. The narrow form restores `dirty_meta` when the
`sync_for_path` half of `ops.rs:518-520` fails, so the next `fsync` redoes the
whole attempt. The honest form is a per-mount "commit owed" flag, set when a
`FileSystem::sync` returns an error and cleared only by one that returns `Ok`,
with `ops.rs:498` gating on `dirty_meta || commit_owed(mount)`; that also covers
the file whose pages the write-back queue flushed, since that path never syncs
at all. Either way the negative control is the second `fsync` under
`usb-flush-fails`: it must not return `0`.
