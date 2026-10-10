---
status: open
kind: defect
opened: 2026-10-09
---

# A crash leaks the blocks taken since the last commit

The DATA volume commits by shadow paging (`bcachefs/src/fs.rs`, the read-write
block's header): no node the last commit's tree reaches is written over, and
the allocator's bitmap is written in place. So a server killed or a machine
stopped between two commits leaves every block taken since the last one marked
used in the bitmap and named by no tree, and the blocks only the older tree
reached, which a commit that landed was about to free, the same. A block whose
bit the device refused to clear when it was given up (`BitmapAllocator::give`,
`succeed`, `fail`) stays marked used the same way, with nothing said. Nothing
finds them again: a read-write mount counts the bitmap as the device holds it
(`BitmapAllocator::count_free`), so the volume is smaller by every such block.

Measured with `bcachefs/tests/crash.rs`'s run (a 512-block volume, a
directory of 62 names renamed, 40 one-block files written, then a commit)
stopped after every write before the first superblock write: the volume mounts
as it was, with 284 blocks marked used where it had 190.

## Exit condition

A mount after any stop, and a volume after any refused write, holds a bitmap
whose used blocks are exactly those the committed tree reaches and the
superblock and bitmap's own, measured by `bcachefs/tests/crash.rs`'s stops and
refusals.
