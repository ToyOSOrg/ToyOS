---
status: open
kind: track
opened: 2026-09-03
---

# Storage is four layers, and a role names a filesystem rather than a partition

What the installed product's disks look like, from the block device up to a
path. **Where each layer runs is the small-kernel track's**
(`issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`,
stages 3 and 4, owner ruling 2026-09-26): block services and file servers are
userland programs and the VFS is a client library. The kernel keeps no
filesystem of ROOT: owner ruling 2026-10-21, which superseded the in-memory read
path the 2026-09-26 ruling kept, hands ROOT's bytes to the supervisor and its
file server serves them (Filesystem, below). The plan this track carried
before that ruling — a kernel VFS forwarding to userland servers through a
mount protocol — is superseded, and nothing here builds it.

## Block

A block device is a service: one program per controller (`blockd` for NVMe;
the xHCI's own program for USB mass storage, once it has one), serving each
partition as a session over `toyos-blockring`. A partition has one holder,
every request is bounded to it before the device sees it, and a flush answers
for its own writer's writes. GPT is the one partition scheme, read by the
service that drives the disk. Caching is the file server's, never the block
service's.

## Volume

A partition's type GUID says what it is, so roles come from the disk and no
configuration file names a device. A role names a **filesystem by its UUID**,
and a filesystem is a set of members: partitions on any number of disks, each
carrying that UUID in its superblock. The probe collects every member it can
see across every disk and hands the set to the file server, which mounts,
mounts degraded, or refuses — a member vanishing is never silent. A role on one
disk is the one-member case. Two filesystems claiming one unique role are
refused rather than guessed, which `boot_partition_identity` already asserts
for the boot volume. A blank volume joins the system by the designation stamp
the bcachefs crate already carries (`DESIGNATION_MAGIC`,
`lib/bcachefs/src/superblock.rs`): the probe formats a designated volume and
refuses to reformat a used one (`issues/probe-mounts-on-a-checksum.md`
is what that refusal still owes). Microsoft's basic-data type is a foreign
volume, handed to whichever server recognises its superblock.

| partition | filesystem | mounts as | writable |
|---|---|---|---|
| ESP | FAT32, firmware's rule | `/boot` | kernel only |
| ROOT | bcachefs image the build writes | `/system` | no; signed per release |
| DATA | bcachefs, formatted on first boot | `/apps`, `/config`, `/home`, `/state` | yes |
| LOG | FAT32 while a Mac has to read the dev stick | `/log` | yes |

`/tmp` has no backing. The dev loop keeps the ESP and ROOT on the stick and
DATA on the internal NVMe; the installed product carries all four on one disk.
Roles come from partitions, so both shapes are the same code.

## Filesystem

**ROOT is opaque to the kernel.** The loader reads the selected slot's ROOT,
the supervisor and the file server into memory, each a section of the slot's
signed header, and the kernel keeps their pages read-only, starts the
supervisor from its image and hands it the other two. The supervisor starts the
file server's SYSTEM role from its image, serving ROOT read-only out of that
memory, before it reads a file. `/system` is immutable by design: the installer
and the updater write it offline, checkpoint it and mark the journal clean; a
server that finds a dirty journal under `/system` refuses by name, because
replay is a write-path job and a system image needing one is a broken install.

**Every filesystem is a file server per role** — SYSTEM, LOG, DATA, BOOT —
SYSTEM over ROOT's bytes in memory and every other over its block service's
sessions, linking the whole crate it serves: a server that crashes loses its
own role and nothing else, and no filesystem parser runs in the kernel.

**Multi-device is a filesystem property, not a block-layer subsystem.** Pooling
across disks, replicas per subvolume, tiering with a fast foreground device,
erasure coding, snapshots, checksums and scrub, compression, encryption,
subvolume per user and per app: every one is a feature of upstream bcachefs's
format, which is why `issues/bcachefs-crate-is-not-bcachefs.md`'s
ruling — the crate becomes a real implementation — is what makes them
reachable. The block layer stays dumb. LVM- and md-shaped aggregation is
refused: it cannot tell metadata from data, place a file's replicas on
different disks, or snapshot.

**Foreign filesystems.** NTFS is postponed behind every other stage here by
the owner's ruling, and when it comes it is read-only: Windows fast startup
leaves a mounted NTFS hibernated, and a write corrupts it, so the server
refuses write access on a volume whose dirty or hibernation flag is set, by
name. BitLocker volumes are reported unreadable rather than mounted. The
outside judge for an NTFS driver is Windows itself, run at development time by
a builder against a volume ToyOS wrote and its readback pasted into the pull
request; the suite reads committed fixtures and fetches nothing. ext4 follows
NTFS by the same shape if wanted.

## Paths

The tree, its names and how a program finds them are
`issues/where-everything-lives.md`. What this track owes it is the
mount structure: a mount point is exactly one top-level name
(`kernel/src/vfs.rs`, `ROOT_ENTRIES` and the array indexed by it), so
`/media/<label>` for a foreign volume — a Windows disk is `/media/windows` — is
a nested mount the structure cannot represent. The mount protocol owes that,
and `/media` is an empty directory until then.

Users are a track of their own,
`issues/a-user-is-a-home-tree-and-a-login-row.md`: a `/home/<user>`
tree plus a login session whose view init builds from a per-user row.

## Dual boot

Windows keeps its files under `EFI/Microsoft` on the shared ESP, ToyOS under
`EFI/toyos`; ToyOS never writes Windows's. Boot choice is the firmware's menu
or a two-entry ToyOS bootloader. The installer's whole obligation to the other
OS is to leave every partition it did not create untouched and add one boot
entry.

## Stages, in order

The block services and the file servers are the small-kernel track's steps;
what is left here is built on them.

1. The users track, `issues/a-user-is-a-home-tree-and-a-login-row.md`.
2. Real bcachefs under ROOT and DATA — the format swap; nothing above changes.
3. The installer, written together with the layout it lays down: GPT, ESP,
   ROOT, a designated DATA, one boot entry.
4. Updates: one ROOT partition per release, a second beside the first, the
   bootloader choosing — no snapshot logic in the kernel's read path.
5. Multi-device, replicas, tiering, snapshots, as the bcachefs crate grows into
   them.
6. A full secure boot chain, the end state of `/system`'s immutability:
   firmware verifies the bootloader, the bootloader every section of the slot
   it boots (built: the signed header binds the kernel, the two boot programs
   and ROOT), and a link that fails is refused by name.
7. NTFS read-only, postponed here by the owner.

**A file's pages are the kernel's paged objects'** (memory stage 7a, built by
this track): one per file, supplied by its file server, read by its holders
without a request (`system/fileserver/src/pages.rs`). The server's block cache
keeps the volumes' structures alone. Mapping them is memory stage 7b, the
process-memory track's
(`issues/process-memory-in-4-kib-pages-still-copies-each-programs-text.md`).

LOG stays its own FAT32 partition through the dev phase, because a Mac has to
read the stick; folding it into DATA in the installed product is deferred by
the owner's ruling of 2026-09-26, and no stage above owes it yet.
