---
status: open
kind: defect
opened: 2026-10-24
---

# The bcachefs mount's room for btree nodes counts no reclaim cascade

The read-write mount (`lib/bcachefs/src/engine/mount/`) admits a transaction only
while the room left can rewrite every dirty, unpointed or granted node, and
reclaim's own room, at three slots each (`COMMON_REWRITE`). It adds a margin for
packed upstream nodes, and ordinary transactions also leave the freeing reserve
(`Allocator::fits` in `mount/allocator.rs`). A bucket comes back only once its
emptying is durable.

**What this bound does not count:**
- **Reclaim's cascade.** Reclaim's own pointer transactions dirty allocator,
  backpointer and accounting leaves, and those leaves can need rewrites of their
  own. The bound counts reclaim's room once, not the rewrites that room causes.
- **Buckets of several nodes.** On 2 MiB buckets of 256 KiB nodes, upstream's
  geometry for large devices, a rewrite returns no slot until every node in its
  bucket is dead. Partly dead btree buckets stay until something moves their live
  nodes.

**When either is exceeded,** `take_bucket` finds no bucket. Reclaim then answers
`NoSpace("no free bucket for a btree node")`, and the mount stops read-only by
that name. The volume on the medium stays consistent and replays. No B6 test
reaches it: the full-volume tests, crashed or not, unmount clean on 32 KiB nodes.

**Owner:** the storage track.

**Exit:** B8's copygc, which moves the live nodes out of partly dead buckets and
keeps their reserve. The cascade's bound comes with the derived-btree write buffer
(`issues/bcachefs-reclaim-reserves-are-sized-for-common-rewrites.md`).
