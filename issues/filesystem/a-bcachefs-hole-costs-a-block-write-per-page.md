---
status: open
kind: defect
opened: 2026-09-02
---

# A hole on bcachefs costs one block write per page, because the extent list cannot say "hole"

`Mounted::resolve_or_alloc_block` (`bcachefs/src/fs.rs`) now zeroes every block
it allocates only to bridge a gap, because the allocator hands back the blocks a
shrink most recently freed and a hole that reads the previous tenant's bytes is
a data leak. That is correct and it is what FAT32 already does (`Fat32::set_len`
calls `zero_range` on every grow). The cost is linear in the hole: reaching page
N over a gap of M pages writes M zero blocks, one device write each, inside the
VFS lock a flush holds.

Nothing in the tree pays it today. Every writer this kernel has appends
sequentially, so `covered` reaches `target` in one block and the zeroing loop
does not run — `fs_large_file` writes 1024 pages and allocates one block per
page with no gap. The path that pays is a `lseek` past the end followed by a
write, which
`issues/kernel/lseek-past-eof-is-silently-clamped.md`
currently makes unreachable, and a shrink-then-write-above-the-mark, whose gap
is bounded by the file.

The reason it is linear is the representation: `block_for` walks the extent list
accumulating `block_count`, so page K's block is decided by how many blocks
precede it. A list like that has no way to say "these pages are not allocated" —
every page below the size must have a block. FAT32 has the same property and the
same cost.

**Exit condition.** An extent list that can express an unallocated run, so a
hole costs one list entry rather than one write per page, and a read of it
resolves to zeros without a block. That is an on-disk format change for
bcachefs, which is why it is not a small one — the same wall
`issues/filesystem/a-listing-still-scans-the-whole-mount.md` ends at.
