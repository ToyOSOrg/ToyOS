---
status: open
kind: defect
opened: 2026-10-29
---

# The file servers' clean blocks are given back to no other job

Each file server's block cache (`system/fileserver/src/cache.rs`) keeps clean
blocks up to the limit its server gives it (`system/fileserver/src/main.rs`,
`clean_limit`). Together the DATA, log and boot servers keep at most an
eighth of physical memory clean: 16 MiB each for log and boot, the rest of
the eighth for DATA. Reads fill it, and so do writes, since a flushed block
stays cached as clean. Nothing drops a clean block for another job's sake: a
block leaves only to make room for another in the same cache, or when the
server ends. So with every cache full, an ordinary job can be refused memory
while an eighth of the machine holds blocks that could be read again from
the disk.

The eighth is the bound netstack and the compositor take for want of a
kernel memory limit (`issues/no-physical-memory-fairness.md`); it is an
interim, not a sizing.

A clean block the allocator refuses, or whose place in the eviction order it
refuses, is not kept, and the read is still answered. Three allocations
still have no refusal path, so a server whose job is out of memory can still
abort at them:
- the block map's node, a small one every few inserts of a clean or a dirty
  block;
- a dirty block;
- a fetch's staging buffer.

**Exit**: the file servers drop clean blocks when the memory service reports
pressure, and `CLEAN_SHARE` is deleted.

**Owner**: the file server (`system/fileserver/`) and the memory track
(`issues/anonymous-memory-swaps-to-an-encrypted-partition.md`'s memory
service).
