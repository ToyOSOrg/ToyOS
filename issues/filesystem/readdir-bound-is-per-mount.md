---
status: open
kind: defect
opened: 2026-08-30
---

# The listing bound is per mount, so a full mount cannot list an empty directory

Carried out of the closed `untrusted-input-panics` entry, which its fix does
not touch: `FileSystem::list` returns every name in the mount and `Vfs::list`
filters, because no per-directory index exists anywhere in the VFS. The
`vfs::MAX_LIST_ENTRIES` bound (16,384) therefore counts the *mount* — a tmpfs
holding 16,385 files cannot list any directory in it, including an empty one,
and every `readdir` is O(mount). `/home` behaves the same through
`bcachefs::Mounted::list`, which walks the whole tree under the same ceiling.

The fix is a real directory index, not a bigger constant.
