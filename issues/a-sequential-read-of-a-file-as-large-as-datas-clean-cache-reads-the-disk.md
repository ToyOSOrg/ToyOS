---
status: open
kind: defect
opened: 2026-10-28
---

# A sequential read of a file as large as DATA's clean cache reads the disk

The file server's cache (`system/fileserver/src/cache.rs`) keeps clean
blocks up to the limit the server gives it, and evicts them in the order
they arrived. A hit does not move a block. So once a file and the volume's
metadata together pass that limit, a sequential read evicts, at each miss,
the block it is about to read next, and every pass misses every block.
The limit was a fixed 16,384 blocks (64 MiB) when this was measured; it is
now most of an eighth of physical memory for DATA (#1099), so the 64 MiB
file below fits, and a file larger than that limit falls off the same cliff.

Measured on the AMD bench (2026-10-28, `osbench persist` at PR #1097's
c8f3fbfb0, DATA on its NVMe): `fs.disk.seq_read.4096`, a 64 MiB file read in
4 KiB calls, p50 158 MB/s, min 158 and max 426, against 4,960 and 5,054 MB/s
under the bench's Linux. Four of the five rounds ran at about a third of the
fifth.

The cause, shown on the host by a probe of the cache over a counting disk
(PR #1097's evidence, `probe.log`): blocks written in 4 KiB calls, flushed,
then read in order five times. With 16 blocks over the limit (16,400), each
pass made 16,400 disk requests. With 16 blocks under it (16,368), each pass
made none.

**Exit**: a sequential read of a file larger than the clean limit keeps
hitting what the cache can hold, rather than missing everything. A host test
of the cache shows it, by counting the disk reads of repeated passes over
16 blocks more than the cache's limit.

**Owner**: the file server's cache (`system/fileserver/src/cache.rs`).
