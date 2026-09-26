---
status: open
kind: track
opened: 2026-09-03
---

# Storage is four layers, and a role names a filesystem rather than a partition

What the installed product's disks look like, from the block device up to a
path. **Where each layer runs is the small-kernel track's**
(`issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`,
stages 3 and 4, owner ruling 2026-09-26): block services and file servers are
userland programs, the VFS is a client library, and the kernel keeps ROOT's
in-memory read path and nothing else of storage. The plan this track carried
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
`bcachefs/src/superblock.rs`): the probe formats a designated volume and
refuses to reformat a used one (`issues/isolation/probe-mounts-on-a-checksum.md`
is what that refusal still owes). Microsoft's basic-data type is a foreign
volume, handed to whichever server recognises its superblock.

| partition | filesystem | mounts as | writable |
|---|---|---|---|
| ESP | FAT32, firmware's rule | `/boot` | kernel only |
| ROOT | bcachefs image the build writes | `/system` | no; versioned per release |
| DATA | bcachefs, formatted on first boot | `/apps`, `/home` | yes |
| LOG | FAT32 while a Mac has to read the dev stick | `/log` | yes |

`/tmp` has no backing. The dev loop keeps the ESP and ROOT on the stick and
DATA on the internal NVMe; the installed product carries all four on one disk.
Roles come from partitions, so both shapes are the same code.

## Filesystem

**The kernel reads ROOT and nothing more.** The loader reads the selected
slot's ROOT into memory and the kernel mounts it read-only with bcachefs's read
half; no code that changes a disk is in the kernel. `/system` is immutable by
design: the installer and the updater write it offline, checkpoint it and mark
the journal clean; a kernel that finds a dirty journal under `/system` refuses
by name, because replay is a write-path job and a system image needing one is a
broken install.

**Every other filesystem is a file server per role** — LOG, DATA, BOOT — over
its block service's sessions, linking the whole crate it serves: a server that
crashes loses its own role and nothing else, and no foreign parser runs in the
kernel on bytes another OS wrote.

**Multi-device is a filesystem property, not a block-layer subsystem.** Pooling
across disks, replicas per subvolume, tiering with a fast foreground device,
erasure coding, snapshots, checksums and scrub, compression, encryption,
subvolume per user and per app: every one is a feature of upstream bcachefs's
format, which is why `issues/kernel/bcachefs-crate-is-not-bcachefs.md`'s
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

No drive letters, no `/usr`, `/var`, `/opt`, `/dev`, `/proc` or `/sys`:
devices and processes are capabilities and syscalls here, not files. Each
process sees the directories its parent gave it
(`issues/isolation/every-program-sees-only-the-files-it-was-given.md`).

- `/boot` — bootloader, kernel, kernel arguments.
- `/system` — the OS image, read-only, versioned: today's `bin`, `lib`, `share`
  and the manifest.
- `/apps/<name>` — each installed program in its own directory with its own
  binaries, data and manifest row; doom moves here with its WAD.
- `/home/<user>` — Documents, Downloads, `.config/<app>` for settings,
  `.local/<app>` for saves and caches.
- `/log`, `/tmp` — as today.
- `/media/<label>` — foreign and unassigned volumes; a Windows disk is
  `/media/windows`: one more directory capability, served by the server that
  recognises the volume.

Users are a track of their own,
`issues/filesystem/a-user-is-a-home-tree-and-a-login-row.md`: a `/home/<user>`
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

1. The users track, `issues/filesystem/a-user-is-a-home-tree-and-a-login-row.md`.
2. Real bcachefs under ROOT and DATA — the format swap; nothing above changes.
3. The installer, written together with the layout it lays down: GPT, ESP,
   ROOT, a designated DATA, one boot entry.
4. Updates: one ROOT partition per release, a second beside the first, the
   bootloader choosing — no snapshot logic in the kernel's read path.
5. Multi-device, replicas, tiering, snapshots, as the bcachefs crate grows into
   them.
6. A full secure boot chain, the end state of `/system`'s immutability:
   firmware verifies the bootloader, the bootloader the kernel, the kernel the
   ROOT image it mounts, and a link that fails is refused by name.
7. NTFS read-only, postponed here by the owner.

LOG stays its own FAT32 partition through the dev phase, because a Mac has to
read the stick; folding it into DATA in the installed product is deferred by
the owner's ruling of 2026-09-26, and no stage above owes it yet.
