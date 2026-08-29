---
status: open
kind: defect
opened: 2026-08-28
---

# Directories are a VFS fiction: `mkdir`/`rmdir` never touch the backing filesystem

Directories are a VFS fiction. `Vfs` keeps `created_dirs: hashbrown::HashSet<String>` and that set is the whole of what `mkdir` and `rmdir` persist. `vfs::FileSystem` (`kernel/src/vfs.rs`) has no `create_dir` and no `remove_dir` member, so neither call can reach a mount even by intent — `kernel/src/fat32_adapter.rs` says as much: "the VFS has no per-mount `mkdir`, so `create` is the only notice this mount gets." `Vfs::remove_dir` reads a mount's listing to answer the real outcome, but it mutates only `created_dirs`; no `rmdir` erases anything on the device.

The driver underneath is complete and unused. `toyos-fat32/src/fs.rs` `remove_dir` returns `NotFound` for an absent name, `NotADirectory` for a file, and `DirectoryNotEmpty` from a real `dir_is_empty` check before it erases entries and frees the cluster chain; `create_dir`/`create_dir_all` build real FAT32 directories. The kernel reaches `create_dir_all` only as a side effect of creating a file, through `ensure_parent` in `kernel/src/fat32_adapter.rs`; it never calls the driver's `create_dir`/`remove_dir` directly.

**Real on-disk directories leak.** On the writable `/log` FAT32 mount, `ensure_parent` writes genuine FAT32 directories when a file is created under one. Deleting that file (`Vfs::delete` → `delete_file`, files only) leaves the directory entry and its cluster chain in place; because the directory is now empty, `toyos-fat32`'s `walk` emits only files and so it is invisible to `Vfs::list`. Nothing in ToyOS can see or reclaim it — one cluster leaks per create/remove cycle, visible only to an outside FAT32 reader — and `sys_rmdir` refuses it (`NotFound`, since the VFS has no view of an empty on-disk directory) rather than reaching the mount to free it.

Precondition: none beyond naming a path. The filesystem is the declared ambient exception, so any unprivileged process with no handle reproduces it.

Repro. (b) Create `/log/d/f.txt`, delete the file, then `rmdir("/log/d")` returns `NotFound` while the directory entry and its cluster chain are still on the partition — a leak no in-tree code can see or reclaim. (d) `mkdir("/home/newdir")` succeeds and survives `cd` and `list` this boot, then is gone after a reboot — `created_dirs` is in-memory and nothing writes it anywhere.

Fix direction: give `vfs::FileSystem` `create_dir` and `remove_dir` members with real `Result`s and forward `Vfs::create_dir`/`remove_dir` through `resolve_fs` to the mount, so `FatFs` delegates to the driver's existing `create_dir`/`remove_dir` and carries `AlreadyExists`, `NotFound`, `NotADirectory` and `DirectoryNotEmpty` back out. `created_dirs` then shrinks to a per-mount fallback for mounts that genuinely cannot represent an empty directory (the tmpfs and bcachefs adapters), rather than being the only representation for every mount — which also closes the neighbouring visibility defect in which `walk` cannot see an empty directory. The `remove_dir` half is the ordering to be careful about: the emptiness check and the erase must run under the one VFS guard `resolve_for_modify` already holds.
