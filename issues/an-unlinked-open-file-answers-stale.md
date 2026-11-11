---
status: open
kind: track
opened: 2026-10-22
---

# An unlinked open file answers Stale

**Ruled** (owner, 2026-10-21):
removing the last directory name prevents fresh opens through that name, and
existing handles and mappings keep the same file until their final release. A
new file made at the old path is a different file and never retargets them.
The file server owns the lifetime and the storage's reclaim; the kernel keeps
no filesystem identity or naming.

Built for DATA and MEMORY: a file unlinked or renamed over while open is read
and written through its descriptor until the last close, and its blocks are
freed then (`unlink_and_replacement_keep_live_inode_identity` in
`system/fileserver/src/data.rs`, `unlink_and_replacement_keep_open_inode_identity`
in `memory.rs`, `persistent_unlink_preserves_dirty_bytes_until_the_last_holder`
in `pages/tests.rs`); a crash leaves the orphans for the next mount to release
(`namespace_operations_survive_a_crash_at_every_request` in
`lib/bcachefs/src/engine/tests/crash.rs`).

What is left:

- A FAT volume still answers `Stale`: its `unlink` marks the open file gone
  (`system/fileserver/src/fat.rs`, `orphan`).
- No guest test reads a mapping of an unlinked file, which waits for file
  mappings
  (`issues/process-memory-in-4-kib-pages-still-copies-each-programs-text.md`).

**Exit**: the guest test of a mapping, and FAT keeping an unlinked open file.
