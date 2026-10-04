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

**This is a bound that was there and is not any more.** The old
`list(16_384)` stopped at the 16,384th entry it materialised and refused, so a
mount with a million names cost 16,384 entries of work and then an error. The
new one visits every name in the mount and filters, so that same mount costs a
million — the refusal now counts only what the listed directory holds, and
nothing counts the walk. It runs under the VFS lock a `SYS_READDIR` holds,
which every other filesystem syscall on the machine waits behind.

The end of it is a real directory index — a store keyed by (directory, name)
rather than a flat namespace of paths that happen to contain `/`. On bcachefs
that is an on-disk format change, so it is not a small one.

**`SYS_CHDIR` and `SYS_SPAWN` pay the same worst case on bcachefs.** Both judge
their directory with `FileSystem::is_dir` under the VFS lock, and
`Mounted::is_dir` (`bcachefs/src/fs.rs`) is a `for_each_live` walk that stops
at the first name beneath the directory — so a directory that is there costs
the walk up to its first entry, and one that is not costs every node on the
mount. Every spawn states a cwd, so this is on the path of every process start
whose cwd is under `/system`, `/apps` or `/home`. Nothing is kept, and a node the walk
cannot read refuses that one call. The same (directory, name) index ends it.
