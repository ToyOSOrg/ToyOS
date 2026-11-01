---
status: open
kind: defect
opened: 2026-10-28
---

# A first format interrupted mid-flush can leave a superblock over a bitmap it never wrote

`bcachefs::Formatted::format` writes the allocation bitmap, the root node and
then the superblock, but through the DATA file server's cache
(`system/fileserver/src/cache.rs`), which writes every dirty block at a
flush lowest first and in runs that may be in flight together, unordered.
Block 0 is the lowest, so the superblock can reach the medium before the
bitmap does. A machine that stops between the two has, in place of the
designation stamp, a superblock whose bitmap blocks hold whatever the
partition held before — and `BitmapAllocator::open` reads them as the
allocation state. The designation a bench runs from inside ToyOS
(`bench_data_designated`) formats 170 GiB on its next boot, about 1,361
bitmap blocks, so the window is that one flush.

Found by inspection in the review of the partitions work; no boot has
shown it.

**Owner:** the DATA file server and the bcachefs crate.

**Exit condition:** a format reaches the medium superblock-last — every
other block written and made durable before block 0 is written — and a host
test cuts the format's writes at every point against a stamped partition and
finds either the stamp or a whole volume at block 0.
