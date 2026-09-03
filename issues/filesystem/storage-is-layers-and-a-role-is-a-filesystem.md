---
status: open
kind: track
opened: 2026-09-03
---

# Storage is four layers, and a role names a filesystem rather than a partition

What the installed product's disks look like, from the block device up to a
path, and the order the pieces land in. Ruled by the orchestrator on
2026-09-03 from the discussion with the owner; the eight decisions still the
owner's are in `issues/filesystem/eight-storage-decisions-are-the-owners.md`,
and this file takes their answers.

## Block

A block device is a shared handle: one object per physical device, the driver
serialising its own queue, every consumer holding a handle and a partition view
(offset, length) over it. The page cache keys every block on (device, partition,
block) and holds one instance per partition it serves. Two devices claiming one
id are refused at registration. GPT is the one partition scheme. This is
`issues/build/page-cache-owns-one-device.md`'s ruling, and root filesystem PR 1
builds it; before it, one consumer owns the one NVMe device outright and a
machine booting off its internal disk gets neither `/boot` nor `/log`.

## Volume

A partition's type GUID says what it is, so the kernel learns roles from the
disk and no configuration file names a device. A role names a **filesystem by
its UUID**, and a filesystem is a set of members: partitions on any number of
disks, each carrying that UUID in its superblock. The probe collects every
member it can see across every disk and hands the set to the driver, which
mounts, mounts degraded, or refuses — a member vanishing is never silent. A
role on one disk is the one-member case. Two filesystems claiming one unique
role are refused rather than guessed, which `boot_partition_identity` already
asserts for the boot volume. A blank volume joins the system by the
designation stamp the bcachefs crate already carries (`DESIGNATION_MAGIC`,
`bcachefs/src/superblock.rs`): the probe formats a designated volume and
refuses to reformat a used one (`issues/isolation/probe-mounts-on-a-checksum.md`
is what that refusal still owes). Microsoft's basic-data type is a foreign
volume, handed to whichever driver recognises its superblock.

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

**The kernel links bcachefs's read path and nothing more.** The crate is pure
code over a block trait; a feature flag splits it into the read half —
superblock, btree lookup, extents — and the write half — allocator, journal,
replay, every mutation. The kernel takes the read half only, so no code that
changes a disk compiles into it. `/system` is immutable by design: the
installer and the updater write it offline, checkpoint it and mark the journal
clean; a kernel that finds a dirty journal under `/system` refuses by name,
because replay is a write-path job and a system image needing one is a broken
install. The initrd, today a read-only bcachefs image in RAM mounted through
`ReadOnlyBcacheFsAdapter` (`kernel/src/main.rs`), is deleted: the same image
sits on ROOT (`issues/build/the-initrd-is-still-the-root-filesystem.md`).

**Every other filesystem is a userland server** behind the VFS's existing
trait (`kernel/src/vfs.rs`, `FileSystem`, one access mode per mount) through
a mount protocol: init's manifest row moves the DATA members' device handles
into the storage server, which links the whole crate and mounts `/home` and
`/apps` read-write. A server that crashes loses its mounts and nothing else,
and no foreign parser runs in the kernel on bytes another OS wrote. This is
`issues/kernel/every-driver-is-still-in-the-kernel.md` applied to storage; FAT32
moves out first, being a pure crate already whose two volumes are the cheapest
to lose.

**Multi-device is a filesystem property, not a block-layer subsystem.** Pooling
across disks, replicas per subvolume, tiering with a fast foreground device,
erasure coding, snapshots, checksums and scrub, compression, encryption,
subvolume per user and per app: every one is a feature of upstream bcachefs's
format, which is why `issues/kernel/bcachefs-crate-is-not-bcachefs.md`'s
ruling — the crate becomes a real implementation — is what makes them
reachable. The block layer stays dumb. LVM- and md-shaped aggregation is
refused: it cannot tell metadata from data, place a file's replicas on
different disks, or snapshot.

**Foreign filesystems.** NTFS read-only first: Windows fast startup leaves a
mounted NTFS hibernated, and a write corrupts it, so the server refuses write
access on a volume whose dirty or hibernation flag is set, by name. BitLocker
volumes are reported unreadable rather than mounted. The outside judge for an
NTFS driver is Windows itself in a QEMU guest reading a volume ToyOS wrote —
a differential oracle that needs no host binary. ext4 follows NTFS by the same
shape if wanted.

## Paths

No drive letters, no `/usr`, `/var`, `/opt`, `/dev`, `/proc` or `/sys`:
devices and processes are capabilities and syscalls here, not files. Paths are
ambient by the owner's ruling
(`issues/kernel/the-capability-end-state-is-twelve-answers.md`), so the
hierarchy is a convention, and `/boot`'s mount guard is the one restriction.

- `/boot` — bootloader, kernel, kernel arguments.
- `/system` — the OS image, read-only, versioned: today's `bin`, `lib`, `share`
  and the manifest.
- `/apps/<name>` — each installed program in its own directory with its own
  binaries, data and manifest row; doom moves here with its WAD.
- `/home/<user>` — Documents, Downloads, `.config/<app>` for settings,
  `.local/<app>` for saves and caches.
- `/log`, `/tmp` — as today.
- `/media/<label>` — foreign and unassigned volumes; a Windows disk is
  `/media/windows`.

Users are a track of their own: a `/home/<user>` tree plus a login session
whose namespace init builds from a per-user row. The sshd key path already has
that shape.

## Dual boot

Windows keeps its files under `EFI/Microsoft` on the shared ESP, ToyOS under
`EFI/toyos`; ToyOS never writes Windows's. Boot choice is the firmware's menu
or a two-entry ToyOS bootloader. The installer's whole obligation to the other
OS is to leave every partition it did not create untouched and add one boot
entry.

## Stages, in order

1. Root filesystem PR 1: the block layer above; an internal-disk boot gets
   `/boot` and `/log`.
2. Root filesystem PR 2: ROOT, the kernel argument naming it, the initrd
   deleted, the hierarchy laid down.
3. The users track filed and built.
4. The mount protocol, and FAT32 as the first userland filesystem server.
5. NTFS read-only.
6. Real bcachefs under ROOT and DATA — the format swap; nothing above changes.
7. The installer: GPT, ESP, ROOT, a designated DATA, one boot entry.
8. Updates: a second ROOT beside the first, the bootloader choosing.
9. Multi-device, replicas, tiering, snapshots, as the bcachefs crate grows into
   them.

**Not decided here** (the owner's file names each): how ROOT is versioned,
whether LOG survives as its own partition past the dev phase, the hierarchy's
names, where the hierarchy lands, when the users track is filed, NTFS's write
policy, the installer's shape, and verified boot.
