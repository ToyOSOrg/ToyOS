---
status: open
kind: defect
opened: 2026-10-09
---

# A btree split leaves nodes holding a few entries

`bcachefs/src/btree.rs`'s `split_node` packs a leaf that overflows into as
few nodes as hold it (`pack`), each filled in key order: a full node and a
node holding what is left over, often one entry. Keys are hashes of names, so
the next insert lands in the full node more often than not and splits it
again. The tree grows by about a node every few entries where a node holds
dozens.

Measured with `main`'s crate at 198a9d38e (a scratch binary, not committed):
300 names with no data created on a 1024-block volume, one sync after each.
The first 72 fit one leaf, 4 blocks used; the 228 after them took 93 more
blocks, about 2.5 entries a node. Every DATA create, every `mkdir` and every
`/state/<service>` pays it, and ROOT's mkfs as well.

## Exit condition

A host test that creates 300 names one at a time finds the volume's nodes,
leaves and interior, holding on average at least half what a node can.
