---
status: open
kind: defect
opened: 2026-10-28
---

# A sequential read of a file as large as DATA's clean cache reads the disk

**Since memory stage 7a** a file's pages are no longer in the block cache
(`system/fileserver/src/cache.rs`), which keeps the volumes' structures: they
are the file's paged object's, within the DATA pager's capacity, which is
what #1099 gave the block cache, most of an eighth of physical memory
(`system/fileserver/src/main.rs`, `pager`). So the 64 MiB file below fits.
When the capacity is full, the eviction drops a file nobody holds, then
another file's clean pages, then the read file's own outside the range asked
for, so a file larger than the capacity is read off the disk whole on every
pass.

Before 7a the block cache kept clean blocks up to the limit the server gave
it, and evicted them in the order they arrived. A hit did not move a block.
So once a file and the volume's metadata together passed that limit, a
sequential read evicted, at each miss, the block it was about to read next,
and every pass missed every block. The limit was a fixed 16,384 blocks
(64 MiB) when this was measured.

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

**Exit**: a sequential read of a file larger than the pager's capacity keeps
hitting what the capacity can hold, rather than reading everything again on
every pass: the file server's eviction over its objects
(`Files::make_room`), shown by a host test counting the volume reads of
repeated passes over a file of capacity plus 16 pages.

**Owner**: the file server's paged files (`system/fileserver/src/pages.rs`).
