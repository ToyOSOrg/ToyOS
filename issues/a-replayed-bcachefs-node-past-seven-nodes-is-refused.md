---
status: open
kind: defect
opened: 2026-10-23
---

# A replayed bcachefs node whose keys fill more than seven nodes stops the read-write mount

The writer splits a node in one reclaim transaction, into at most seven
nodes and a root (`MAX_GROUPS` in `lib/bcachefs/src/engine/mount/reclaim.rs`).
That bound holds for a node it wrote, and for one read from the medium, packed,
plus two transactions. It does not hold for a node a replay fills:
`Mount::install_replay` puts every key of the journal window into the leaf
its on-disk tree leads to, with no bound. A window that puts more than seven
nodes' keys into one leaf is reachable on an unclean upstream volume. Examples
are many inodes journaled since their leaf was last written, or keys journaled
into split nodes whose pointer update was not yet journaled.

The reader replays such a volume in memory and reads it whole. The
read-write mount opens it, but its first write that needs the leaf split
stops the mount, read-only by name: "a btree node whose keys fill more than
seven nodes". `tests/journal.rs`,
`a_replayed_leaf_past_seven_nodes_is_read_but_not_mounted_read_write`, builds
the case: about 2 860 inodes in 13 entries of a 64 MiB volume's window.

**Exit.** A split that spans several reclaim transactions for a node no
single transaction can split. For instance, write the window's keys out as a
run of new leaves under the old leaf's parent, a transaction at a time,
before the old leaf's pointer is replaced. Alternatively, a replay that
installs keys into nodes bounded as it goes.
