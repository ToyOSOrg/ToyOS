---
status: assigned
kind: defect
opened: 2026-11-04
---

# DATA mounted a volume another format had written over, and wrote file data over its nodes

Held by the storage-consolidation branch's fixes to the native format's
superblock read, checksum types and mkfs, which close it.

**What happened on both benches.** The native format's preparation boots
(`disks format data --commit`, 2026-11-02, T14 20:12 UTC, AMD 20:31 UTC) ran
mkfs over DATA partitions that held the private format's live volume. mkfs
zeroed block 0, wrote superblock copies at sectors 8 and 2056 — the private
format's bitmap blocks 1 and 257 — and never wrote the partition's last
block, which held the private format's backup superblock. The next boot of a
main image (T14, 2026-11-03 19:39 UTC) found no superblock at block 0, fell
back to the backup without saying so, mounted the old volume ("22 names") and
rewrote block 0 as its own. Its bitmap now read the native superblock's bytes:
1330 of the blocks below 1728 and most of 0..32767 free, among them the
superblock, 1057 bitmap blocks and the old tree's nodes. Its allocator handed
those out; a day of boots later osbench's file data lay on a node and the next
mount refused `BadMagic { expected: "BTND", got: [212, 213, 214, 215] }`.
The AMD's DATA is the same mixture, not yet failed.

**What the evidence shows.** Captured read-only from the T14's p4 (the first
16 MiB, the last block, blocks 1325056..1361919) and the AMD's (the first
16 MiB): both private superblocks intact and identical (root 1334530, the
mount used block 0); bitmap block 1 beginning with a native superblock header
(version 0x0427, label "ToyOS DATA", seq 10, time base 2026-11-02 20:12:08
UTC) and block 257 holding a whole native copy; a tree of 190 nodes in which
block 1758 is reached twice and live node 15052 is marked free.

**What a replay shows.** osbench's persist and fs.disk sequence on the private
format over a model of the T14's partition: on an intact volume no write lands
on a node the committed tree reaches and the volume checks clean; after the
native mkfs, the mount succeeds silently, 41 writes land on committed nodes,
nodes are reached twice, and the next mount refuses with file bytes where a
node's magic was. The shared write path — the cache, the write-back, the
block service's ordering — placed nothing wrong; the private format's silent
backup fallback and unchecked bitmap did, on a partition the native mkfs left
carrying both formats.

**What the native format owes, which the fix provides.** A mount that finds
another writer's bytes where its layout puts a superblock copy or the layout
copy refuses the volume by name, and one that finds a copy a crash tore says
so and rewrites it; no metadata or data is accepted without a checksum; mkfs
leaves no other format's superblock in the places it reserves or past its
last copy.

**What the benches need.** Both DATA partitions hold neither volume whole.
The native format refuses them by name; `disks format data --commit` on each
formats them clean.
