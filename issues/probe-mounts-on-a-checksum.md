---
status: open
kind: defect
opened: 2026-08-01
---

# `probe()` mounts on a checksum, and a stamp over a used volume does not reformat

Two things about DATA's probe (`DataVolume::probe` in
`system/fileserver/src/data.rs`) and the bcachefs engine under it.

**The threshold does not match the consequence.** A probe that mounts opens
the volume read-write (`Mount::open`): replay, orphan release, the roots it
makes and its first commit all write. So mounting a stranger's disk modifies
it, which is a weaker form of the wrong the designation stamp exists to
prevent. Accidental collision is not the risk: random bytes do not pass a
superblock's magic and checksum. The risk is a volume this machine did not
write on a partition typed as DATA — a **deliberately crafted** one on a disk
somebody hands you, which is the metal track's situation exactly.

What bounds it today:

- **The exact check** (owner, 2026-09-01: "take the exact check now"): a
  volume mounts only with exactly the buckets mkfs lays out on that partition
  (`Mount::spans_its_device`), so a volume copied onto a partition of another
  size is refused, not mounted
  (`a_volume_copied_onto_a_larger_partition_does_not_mount`). It is not
  authentication: whoever knows the partition's size writes a volume of it.
- **Hostile contents are refusals, not reach.** The writer refuses features,
  fields and geometries it does not keep (`writable`), every superblock copy,
  member and btree pointer is bounded by the device it is read from
  (`read_copy`, `read.rs`), a file extent's checksum is verified before its
  bytes are used, a symlink target past 4 KiB is refused before it is
  allocated (`readlink`), and every block request is bounded to the
  partition by the block service.

**Authentication is deferred to the metal track** by the same ruling:
read-write only for something the attacker cannot compute — a keyed MAC or a
designation-like stamp — and read-only for everything else. It needs a place
to keep a secret, and ToyOS has neither a key store nor TPM support. The
threat this leaves open is stated rather than reduced: a deliberately
crafted volume of the partition's own size still earns a read-write mount,
and read-write means written on sight.

**Separately: a designation stamp over a used volume does not reformat.** The
stamp is block 0 (`lib/bcachefs/src/designation.rs`), and it zeroes only the
superblock's layout copy at sector 7; the primary copy at sector 8, block 1,
still reads, so the probe mounts the old volume and never looks at the stamp.
A volume whose superblocks all fail, stamped, is refused "an existing volume
cannot be reformatted" while any copy's magic remains (`carries_volume`).
Harmless for the harness, which stamps fresh images, but "re-stamp the disk to
reformat `/home`" is not a workflow that works; `disks format data --commit`
in the preparation image is the way to reformat.

**Exit**: read-write mount only of a volume this machine can prove it wrote,
by the metal track's secret, with everything else read-only; shown by a test
in which a crafted volume of the partition's size mounts read-only.
