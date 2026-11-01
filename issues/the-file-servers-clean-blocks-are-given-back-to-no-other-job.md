---
status: open
kind: defect
opened: 2026-10-29
---

# The file servers' clean blocks are given back to no other job

Each file server's block cache (`system/fileserver/src/cache.rs`) keeps its
volume's structures clean up to the limit its server gives it, 16 MiB
(`system/fileserver/src/main.rs`, `CLEAN_BLOCKS`). A file's pages are its
paged object's since memory stage 7a, which holds them within the pager's
fixed capacity
(`issues/a-pager-holds-a-fixed-capacity-until-the-memory-service-reclaims-its-clean-pages.md`).
Nothing drops a clean block for another job's sake: a block leaves only to
make room for another in the same cache, or when the server ends.

A clean block the allocator refuses, or whose place in the eviction order it
refuses, is not kept: a read is still answered, and a flushed block is already
on the disk. These allocations still have no refusal path, so a server whose
job is out of memory can still abort at them:
- the block map's node, a small one every few inserts of a clean or a dirty
  block;
- a block's share count, a small one per block kept;
- a dirty block;
- a fetch's staging buffer;
- a flush's list of runs, the writes it keeps in flight, and each run's list
  of shares.

**Exit**: the file servers drop clean blocks when the memory service reports
pressure.

**Owner**: the file server (`system/fileserver/`) and the memory track
(`issues/anonymous-memory-swaps-to-an-encrypted-partition.md`'s memory
service).
