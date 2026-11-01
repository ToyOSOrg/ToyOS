---
status: open
kind: defect
opened: 2026-11-01
---

# A carried file that is refused costs every other file's fsync, or leaks its blocks

An fsync on DATA commits its own file and every file the commit carries
(`Volume::carried`): one whose entry was written, or whose reserve took
blocks, since the last commit (`system/fileserver/src/pages.rs`,
`Files::fsync`; `system/fileserver/src/data.rs`, `DataVolume::commit`). A
refusal on a carried file is paid by the files beside it:

- **Its pages refused.** `Files::fsync` refuses the commit when a carried
  file's write-back is refused, since its entry would name blocks without
  its bytes. While that write-back stays refused, every fsync of every
  file on the volume is refused with it.
- **Its entry refused.** `DataVolume::commit` says a carried entry whose
  write was refused, and commits anyway. Where the file took blocks since
  the last commit, the commit makes the allocator's take durable with no
  entry naming them, and after a stop those blocks are leaked: not readable
  by any file, and not free. This is not a disclosure.

A file carried only by blocks taken (its entry unwritten since the last
commit) could be left out of the commit at the cost of that leak, rather
than refusing everyone's fsync.

Found by review of #1141; no test drives either refusal into another file's
fsync or a stop.

**Exit**: a refusal on one carried file costs only that file: an fsync of
another lands whole, and after a stop the volume holds as free every block
no entry names. A host test refuses one carried file's write-back, and
another's entry, fsyncs a third, stops, and counts the free blocks.

**Owner**: the storage track (`issues/storage-is-layers-and-a-role-is-a-filesystem.md`).
