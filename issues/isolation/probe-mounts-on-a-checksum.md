---
status: open
kind: defect
opened: 2026-08-01
---

# `probe()` mounts on a checksum, and a stamp over a used volume does not reformat

Two things, from reading `bcachefs_adapter::probe` against the crate:

**The threshold does not match the consequence.** `Storage::Ours` is a
read-write mount: `sync()` rewrites both superblocks, and any file operation
writes the bitmap, btree nodes and data. So mounting a stranger's disk modifies
it, which is a weaker form of the wrong the designation stamp exists to prevent.
Accidental collision is not the risk — random block-0 bytes satisfy 4 bytes of
magic, 4 of version and a 32-bit CRC with probability about 2^-64 — and neither
is a *genuine* upstream bcachefs volume, which does not begin with ASCII `BCFS`
(this crate shares the name and nothing else; `issues/kernel/`). The risk is a **deliberately
crafted block 0** on a disk somebody hands you, which is the metal track's
situation exactly.

Recommendation, for the owner to decide:

- **Done:** `Superblock::check` takes `block_count != device_blocks`, so a
  volume image copied onto a different disk stops mounting — the same property
  the designation stamp's block count gives a format. It is not authentication:
  an attacker who knows the disk size writes the right number. `volume_from_another_disk`
  is the guest arm and `a_volume_copied_onto_a_larger_device_does_not_mount` the
  host one.
- **Done, 2026-08-23:** a file's extents are range-checked against the volume
  in `decode_leaf_value`, so "mounting a hostile volume is merely rude" no
  longer has an unchecked extent reaching a block read — or a bitmap write —
  behind it. The residual bound this left — `read_link` sizing one kernel
  allocation from the volume rather than the smaller heap ceiling — was closed
  by giving `Mounted::read_link` a `max_len` the adapter passes as
  `MAX_LINK_TARGET`, refused as `FsError::TargetTooLong` before the allocation.
- **The real fix, if the threat model wants one:** read-write requires
  something the attacker cannot compute — a keyed MAC, or a designation-like
  stamp — and everything else mounts read-only. ToyOS has no key store and no
  TPM support, so this is a metal-track decision, not a patch.

**The owner ruled on 2026-09-01: take the exact check now, authenticate on
the metal track.** The exact check is taken and the first bullet above is
closed. It is not authentication and the entry must keep saying so: an attacker
who knows the disk's size writes the right number. What it removes is every case
where the volume did not come from this disk.

Authentication — read-write only for something the attacker cannot compute,
read-only for everything else — is deferred to the metal track, because it needs
a place to keep a secret and ToyOS has neither a key store nor TPM support. The
threat this leaves open is stated rather than reduced: a deliberately crafted
block 0 on a disk handed to the machine still earns a read-write mount, and
read-write means written on sight.

**Separately, and reproduced:** a designation stamp written over a disk that
already held a ToyOS volume does **not** cause a reformat. `designate_for_format`
writes block 0 only, `Superblock::read` falls back to the backup superblock at
the last block when block 0 does not parse, and a stamp does not parse — so
`mount()` succeeds from the backup and `probe()` returns `Ours`, mounting the old
volume. Harmless for the harness, which stamps freshly created sparse files, but
it means "re-stamp the disk to reformat `/home`" is not a workflow that works.
`probe`'s doc comment claims the decision comes "from one read of block 0"; it
comes from two, and the second one wins.
