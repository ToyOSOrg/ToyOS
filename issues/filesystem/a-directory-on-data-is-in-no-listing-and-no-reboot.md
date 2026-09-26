---
status: open
kind: defect
opened: 2026-09-26
---

# A directory on DATA is in no listing and survives no reboot

The DATA adapter answers every `create_dir` with `NotSupported`
(`kernel/src/bcachefs_adapter.rs`), so the VFS keeps the directory itself in
`created_dirs` (`kernel/src/vfs.rs`). Three things follow:

- **No listing of the parent shows it.** `Vfs::list` builds its answer from
  the filesystem's files and consults `created_dirs` only to decide whether the
  listed directory itself exists. After init makes `/home/toy` and its eight
  folders, `ls /home/toy` prints nothing and `ls /home` does not name `toy`.
  A directory appears in its parent's listing only once a file is under it.
- **A child is taken whose parent was never made.** `create_dir("/home/toy/Apps")`
  succeeds with no `/home/toy`, so `std::fs::create_dir_all` makes the leaf and
  nothing above it, and `read_dir("/home/toy")` then answers `NotFound`. init
  makes every level in turn for this reason (`make_dir` in
  `userland/init/src/main.rs`).
- **It is memory.** Nothing reaches the volume, so an empty directory is gone
  at the next boot; init remakes the session home and each service's `/state`
  every boot.

Seen building `layout_fresh_boot`: `read_dir /home/toy: entity not found` from
a boot whose init had just made `/home/toy/Apps`, which is why
`tests/toyos-rust-tests/src/bin/layout_paths.rs` asks for each directory by
name.

## Owner

The storage track, `issues/filesystem/storage-is-layers-and-a-role-is-a-filesystem.md`:
real bcachefs under DATA carries directories.

## What would close it

DATA stores directories, `create_dir` refuses a missing parent, a listing names
every directory under it, and `created_dirs` is gone or kept only for a mount
that has no directories and says so.
