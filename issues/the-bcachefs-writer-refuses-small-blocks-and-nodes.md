---
status: open
kind: defect
opened: 2026-10-23
---

# The bcachefs writer refuses volumes of blocks under 4 KiB or nodes under 32 KiB

`Mount::open` (`lib/bcachefs/src/engine/mount/mod.rs`, `writable`) refuses by
name a volume whose blocks are under 4 KiB ("writing a volume of blocks under
4 KiB") or whose btree nodes are under 32 KiB ("writing a volume of btree nodes
under 32 KiB"). The reader still opens such volumes, so to ToyOS they are
read-only.

ToyOS's own mkfs always formats at 4 KiB blocks. Like upstream's `format`, it
takes nodes no larger than a bucket, so a volume under 64 MiB has 16 KiB nodes
and is read-only to the writer. ROOT is never written online, whatever its
node size. Upstream's `format` uses 512-byte blocks on volumes under 1 GiB unless
told otherwise (A1.46), and such a volume is read-only to ToyOS too.

**Why.** Every write is a whole 4 KiB device block (`toyos-blockdev`), so a
512-byte-block volume's entries and bsets would fall off one. Under 32 KiB, a
node has room past its first bset for about two whole-block appends. Reclaim's
changes to the allocator's own leaves then rewrite more nodes than they
settle. Measured on a 32 MiB volume of 16 KiB nodes filled by 48 028 creates,
the mount wrote 621 130 journal entries and its unmount stopped with
"journal reclaim's share exhausted". The same workload on 32 KiB nodes wrote
6 345 entries and unmounted clean (storage B6's evidence). The floor is the
coordinator's ruling on that measurement (2026-10-24).

**Exit.** Writes at the volume's block size, with read-modify-write of the
device block where it is larger. For small nodes, a write buffer for the
derived btrees, so a rewrite dirties a bounded handful of leaves. Until then
the refusal is the behaviour, by name.
