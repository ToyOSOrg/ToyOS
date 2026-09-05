---
status: open
kind: defect
opened: 2026-09-05
---

# A `/home` delete the device refuses has already discarded the file it did not delete

`BcacheFsAdapter::delete` (`kernel/src/bcachefs_adapter.rs:299`) destroys every
piece of in-memory state the file has *before* it asks the device:

    file_cache::mark_deleted(file_id)   // sets `deleted`, kernel/src/file_cache.rs:631
    self.name_to_id.remove(name)
    self.revoke(name)                   // the shared `FileBlocks` cell -> None
    mapped("delete", name, self.fs.delete(name))?   // :307 — the `?` is the loss

The device call is the only step that can fail, and it is last. A read the
btree descent makes can be refused on the caller's own budget —
`FsError::DeviceRead` carrying `DeviceError::Refused`, which
`as_device_refusal` (`:72`) maps to `SyscallError::WouldBlock` — so `SYS_UNLINK`
returns an error meaning *nothing happened*, on a call after which four things
have happened:

- the cached file is marked `deleted`, and `writeback.rs:111` skips a deleted
  file's flush, so every page a writer dirtied and no flush has carried is
  dropped when `finish_writeback` frees it;
- the blocks cell is revoked, so every backing for that name fails from here on;
- the name is unbound, so the next `open` reads the device;
- and the on-disk entry is still there when the refusal came before
  `btree::delete` (`bcachefs/src/fs.rs:854`'s `delete_by_name` removes the entry
  and only then frees the extents at `:865`).

So the re-open serves the file's *pre-delete* contents and the unflushed writes
are gone, out of a syscall that said it did nothing.

## Reproduction

Not run. The path is read off the code above; no arm in the tree stages a
refusal on this call. The nearest instrument is the `fsync-budget-spent`
actuator, which stages a spent budget for `SYS_FSYNC` and not for a delete, so
staging this needs an actuator of its own on `BcacheFsAdapter::delete`'s device
call.

## Not the retry record

`issues/kernel/ftruncate-answers-wouldblock-and-nothing-retries-it.md` names the
same trigger and a different mechanism: it is about a refusal reaching userland
as `WouldBlock` with no retry ladder behind it, and states of its own subject
that a refused resize "changes nothing", which is what makes it a spurious
failure rather than a loss. This one is a loss, and a retry ladder alone would
not close it — the ordering would still discard the file on the attempt that
gave up.

## Exit condition

`delete` asks the device first and touches the cache, the name and the blocks
only on its success — or the refusal path restores all four — with an actuator
staging the refusal and an arm that re-opens the name and finds the file it had
before, its unflushed writes included.
