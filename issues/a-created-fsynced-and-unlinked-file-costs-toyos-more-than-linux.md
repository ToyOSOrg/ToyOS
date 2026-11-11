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

On 2026-11-11 at ad5db5a65 ([evidence](https://github.com/ToyOSOrg/evidence/tree/d44ebdae1f9f584d7b826e1498dbfd791a8d2339), four boots against five Linux
runs) the AMD bench read the row ahead: 2.83, 3.01, 2.86 and 2.54 ms against
Linux's 3.01 to 3.37 ms. The T14 read 6.87 to 7.41 ms against 0.86 to
0.97 ms, with every durable row of those four boots in the slow mode of
`issues/toyos-durable-writes-on-the-t14s-nvme-run-six-times-slower-in-some-boots.md`,
so the row's own difference there is not separated.

**Exit**: the row within Linux's on both benches, or its difference
measured to its parts and justified, in a labelled current-state
comparison.

**Owner**: the file server's DATA volume (`system/fileserver/src/data.rs`)
and the DATA format (`lib/bcachefs/src/fs.rs`).
