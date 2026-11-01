---
status: open
kind: defect
opened: 2026-10-23
---

# The bcachefs check holds a volume's metadata in memory

`lib/bcachefs/src/engine/fsck.rs` walks every node once and keeps what it
compares until the walk ends:
- every inode, decoded;
- every dirent with its value;
- every extent key;
- the stored alloc, freespace, LRU, backpointer and accounting keys;
- `derive.rs`'s per-bucket usage, backpointers and accounting.

By inspection that is a few hundred bytes per file, roughly 0.5 GB per
million files. This is an estimate, not a measurement.

It is acceptable where fsck runs today: host tests and ToyOS's own images
for the upstream judge. Design §7's fsck on the first mount after
designation meets an empty DATA. A check of a full DATA volume, such as
§7's explicit test job or any later mount, would need memory in proportion
to its files.

**Owner:** the storage track's engine stages. Whoever first runs fsck on a
populated DATA takes it up.

**Exit:** fsck compares each btree with its derivation as both stream in
position order, holding only bounded per-key state. The namespace checks
that join inodes to dirents and extents also run in bounded memory. A host
test then checks a volume of many files under a stated memory bound.
