---
status: open
kind: track
opened: 2026-08-01
---

# The `lib/bcachefs/` crate must implement bcachefs

The crate once carried a ToyOS-native format under bcachefs's name (`BCFS`
and `BTND` magics), which no bcachefs tool could read. The owner ruled on
2026-08-15:

> *"bcachefs is the default filesystem for toyos. the crate must be an
> implementation of the spec."*

The format is implemented clean-room (root `CLAUDE.md`): an analyst reads
upstream and writes the specification, and a different agent implements from
it.

**Where it stands** (#1152). The private format is gone. `lib/bcachefs/src/engine`
reads single-device 1.39 volumes, crc32c and ChaCha20/Poly1305, and replays the
journal of one not cleanly unmounted. It formats ROOT at build time and DATA
on a designated partition, and mounts DATA read-write: namespace operations
and file data as journaled transactions, holes as absent extents, sparse
bucket evacuation, journal reclaim and a clean unmount. Its fsck derives the
allocator and accounting state from a volume's keys and compares. It refuses
to write a volume of blocks under 4 KiB or nodes under 32 KiB
(`issues/the-bcachefs-writer-refuses-small-blocks-and-nodes.md`), and its
reclaim reserves are sized for common rewrites
(`issues/bcachefs-reclaim-reserves-are-sized-for-common-rewrites.md`).

**Exit**: the integrated writer's volumes, ROOT and a used DATA on each bench
included, pass upstream's `bcachefs fsck -n -K`, with that evidence published.
At #1152's writer before journal reclaim was integrated, both benches' DATA and
the host fixtures passed upstream's fsck. A clean unmount ends in one terminal
publication (`mount/publish.rs`) on every geometry the writer accepts; a
volume whose btree nodes are one block is one upstream cannot check
(`issues/upstream-cannot-check-written-bcachefs-volumes-of-one-block-nodes.md`).

**How the judge is used, since a judge is not a dependency:** upstream's
`bcachefs-tools` is a *development* instrument, run by a builder on a volume
ToyOS wrote, with its `fsck` and its readback published as evidence. What the
suite runs against is committed fixtures — volumes upstream's tools wrote,
recorded in `NOTICE`. Nothing is fetched and no distribution is depended on.
