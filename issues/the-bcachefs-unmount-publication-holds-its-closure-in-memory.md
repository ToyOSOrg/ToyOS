---
status: open
kind: defect
opened: 2026-11-04
---

# The bcachefs clean unmount holds its whole publication in memory

A clean unmount ends in one terminal publication
(`lib/bcachefs/src/engine/mount/publish.rs`). Every node it writes stays in
the node cache, held, from the plan's start until the clean entry. The
cache evicts only clean, unheld nodes, so a closure larger than
`MountOptions::cache_nodes` grows the cache past it, and no bound checks
that it fits.

Before the publication, reclaim writes everything committed before the
unmount, so the closure is normally reclaim's own leftovers. Across the
bcachefs suite's 15015 publications, crash judges included, the largest had
27 members. With that reclaim pass skipped, 207 changes scattered over a
20000-file volume made a closure of 127. The fileserver's DATA cache holds
512 nodes. A volume of scattered nodes can still need a closure up to its
derived btrees' size, as
`issues/the-bcachefs-btree-room-bound-counts-no-reclaim-cascade.md`
describes for slots.

**Owner:** the storage track.

**Exit:** a closure that fits the cache by construction. One route: write
members as soon as their contents settle, evict them, and re-read one if a
later round changes it, each rewrite under a new node sequence. Another is
a bound checked before the publication starts.
