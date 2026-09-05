---
status: open
kind: track
opened: 2026-09-03
---

# Storage is four layers, and a role names a filesystem rather than a partition

What the installed product's disks look like, from the block device up to a
path, and the order the pieces land in.

## Block

A block device is a shared handle: one object per physical device, the driver
serialising its own queue, every consumer holding a handle and a partition view
(offset, length) over it. The page cache holds one instance per partition it
serves and keys every block on the partition offset it was opened over, so a
page is written back where it was filled from. Two devices claiming one id are
refused at registration, which is what keeps two of them out of one cache. GPT
is the one partition scheme. Each cache instance is capped from memory rather
than from its device, so N of them claim N times that cap.

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
install. ROOT is a bcachefs image on a partition of the boot medium, found by
partition type and selected by the UUID in its own superblock against the
kernel's `root=` argument, mounted read-only through `ReadOnlyBcacheFsAdapter`
(`kernel/src/rootfs.rs`).

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

**Foreign filesystems.** NTFS is postponed behind every other stage here by
the owner's ruling, and when it comes it is read-only: Windows fast startup
leaves a mounted NTFS hibernated, and a write corrupts it, so the server
refuses write access on a volume whose dirty or hibernation flag is set, by
name. BitLocker
volumes are reported unreadable rather than mounted. The outside judge for an
NTFS driver is Windows itself, run at development time by a builder against a
volume ToyOS wrote and its readback pasted into the pull request; the suite
reads committed fixtures and fetches nothing. ext4 follows NTFS by the same
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
  `/media/windows`. A mount point is exactly one top-level name
  (`kernel/src/vfs.rs`, `ROOT_ENTRIES` and the array indexed by it), so
  `/media/<label>` is a nested mount the structure cannot represent: the
  mount protocol owes that, and `/media` is an empty directory until then.

Users are a track of their own,
`issues/filesystem/a-user-is-a-home-tree-and-a-login-row.md`: a `/home/<user>`
tree plus a login session whose namespace init builds from a per-user row.

## Dual boot

Windows keeps its files under `EFI/Microsoft` on the shared ESP, ToyOS under
`EFI/toyos`; ToyOS never writes Windows's. Boot choice is the firmware's menu
or a two-entry ToyOS bootloader. The installer's whole obligation to the other
OS is to leave every partition it did not create untouched and add one boot
entry.

## Stages, in order

1. The users track, `issues/filesystem/a-user-is-a-home-tree-and-a-login-row.md`.
2. The mount protocol, and FAT32 as the first userland filesystem server.
3. Real bcachefs under ROOT and DATA — the format swap; nothing above changes.
4. The installer, written together with the layout it lays down: GPT, ESP,
   ROOT, a designated DATA, one boot entry.
5. Updates: one ROOT partition per release, a second beside the first, the
   bootloader choosing — no snapshot logic in the kernel's read path.
6. Multi-device, replicas, tiering, snapshots, as the bcachefs crate grows into
   them.
7. A full secure boot chain, the end state of `/system`'s immutability:
   firmware verifies the bootloader, the bootloader the kernel, the kernel the
   ROOT image it mounts, and a link that fails is refused by name.
8. NTFS read-only, postponed here by the owner.

LOG stays its own FAT32 partition through the dev phase, because a Mac has to
read the stick; folding it into DATA is the installed product's shape and no
stage above owes it yet.
