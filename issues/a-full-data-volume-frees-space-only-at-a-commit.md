---
status: open
kind: defect
opened: 2026-10-09
---

# A full DATA volume frees space only at a commit

The DATA volume's commit is shadow paging (`bcachefs/src/fs.rs`), so a block
the last committed tree reaches is not free until the next commit lands
(`BitmapAllocator`'s `pending`): an unlink or a truncate of a file a sync
already wrote gives its blocks back only at the next sync, and every change
copies the nodes it touches to new blocks first. On a volume its files filled,
a write that follows an unlink is refused `ResourceExhausted` until fileserver's
next sync, at most `WRITEBACK` later.

`NODE_RESERVE` (16 blocks) is kept from every change but a delete and an
update whose entry grows no longer, which leave the tree no larger, so a
commit gives back whatever they drew. Its limits: the deletes and shrinks
between two commits on a full volume copy at most 16 committed nodes between
them, and a run that touches more is refused until the next sync; and a tree
deeper than 16 levels could not copy one delete's path at all. Nothing reads
DATA's depth: 16 is a bound picked, not one measured against it.

Measured on a 1024-block `DataVolume` filled with 841 one-page files and
synced (a scratch host test, not committed), before the reserve was kept from
names with no data: a one-page write after an unlink was refused
`ResourceExhausted` and accepted after a sync; then 13 unlinks of every seventh
file were accepted and the 14th refused `ResourceExhausted`.

## Exit condition

On a volume its files filled, an unlink followed by a write of the blocks it
freed is accepted with no sync between them, and so is a delete of every file
on it, measured by a host test of `fileserver::data`.
