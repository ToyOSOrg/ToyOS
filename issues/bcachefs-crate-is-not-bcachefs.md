---
status: open
kind: track
opened: 2026-08-01
---

# The `bcachefs/` crate does not implement bcachefs

ToyOS's `bcachefs/` crate implements a ToyOS-native on-disk format written from scratch.
It shares a name with Linux bcachefs and nothing else, and **the magics alone settle
it**: ours is a four-byte `MAGIC = b"BCFS"` plus
`DESIGNATION_MAGIC = b"TOYOS-FORMAT-ME\0"` (`superblock.rs:5,24`) and `NODE_MAGIC = b"BTND"`
(`btree.rs:7`), against upstream's 16-byte UUID `BCHFS_MAGIC`, its per-bset
`BSET_MAGIC ^ sb.uuid` and its `JSET_MAGIC`. Neither implementation could mount
what the other writes, and neither would get past the first block trying.

That collision has already cost this project once: research into the *upstream*
format was filed under this crate's name and needed a warning at its top to stop
a reader taking it for documentation of what we ship. Warning the reader fixes
the document and not the collision — a crate that does not implement the format
it is named after is a hazard we keep paying for, in exactly this way. Renaming
it is the owner's call, not something to do in a docs pass.

## Answered by the owner, 2026-08-15

> *"bcachefs is the default filesystem for toyos. the crate must be an
> implementation of the spec."*

The resolution is neither a rename nor a deletion. **`bcachefs/` must become a
real implementation of the bcachefs on-disk format, and that format is ToyOS's
default filesystem.** The name stops being wrong by the crate growing into it.

This entry therefore stops being a question and becomes work: what is owed is the
implementation, and the gap between the two formats recorded above is the measure
of it. The upstream research this tree used to carry went with the documents, so
whoever picks this up reads the format out of upstream's own source and
specification again — that reading is part of the work now, not a head start on
it.

The track itself is not planned here.

One fact about the tree that a real-bcachefs track inherits whichever way it is
sequenced: the kernel must parse this format to reach `/system/bin/init` at all, so a
machine whose ROOT it cannot mount has no userland. The observation the ruling
inverts is the other half — a home-grown format has no second implementation to
be judged against, and upstream bcachefs is exactly such a judge.

**How that judge is used, since a judge is not a dependency:** upstream's `bcachefs-tools` is a *development*
instrument, run by a builder on a volume ToyOS wrote, with its `fsck` and its
readback pasted into the pull request. What the suite runs against is committed
fixtures — volumes upstream's tools wrote, recorded in `NOTICE`. Nothing is
fetched and no distribution is depended on.
