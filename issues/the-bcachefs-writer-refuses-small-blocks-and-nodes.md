---
status: open
kind: defect
opened: 2026-10-23
---

# The bcachefs writer refuses volumes of blocks under 4 KiB or nodes of 8 KiB and under

`Mount::open` (`lib/bcachefs/src/engine/mount/mod.rs`, `writable`) refuses two
geometries by name. The reader still opens both, so to ToyOS they are
read-only.

- **Blocks under 4 KiB** ("writing a volume of blocks under 4 KiB"). Every
  write is a whole 4 KiB device block (`toyos-blockdev`), so a 512-byte-block
  volume's entries and bsets would fall off one. Upstream's `format` uses
  512-byte blocks on volumes under 1 GiB unless told otherwise (A1.46).
- **Nodes too small for reclaim's largest split** ("writing a volume of btree
  nodes too small for reclaim's largest split", `splits_fit`). A node read
  from the medium can hold five times its bytes once unpacked. Before reclaim
  plans its split, two transactions more can land in it. Reclaim spreads all
  of that over at most seven new nodes (`MAX_GROUPS`, the bound its reserves
  are sized for), each taking its share and at most one key past it. That
  needs a little over 11 KiB of keys a node. So 16 KiB nodes are written, and
  8 KiB ones and smaller are refused.

ToyOS's own mkfs formats 4 KiB blocks, and nodes of a bucket up to 256 KiB.
Its smallest node, on volumes under 64 MiB, is 16 KiB. So every volume it
formats is writable. ROOT is never written online, whatever its node size.

**Exit.**
- Blocks: writes at the volume's block size, with read-modify-write of the
  device block where it is larger.
- Nodes: a split bound that does not grow with the imported node. One route is
  to rewrite an imported node to unpacked keys before it takes a change.
  Another is reserves sized for more groups.
Until then each refusal is the behaviour, by name.
