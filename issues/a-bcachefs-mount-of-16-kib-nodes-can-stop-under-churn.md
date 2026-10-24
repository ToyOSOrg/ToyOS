---
status: open
kind: defect
opened: 2026-10-24
---

# A bcachefs read-write mount of 16 KiB btree nodes can stop under sustained churn

On a volume whose btree nodes are 16 KiB (mkfs and upstream's `format` pick
them under 64 MiB), reclaim's writes of the allocator's own leaves need not
settle. A node's bsets are whole 4 KiB device blocks, so a node rewritten at two
thirds full has room for about two appends. Each rewrite takes a bucket and
releases one, changing the alloc and backpointer keys of both. Once buckets are
reused, those keys lie in different leaves, so one rewrite dirties about four
leaves, each a third or half of the way to its own rewrite. The cascade then
grows rather than dies out.

**Evidence** (storage B6, at the branch head named in its pull request): a
32 MiB volume filled by 48 028 creates wrote 621 000 journal entries. After 20
unlinks its clean unmount stopped read-only with "journal reclaim's share
exhausted". Its leaves-first sweep kept re-dirtying alloc and backpointer
leaves (11 dirty nodes growing to 25), and never reached the level-1 nodes
holding the journal's oldest pin. The same workload on 64 MiB (32 KiB nodes)
filled after 134 943 creates in 6 345 entries, and its unmount finished in 27
rounds. Nothing is lost when this happens: the mount stops by name, and the
volume on the medium replays.

**Exit.** A write buffer for the derived btrees, which journals their keys and
writes them into leaves in batches, so a rewrite dirties a bounded handful of
leaves (the exit `issues/bcachefs-reclaim-reserves-are-sized-for-common-rewrites.md`
names). Alternatively, a writer floor of 32 KiB nodes
(`issues/the-bcachefs-writer-refuses-small-blocks-and-nodes.md`). That floor
would also refuse read-write the 32 MiB upstream fixture `unclean.img.gz`,
which B5 mounts read-write.
