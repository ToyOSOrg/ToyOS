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

On a volume full of metadata, removing names (unlink, rmdir, an orphan's
release) succeeds on the freeing reserve, but it frees no bucket. Creates keep
answering `NoSpace("no room left for the btree nodes a change may write")`. File
data is the room that comes back once B7 writes it.

**Exit.** Node merges: a leaf under a fill threshold is merged with a
neighbour in a reclaim rewrite, releasing a node.
