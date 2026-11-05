# Storage

Storage is four layers, all in userland:

```text
   path        /home/toy/notes.txt     client library: views, longest prefix
     |
   filesystem  one file server per role: SYSTEM DATA LOG BOOT MEMORY
     |
   volume      a role names a filesystem by UUID; its members may span disks
     |
   block       one driver per controller; the block service grants partitions
     |
   device      NVMe, USB mass storage, ...
```

## Block

One driver process per disk controller offers its disks to the block service.
The driver reads its own disk's GPT, the one partition scheme, and offers each
partition. The block service checks a client's grant, opens the partition for
it and hands the session to the driver that serves it, so data never passes
through the block service. A partition has one holder at a time, every request
is bounded to it before the device sees it, and a flush answers for its own
writer's writes. Caching belongs to file servers, never to the block layer.

Partition editing and storage initialisation are userland operations shared by
disk tools and the installer. Editing the table, reading payloads and
formatting are distinct authorities, and the storage owner enforces stable
target identity and excludes conflicting use. Being logged in or running a
signed utility does not grant that power.

## Volume

A partition's type GUID says what it is, so roles come from the disk and no
configuration names a device. A role names a filesystem by its UUID, and a
filesystem is a set of members, partitions on any number of disks, each
carrying that UUID. The file server mounts the set, mounts it degraded or
refuses; a member vanishing is never silent. Two filesystems claiming one
unique role are refused rather than guessed. A blank volume joins the system
by a designation stamp; a used volume is never reformatted.

| Partition | Filesystem | Serves | Writable |
|---|---|---|---|
| ESP | FAT32, as firmware requires | `/boot` | read-only except to the updater |
| ROOT | bcachefs image, built and signed per release | `/system` | no |
| DATA | bcachefs, formatted on first boot | `/apps` `/config` `/home` `/state` | yes |
| LOG | FAT32, readable on any computer | `/log` | yes |

`/tmp` lives in RAM with no partition. Development machines keep the ESP and
ROOT on a USB stick and DATA on internal storage; an installed machine carries
all four on one disk. Both shapes are the same code.

## Filesystems

Every filesystem is served by a file server per role, linking the whole
implementation of its format, so a crashing server loses its own role and
nothing else, and no filesystem parser runs in the kernel. The SYSTEM server
serves ROOT out of memory the loader filled and verified; the MEMORY server
serves RAM-backed temporary files; the rest serve their block sessions.

`/system` is immutable: the installer and updater write it offline and leave
it clean, and a server that finds it needing journal replay refuses it as a
broken installation.

Multi-device features, replicas, tiering, snapshots, checksums, scrubbing,
compression, encryption and per-user or per-app subvolumes are properties of
the bcachefs format, reached by implementing it, not a block-layer subsystem.
LVM- and md-style aggregation is refused: it cannot tell metadata from data,
place a file's replicas on different disks or snapshot.

## Foreign filesystems

A foreign volume appears under `/media/<label>`. A common format for USB
exchange with Windows and macOS is required. NTFS, when it comes, is read-only
and refuses a volume Windows left hibernated or dirty; BitLocker volumes are
reported unreadable. ext4 and btrfs are the architectural cases for further
formats.

## File pages

A file's pages are a paged memory object supplied by its file server and read
by its holders without a request; the file server's own cache holds only the
volume's structures. One authoritative page cache serves both ordinary I/O and
mappings. See [Memory](memory.md).
