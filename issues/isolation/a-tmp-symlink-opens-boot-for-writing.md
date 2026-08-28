---
status: open
kind: defect
opened: 2026-08-28
---

# `sys_open` checks `user_may_modify` before `open_file` follows the link, so a `/tmp` symlink opens `/boot` for writing

`/boot` is mounted `UserAccess::KernelOnly` (`kernel/src/main.rs:435`) because "a
writable `/boot` lets a process brick the machine — esp_files replayed exactly
that attack" (`main.rs:433`), and the enum says what that buys: "Readable, and
every userland syscall that would change it is refused" (`kernel/src/vfs.rs:74`).
An ordinary process reaches through that guard with two syscalls.

## The mechanism

`sys_open` (`kernel/src/arch/syscall/fs.rs:44-53`) resolves and checks under one
`vfs::lock()` and then **opens under a different one**:

    let resolved = {
        let vfs = vfs::lock();
        match resolve_and_check(&vfs, &cwd, path, open_modifies(flags)) { … }
    };                                              // guard dropped at :52
    process::with_process_data(|data| ops::open(&mut data.handles, &resolved, flags))

`resolve_and_check` (`fs.rs:23-34`) checks `vfs.user_may_modify(&resolved)`, and
`resolved` came from `vfs.resolve_absolute` (`vfs.rs:162-170`) — pure `normalize`,
no link following. `user_may_modify` (`vfs.rs:142-145`) is one mount-name lookup:
`self.mounts.get(&mount).is_none_or(|m| m.access == UserAccess::ReadWrite)`. So
the mount that is checked is the mount the *symlink itself* lives on.

`ops::open` (`kernel/src/object/ops.rs:71-133`) then takes a fresh
`crate::vfs::lock()` (`:77`) and calls `vfs.open_file(path)` (`:102`), which is
`Vfs::open_file`/`open_file_depth` (`vfs.rs:303-326`). That one **does** follow
`fs.read_link` — recursively, `depth <= 10`, rebuilding the path as
`format!("/{}/{}", mount, target)` and recursing. `normalize` (`vfs.rs:107-121`)
pops on `".."`, so a target of `../boot/toyos/kernel.elf` on `/tmp` becomes
`/boot/toyos/kernel.elf`. The recursion's `resolve_fs` (`vfs.rs:147-160`) looks
the mount up **by name only and never reads `Mount.access`** (`vfs.rs:78-81`).
There is no second `user_may_modify` on this path, under any interleaving.

The link is free to say anything: `sys_symlink` (`fs.rs:158-169`) checks the
*link* path with `resolve_for_modify` and passes `target` through untouched to
`Vfs::create_symlink` (`vfs.rs:461-469`) and `TmpFs::create_symlink`
(`kernel/src/tmpfs.rs:125-128`), a bare `symlinks.insert(name, target)`.
`SYS_SYMLINK` carries no gate (`kernel/src/arch/syscall/dispatch.rs:398-402`).

Downstream nothing re-validates. `ops::open` keeps `Rights::WRITE` because
`writable` came from the caller's flags (`ops.rs:124-128`); `sys_write`
(`kernel/src/arch/syscall/io.rs:47-54`) demands only that right; `ops::try_write`
(`ops.rs:375-408`) calls `file_cache::write_page(state.file_id, …)`, which is
purely FileId-keyed (`kernel/src/file_cache.rs:253-321`). `ftruncate`
(`dispatch.rs:321` → `ops.rs:560`) rides the same handle.

## Impact

Two levels, both live.

**The shared cache, immediately.** `FileSystem::open_file`'s contract is "the
same `FileId` on every open of the same file" (`vfs.rs:44`), so the write lands
in the canonical file-cache pages for `/boot/toyos/kernel.elf` and every reader
of that path sees the attacker's bytes. `KernelOnly`'s stated contract is broken
at `vfs.rs:74` with no flush involved.

**The physical ESP, durably.** The write handle records the *symlink* path
(`ops.rs:118-119`, `kernel/src/object/file.rs:13`), so its own teardown flushes
through the tmp mount, where `TmpFs::write_page` is a no-op (`tmpfs.rs:111-113`).
That is an identity confound in the writeback plumbing, not a defense: hold a
second handle on the same file and the flush goes out under the *boot* path.
`file_cache::release_to_writeback` (`file_cache.rs:130-142`) returns `StillHeld`
while another handle remains, so nothing is flushed and no dirty bit is cleared;
when the `/boot`-path handle is the last to close, `OpenFileState::drop`
(`file.rs:20-25`) enqueues `("/boot/toyos/kernel.elf", file_id)` and
`writeback::drain_one` (`kernel/src/writeback.rs:104-134`, flush at `:115`)
calls `Vfs::flush_taken` (`vfs.rs:352-386`), which resolves to the boot FAT32
adapter and calls `write_page` (`kernel/src/fat32_adapter.rs:702-715`). The
kernel image on the boot partition is overwritten — the identical outcome to the
incident the guard was built for.

## Reproduction

Any unprivileged process; no handle, no capability. `/boot` must be mounted,
i.e. a USB boot (`issues/boot-media/boot-exists-only-on-a-usb-boot.md`).

    symlink(b"../boot/toyos/kernel.elf", b"/tmp/evil")   // accepted: /tmp is ReadWrite (main.rs:427)
    let h = open("/tmp/evil", OpenFlags::WRITE)          // accepted: the check saw /tmp
    write(h, b"TEETH")                                   // lands in /boot/toyos/kernel.elf's pages

For the durable form, `open("/boot/toyos/kernel.elf", READ)` first — a read-only
open never consults `user_may_modify`, since `open_modifies` (`fs.rs:14-19`)
excludes plain `READ` — and close it *after* the write handle.

`CREATE`/`TRUNCATE` do not escape: they take the `vfs.delete(path)` branch
(`ops.rs:91-96`), which resolves without following links and unlinks the tmpfs
symlink. Plain `WRITE` on an existing target is the hole.

## Why nothing caught it

`tests/toyos-rust-tests/src/bin/esp_files.rs:87-113` aims every mutating syscall
*directly at* `/boot` paths — its symlink case is
`symlink(b"/boot/toyos/kernel.elf", b"/boot/toyos/link")`, i.e. the **link** on
`/boot`, which is correctly refused. No test puts the link on a writable mount
and the target on `/boot`.

`issues/kernel/the-capability-end-state-is-twelve-answers.md` currently records
the opposite of the truth here — "`/boot`'s mount guard is the one restriction
the ambient space carries" — and should be corrected when this closes.

## Fix direction

The demand check must be evaluated against the path `Vfs::open_file` actually
opens, not the one the caller typed. The shape that makes it unrepresentable is
to make resolution return the post-`read_link` path: give `Vfs` one
link-resolving `resolve_for_open(path) -> String` that `open_file_depth`,
`file_mtime_depth` and `open_backing_depth` all share, run `user_may_modify` on
its result, and do it under the single `vfs::lock()` that then performs the open
— `resolve_for_modify` (`fs.rs:37-42`) already demonstrates the held-guard shape
that `sys_open` alone abandons. Refusing a resolution that changes mount would
be a narrower patch but leaves `open_backing_depth` and `file_mtime_depth`
crossing unchecked.

Worth checking in the same pass, not assumed safe: `SYS_SPAWN` and `SYS_DLOPEN`
resolve paths ambiently too, and this entry did not trace whether they follow
links after any check of their own.

Two checks a fix PR owes. Negative control: extend
`boot_refuses_every_way_of_changing_it` with the same assertions reached through
a `/tmp` symlink whose target is `/boot/toyos/kernel.elf`, plus the two-handle
close ordering, and confirm they fail on the base and pass on the fix. Independent
oracle: `esp_files.rs`'s own module doc (`:8-17`) already encodes the intended
contract from a real prior incident and the host half in
`tests/common/volumes.rs` judges the delivered image with `toyos-fat32-check`,
so the differential is "the same refusal and the same byte-for-byte image,
reached one hop later" rather than a new specification.
