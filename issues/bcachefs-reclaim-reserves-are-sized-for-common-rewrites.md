---
status: open
kind: defect
opened: 2026-10-23
---

# bcachefs reclaim's reserves are sized for common rewrites, so a rare batch stops the mount

The read-write mount (`lib/bcachefs/src/engine/mount/`) reserves two things for
reclaim at mount time: a journal share and cache room. Neither is sized for the
worst case a batch can need. When a batch needs more than its reserve, the mount
stops read-only by name. It never overfills the cache or the ring, and it never
panics. The volume on the medium stays consistent, so the next mount replays it.

**The journal share** (`PER_NODE` in `mount/mod.rs`) is sized as `fresh_cost(1)`
per node that can pin the journal: every such node is assumed to rewrite to a
single fresh node. A node that splits in two, with a new root above it, journals
`fresh_cost(3)`, and an oversized node's split, alone in its batch, up to
`fresh_cost(8)`. Those bounds are derived from key sizes in `mount/reclaim.rs`.
When a share runs short of what a batch's splits need, `Mount::admit` stops the
mount with "journal reclaim's share exhausted".

**Cache room** (`RECLAIM_ROOM`) is sized as every position of a rewrite to one
node, each in a leaf of its own. A batch's positions can exceed that, but they
normally share leaves. A transaction that dirties more nodes than the room is
refused at apply, and the mount stops with "reclaim dirtying more nodes than its
cache room".

**Why not the worst case.** The worst-case bounds hold for every volume, and they
are derived, not measured. Sized for them, a 64 MiB volume's ring leaves a
dirty-node limit of 4, under a transaction's grant of 10. Every volume under
128 MiB laid out as upstream's `format` does would then be refused, and so would
the committed upstream fixture `unclean-32k-nodes.img.gz` that the read-write
mount is tested on. The limits for each geometry are in the evidence of the pull
request that sized the reserves (storage B5).

**Exit.** Reserves that hold for the worst case at a usable limit. One way is
a write buffer for derived-btree updates (backpointers, LRU, need_discard,
bucket_gens), flushed in batches, so a rewrite journals and dirties a bounded
handful of leaves. Another is a reservation of
exact leaves before the slot. Bucket reuse (B6) leaves both reserves as they
were: a rewrite into a reused bucket changes no more positions than one into a
free bucket. The room for new nodes has its own gap
(`issues/the-bcachefs-btree-room-bound-counts-no-reclaim-cascade.md`).
