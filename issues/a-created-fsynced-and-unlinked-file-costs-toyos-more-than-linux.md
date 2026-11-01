---
status: open
kind: defect
opened: 2026-10-28
---

# A created, fsynced and unlinked file costs ToyOS more than Linux

`persist.create_fsync_unlink` creates a file on the persistent filesystem,
writes 4 KiB to it, `sync_all`s it, closes and removes it. At PR #1097's
landing head (069cbbeb8, 2026-10-28, one ToyOS boot and two Linux runs per
bench, DATA and Linux's home on the same NVMe) it read:

| bench | ToyOS | Linux |
|---|---|---|
| T14 | 1,168,401 ns | 953,976 and 966,415 ns |
| AMD | 3,533,628 ns | 3,257,728 and 3,283,416 ns |

That is 8 to 22% dearer, where the plain durable writes
(`write_4k_fsync`, `append_4k_fsync`) are at or near Linux's on both
benches. What the difference is made of is unmeasured. The create and the
unlink are file server requests, and the fsync commits the whole volume
with its superblock.

**Exit**: the row within Linux's on both benches, or its difference
measured to its parts and justified, in a labelled current-state
comparison.

**Owner**: the file server's DATA volume (`system/fileserver/src/data.rs`)
and the DATA format (`lib/bcachefs/src/fs.rs`).
