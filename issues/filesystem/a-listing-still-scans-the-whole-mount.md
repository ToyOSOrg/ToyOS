---
status: open
kind: defect
opened: 2026-09-01
---

# A listing costs the whole mount on the two mounts with no directory structure

The `FileSystem::list` bound is per directory now (`kernel/src/vfs.rs`,
`under_directory`), and FAT32 pays only the named subtree because it descends
to it (`Fat32::walk`'s `under`). The other two mounts still read every name in
the mount and drop the ones that do not match:

- `TmpFs::list` (`kernel/src/tmpfs.rs`) iterates the whole `entries` map.
- `Mounted::list` (`bcachefs/src/fs.rs`) walks the whole B+tree through
  `btree::for_each_live`, decoding every leaf value to learn its name.

So a `readdir` of one directory is O(mount) on `/tmp` and `/home`, and it
decodes a leaf value per entry on `/home`. Only the *allocation* is bounded by
what the directory holds; the work is not. Nothing measures it, and no test
would notice the difference.

The end of it is a real directory index — a store keyed by (directory, name)
rather than a flat namespace of paths that happen to contain `/`. On bcachefs
that is an on-disk format change, so it is not a small one.
