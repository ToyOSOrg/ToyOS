---
status: open
kind: defect
opened: 2026-10-23
---

# The bcachefs writer refuses volumes of blocks under 4 KiB or nodes under 16 KiB

`Mount::open` (`lib/bcachefs/src/engine/mount/mod.rs`, `writable`) refuses by
name a volume whose blocks are under 4 KiB ("writing a volume of blocks under
4 KiB") or whose btree nodes are under 16 KiB ("writing a volume of btree nodes
under 16 KiB"). The reader still opens such volumes, so to ToyOS they are
read-only.

ToyOS's own mkfs always formats at 4 KiB blocks, so its volumes are never
refused. Upstream's `format` uses 512-byte blocks on volumes under 1 GiB unless
told otherwise (A1.46), and an upstream-formatted small volume is then
read-only to ToyOS.

**Why.** Every write is a whole 4 KiB device block (`toyos-blockdev`), so a
512-byte-block volume's entries and bsets would fall off one. Under 16 KiB, a
node cannot hold a full node's keys plus two transactions' split in two
(`MAX_REWRITE` in `mount/reclaim.rs`, asserted at compile time).

**Exit.** Writes at the volume's block size, with read-modify-write of the
device block where it is larger, and a rewrite bound that holds for small
nodes (more than two halves, or a node size floor taken from the
transaction bound). Until then the refusal is the behaviour, by name.
