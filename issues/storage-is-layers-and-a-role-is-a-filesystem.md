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
userland programs, the VFS is a client library, and the kernel keeps no filesystem implementation, including ROOT or tmpfs. The plan this track carried
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
| ESP | FAT32, firmware's rule | `/boot` | authorized updater through the BOOT file server |
| ROOT | bcachefs image the build writes | `/system` | no; versioned per release |
| DATA | bcachefs, formatted on first boot | `/apps`, `/config`, `/home`, `/state` | yes |
| LOG | FAT32 while a Mac has to read the dev stick | `/log` | yes |

This role layout is not a completed encrypted-storage layout: reconcile DATA
with personal storage that requires human unlock, separately auto-unlocked
server storage, and the shared-capacity requirement.

`/tmp` has no backing. The dev loop keeps the ESP and ROOT on the stick and
DATA on the internal NVMe; the installed product carries all four on one disk.
Roles come from partitions, so both shapes are the same code.

## Filesystem

### Filesystem and data policy

- Implement genuine bcachefs in Rust, initially one drive. Preserve the format's
  future pooling/mirroring path without implementing RAID now. Refuse unsupported
  layouts; do not invent a substitute format. Snapshots and compression are deferred.
- Require crash recovery and the format's normal integrity/authentication
  protections; no additional universal file-content checksum requirement.
- Use case-sensitive names. Share free capacity across users/apps, protect an OS
  reserve, warn early and show growth. Cleanup is the user's choice, not automatic
  cache deletion. Do not introduce default per-app storage quotas.
- Buffer ordinary writes. Explicit durable completion must survive the promised
  failure model. If a mutation loses its reply, report an uncertain outcome and
  retry only when proven safe. Favor interactive storage progress without starving
  background work; queue policy remains engineering work.
- Personal storage is one encrypted shared volume, with encrypted contents and
  names. Raw-disk sizes/timestamps may remain visible. Each regular user may unlock
  it with their enrolled method; OS permissions separate users. Authentication,
  recovery and screen/session lifetime are owned by the user track.
- Keep shared free space; independent per-user encryption is deferred. Explicitly
  configured server data may later unlock after trusted boot without a user;
  unattended-server acceptance is deferred. Do not conflate server keys with personal
  unlock or require each application to implement storage encryption itself.
- Provide built-in local storage for app-owned tokens. Keep account access and
  lifecycle explicit, without becoming a competing password-manager UI. Chromium
  needs laptop-stored and USB-key passkeys; prefer direct credential-service
  integration subject to design review. Passkey transfer/sync can wait.
- Support coherent shared file mappings, including files larger than RAM, through
  generic memory mechanisms. Fileserver owns identity, contents, writeback and
  naming. Unlink removes a name, not existing open handles or mappings.
- Read/write removable-file exchange needs a common Windows/macOS-compatible
  format; selection and safe-eject behavior remain open. No promise of every
  foreign format. No built-in backup product or automatic recovery snapshots.

### Filesystem ownership

**All filesystems are userland services.** The verified boot handoff supplies
self-contained supervisor and initial fileserver images plus opaque ROOT bytes,
bound to one release. The kernel validates extents and starts the supervisor by
image capability; it parses no filesystem or pathname. The supervisor starts
the ordinary fileserver in read-only in-memory ROOT mode before reading its
manifest. Neither bootstrap program depends on that filesystem or dynamic libraries.
Keep authenticated role/length bindings, immutable image lifetime and restart access.

Serve per-session `/tmp` through a RAM-backed mode of the ordinary fileserver.
Do not require a disk format just to store temporary files. Coordinate coherent
mapping and client migration; delete kernel tmpfs, pathname service and file cache.
Other roles use their block-service sessions. Parent-supervised crash recovery
may interrupt a role; it does not make operations transparently replayable.

**Multi-device is a filesystem property, not a block-layer subsystem.**
Owner decision, 2026-10-16: keep future drive pooling and mirroring as
filesystem-selection requirements, but implement single-drive operation first.
Use the selected format's genuine single-drive configuration; do not invent a
temporary format or implement unused RAID machinery. Unsupported layouts must
be refused. Account for the format's baseline complexity even with one drive.

The later multi-drive work includes replica placement, reconstruction, degraded
operation and recovery from interrupted changes. Specific expansion and
conversion paths must be designed and verified; choosing a capable format alone
does not promise every layout can be converted without migrating data. These
responsibilities stay in the filesystem service. The block layer exposes the
devices; a separate LVM- or md-shaped aggregation layer is not selected.
Parity, tiering and the other features under review above are separate choices.

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

Owner release-scope decision, 2026-10-18: a dedicated ToyOS drive is sufficient
for release one. Same-drive coexistence validation and filesystem resizing are
deferred. The preservation rules below remain architectural constraints, not
an added release gate or permission to modify unrelated partitions.

Windows keeps its files under `EFI/Microsoft` on the shared ESP, ToyOS under
`EFI/toyos`; ToyOS never writes Windows's. Boot choice is the firmware's menu
or a two-entry ToyOS bootloader. The installer's whole obligation to the other
OS is to leave every partition it did not create untouched and add one boot
entry.

## Stages, in order

The block services and the file servers are the small-kernel track's steps;
what is left here is built on them.

1. The users track, `issues/a-user-is-a-home-tree-and-a-login-row.md`.
2. Implement genuine single-drive bcachefs with first-release encryption and
   recovery, reconciling ROOT/DATA and the personal/server unlock boundaries.
3. The installer, written together with the layout it lays down: GPT, ESP,
   ROOT, a designated DATA, one boot entry.
4. Updates: one ROOT partition per release, a second beside the first, the
   verified activation; no downgrade/rollback or snapshot requirement.
5. Drive pooling and mirroring. The remaining features await the requirements
   review above before their scope and implementation order are settled.
6. A full secure boot chain, the end state of `/system`'s immutability:
   firmware verifies the loader, which authenticates the kernel, bootstrap
   programs and ROOT as one release; failures are refused before handoff.
7. NTFS read-only, postponed here by the owner.

LOG stays its own FAT32 partition through the dev phase, because a Mac has to
read the stick; folding it into DATA in the installed product is deferred by
the owner's ruling of 2026-09-26, and no stage above owes it yet.
