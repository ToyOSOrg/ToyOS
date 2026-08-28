---
status: open
kind: defect
opened: 2026-08-28
---

# `sys_rmdir` returns 0 without touching any mount, so a directory reported as removed is still on disk

Directories are a VFS fiction. `Vfs` keeps `created_dirs: hashbrown::HashSet<String>` (`kernel/src/vfs.rs:87`, empty at `:129`) and that set is the whole of what `mkdir` and `rmdir` touch. `vfs::FileSystem` (`kernel/src/vfs.rs:33-66`) has no `create_dir` and no `remove_dir` member, so neither call can reach a mount even by intent — `fat32_adapter.rs:587-588` says as much: "the VFS has no per-mount `mkdir`, so `create` is the only notice this mount gets."

The chain on current main:

- `kernel/src/arch/syscall/dispatch.rs:297-300` dispatches `SYS_RMDIR` to `sys_rmdir`.
- `kernel/src/arch/syscall/fs.rs:149-155`: `resolve_for_modify(path)?`, then `vfs.remove_dir(&resolved); 0`. There is no result to check — `Vfs::remove_dir` returns unit — so 0 is returned unconditionally.
- `kernel/src/arch/syscall/fs.rs:37-42` → `:23-33`: the only gate is `vfs.user_may_modify`, and `kernel/src/vfs.rs:143-146` passes everything but a `KernelOnly` mount. Per `kernel/src/main.rs:421-441`, `home`, `tmp` and `log` are all `UserAccess::ReadWrite`; only `boot` is refused.
- `kernel/src/vfs.rs:455-459`: `created_dirs.remove(path)`, then `retain(|d| !d.starts_with(&prefix))`. Nothing else.
- `kernel/src/vfs.rs:447-453`: `create_dir` is `created_dirs.insert(String::from(path))`, whose `Result` can only be `Ok` or `InvalidArgument` (path over `MAX_PATH`). `sys_mkdir` (`fs.rs:138-146`) does match on it, but there is no outcome to learn — never `AlreadyExists`, never `NotFound`, never persistence.

The driver underneath is complete and unused. `toyos-fat32/src/fs.rs:817-838` `remove_dir` returns `NotFound` for an absent name, `NotADirectory` for a file, and `DirectoryNotEmpty` from a real `dir_is_empty` check before it erases entries and frees the cluster chain. `grep -rn "remove_dir\|create_dir_all\|\.create_dir(" kernel/src/` shows the kernel never calls it; `create_dir_all` is reached only as a side effect of creating a file, via `ensure_parent` (`kernel/src/fat32_adapter.rs:589-592`, called at `:640`).

Impact, in three parts.

**The kernel lies to userland.** `rmdir` reports success for a directory that never existed, for one that is not empty, and for one that is still on disk afterwards. `toyos-abi/src/syscall.rs:1534-1537` documents it flatly as "Remove a directory" and `rust/library/std/src/sys/fs/toyos.rs:453-456` hands it to `std::fs::remove_dir`, so a caller has no way to learn otherwise. A program using the rmdir-fails-if-nonempty idiom as a guard gets the opposite of the truth.

**Real on-disk directories leak.** On the writable /log FAT32 mount, `ensure_parent` writes genuine FAT32 directories when a file is created under one. Deleting that file (`Vfs::delete` → `delete_file`, `kernel/src/vfs.rs:481-483`, files only) and then calling `rmdir` leaves the directory entry and its cluster chain in place while reporting success. Because the directory is now empty it is also invisible to `Vfs::list` (`kernel/src/vfs.rs:294-297`), so nothing in ToyOS can see or reclaim it — one cluster leaked per create/remove cycle, visible only to an outside FAT32 reader.

**One call erases kernel-established state for the rest of the boot.** `kernel/src/main.rs:447-448` creates `/home/root` and `/home/root/.config` as pure `created_dirs` entries, and nothing re-creates them. `rmdir("/home")` passes `user_may_modify` and the `retain` at `kernel/src/vfs.rs:458` drops both. `Vfs::cd` then misses at `:209`, falls through to the mount listing and returns `NotFound` at `:226` while nothing is stored under `root/`, so `userland/shell/src/main.rs:32-33`'s `set_current_dir` fails under a discarded `let _` and every shell spawned afterwards starts at `/`. `userland/init/src/main.rs:466` records that an unreachable cwd is enough to fail a spawn outright.

Precondition: none beyond naming a path. The filesystem is the declared ambient exception, so any unprivileged process with no handle reproduces all of it.

Repro. (a) `std::fs::remove_dir("/tmp/never-existed")` returns `Ok`. (b) Create `/log/d/f.txt`, delete the file, `rmdir("/log/d")` returns `Ok`, and the directory is still on the partition. (c) `rmdir("/home")`, then spawn a shell and read its prompt. (d) `mkdir("/home/newdir")` succeeds and survives `cd` and `list` this boot, then is gone after a reboot — `created_dirs` is in-memory and nothing writes it anywhere.

Fix direction: give `vfs::FileSystem` `create_dir` and `remove_dir` members with real `Result`s and forward `Vfs::create_dir`/`remove_dir` through `resolve_fs` to the mount, so `FatFs` delegates to the driver's existing `create_dir`/`remove_dir` and carries `AlreadyExists`, `NotFound`, `NotADirectory` and `DirectoryNotEmpty` back out. `sys_rmdir` then matches on that result the way `sys_mkdir` already matches on `create_dir`'s. `created_dirs` should shrink to a per-mount fallback for mounts that genuinely cannot represent an empty directory (the tmpfs and bcachefs adapters), rather than being the only representation for every mount — which also removes the descendant-wiping `retain` and closes the neighbouring visibility defect in `walk` cannot see an empty directory. The `remove_dir` half is the ordering to be careful about: the emptiness check and the erase must run under the one VFS guard `resolve_for_modify` already holds.
