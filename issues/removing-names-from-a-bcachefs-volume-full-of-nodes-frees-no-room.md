---
status: open
kind: defect
opened: 2026-10-24
---

# Removing names from a bcachefs volume full of btree nodes frees no room

The read-write mount (`lib/bcachefs/src/engine/mount/`) splits btree nodes but
never merges them. A leaf whose keys are all deleted stays as a leaf, so the node
count only grows. Inode numbers are never reused (A8.9), so new inodes always
land in the last leaf of the inodes btree, and churn grows that btree without
bound.

**A temporary gap against design §2.3 Q3.**
- Q3 says removals (unlink, rmdir, an orphan's release) never answer `NoSpace`:
  they take a reserve that ordinary transactions leave, and each refills it from
  what it frees.
- In B6 the reserve is one removal's worth (`FREEING_RESERVE` in
  `mount/allocator.rs`), and nothing on a metadata-full volume refills it. A
  removal frees no bucket and can add keys: an unlink adds a `deleted_inodes` key,
  and hash whiteouts can stay.
- So a run of removals on such a volume can spend the reserve. It is then refused
  with `NoSpace("no room left for the btree nodes a removal may write")`.
- What is tested: one crash, then a read-write mount; 21 unlinks; an orphan's
  release; a clean unmount. Creates go on answering `NoSpace("no room left for
  the btree nodes a change may write")`.

**Exit.**
- B7, for volumes that hold file data: a removal frees its extents' buckets and
  refills the reserve from them, as Q3 states.
- For removals that free no bucket: node merges. That covers a volume full of
  metadata alone, and also unlinks of empty files or rmdirs on any full
  volume, which free no data even after B7. A leaf under a fill threshold
  merges with a neighbour in a reclaim rewrite, releasing a node. Design §3.1
  lists merges as later, so adopting them is the owner's decision.
