---
status: open
kind: tooling
opened: 2026-11-03
---

# Upstream cannot check a written bcachefs volume whose btree nodes are one block

The writer (`writable` in `lib/bcachefs/src/engine/mount/mod.rs`) writes
volumes whose btree node is exactly one block, such as 32 KiB nodes on 32 KiB
blocks. The owner ruled on 2026-11-03 that this geometry stays writable. Its
clean unmount terminates through the terminal publication
(`mount/publish.rs`), and ToyOS's own check passes what it writes.

Upstream's tools (v1.39.7, used as a black box) give no independent check of
those writes:

- `bcachefs format --block_size=32k --btree_node_size=32k` refuses with "btree
  node size (32768) must be greater than block size (32768)". No volume
  upstream formats has this geometry.
- `bcachefs fsck -n -f -K` reads a never-written volume of this geometry
  clean. Every volume ToyOS has written aborts on an assertion in upstream's own
  `btree_split`, when upstream changes a one-block node in memory.
  - After one create, all its check passes complete without a finding before the abort.
  - On larger volumes the abort lands inside a check pass.
  - On an unclean volume, replay fails: upstream splits a one-block
    accounting leaf itself, then refuses the pointer its own split made.
  - `-o nodiscard` changes none of this. `-o norecovery` checks nothing.

With nodes of two or more blocks, upstream passes every image ToyOS produced,
including those from crashes during the unmount.

**Owner:** the storage track.

**Exit:** an independent check of written one-block-node volumes: an
upstream release whose split handles them, or another oracle that does not
modify the volume while it checks.
