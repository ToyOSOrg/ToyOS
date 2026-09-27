---
status: open
kind: defect
opened: 2026-09-27
---

# A file server killed mid-sync leaves a half-written btree

`userland/fsd`'s DATA volume is `bcachefs`'s interim format: the btree is
rewritten in place and there is no journal. A sync writes the dirty blocks
fsd's cache holds (`userland/fsd/src/cache.rs`) in block order and then asks
the device to flush; nothing orders a node's children before the node that
names them, and nothing records which of them reached the device. A server
that ends between two of those writes — a crash, a kill, the restart budget
`toyos_manifest::RESTARTS` exists for — or a power cut in the same window
leaves a volume whose next mount reads a node naming blocks that hold another
generation's bytes.

`fsd_restart` does not see it: it ends the server under a write that no sync
has started, so what is on the device is the last sync's whole state.

**Exit**: a sync that a cut at any block boundary leaves mountable, as the
last acknowledged sync or the one before it — copy-on-write with a superblock
that is written last, or a journal replayed at mount — with a host test that
cuts a sync after every block it writes and mounts what is left.
