# Storage

```text
   path        client library: views, longest prefix
     |
   filesystem  one file server per role: SYSTEM DATA LOG BOOT MEMORY
     |
   volume      a role names a filesystem by UUID; its members may span disks
     |
   block       one driver per controller; the block service grants partitions
     |
   device      NVMe, USB mass storage, ...
```

All four layers run in userland.

## Block

- One driver process per disk controller reads its disk's GPT, the one
  partition scheme, and offers the partitions to the block service.
- The block service checks a client's grant, opens the partition and hands
  the session to the driver that serves it; data never passes through the
  block service.
- A partition has one holder at a time, every request is bounded to it before
  the device sees it, and a flush answers for its own writer's writes.
- A driver answers every request it holds, or ends the session, within one
  bound the block protocol states, and shows that its worst case — a device's
  silence before the driver takes a command back, and taking it back — fits
  under it. A client that has waited longer ends the session; it never waits
  on a driver's word alone.
- Caching belongs to file servers, never to the block layer.
- Partition editing and storage initialisation are userland operations shared
  by disk tools and the installer. Editing the table, reading payloads and
  formatting are distinct authorities; the storage owner enforces stable target
  identity and excludes conflicting use. Being logged in or running a signed
  utility grants none of them.

## Volume

- A partition's type GUID says what it is, so roles come from the disk and no
  configuration names a device.
- A role names a filesystem by UUID; its members are partitions on any number
  of disks. The file server mounts the set, mounts it degraded or refuses; a
  member vanishing is never silent.
- Two filesystems claiming one unique role are refused, not guessed.
- A blank volume joins by a designation stamp; a used one is never reformatted.

| Partition | Filesystem | Serves | Writable |
|---|---|---|---|
| ESP | FAT32, as firmware requires | `/boot` | only by the updater |
| ROOT | bcachefs image, built and signed per release | `/system` | no |
| DATA | bcachefs, formatted on first boot | `/apps` `/config` `/home` `/state` | yes |
| LOG | its own partition | `/log` | yes |

- `/tmp` lives in RAM.
- On a development stick LOG is FAT32, so another computer can read a boot's
  log; whether LOG folds into DATA on an installed machine is not decided.
- Development machines keep ESP and ROOT on a stick and DATA internal; an
  installed machine carries all of them on one disk, through the same code.

## Filesystems

- Each role is served by its own file server, linking its format's whole
  implementation, so a crash loses that role alone and no filesystem parser
  runs in the kernel.
- `/system` is immutable. The installer and updater write it offline and leave
  it clean; a server that finds it needing journal replay refuses it as a
  broken installation.
- Replicas, tiering, snapshots, checksums, scrubbing, compression, encryption
  and per-user or per-app subvolumes are properties of the bcachefs format,
  not a block-layer subsystem. LVM- and md-style aggregation is refused: it
  cannot tell metadata from data, place a file's replicas on different disks or
  snapshot.
- File pages are described in [Memory](memory.md).

## Foreign filesystems

- A foreign volume appears under `/media/<label>`.
- A common format for USB exchange with Windows and macOS is required.
- NTFS, when it comes, is read-only and refuses a volume Windows left
  hibernated or dirty; BitLocker volumes are reported unreadable.
- ext4 and btrfs are the architectural cases for further formats.
