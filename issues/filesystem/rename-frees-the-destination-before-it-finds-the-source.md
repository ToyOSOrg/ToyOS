---
status: open
kind: defect
opened: 2026-08-28
---

# `rename` frees the destination before it looks for the source, so a rename that returns `NotFound` has already destroyed the file it names — and `mv x .` loses x

Every `FileSystem::rename` in the kernel disposes of the destination first and
only then asks whether the source exists. When the source is absent — including
when it is the destination, because nothing refuses `old == new` — the
destination is already gone and the syscall returns `NotFound`, the one error
that tells the caller nothing happened.

## The chain

`sys_rename` checks only that the mount is writable and dispatches
(`kernel/src/arch/syscall/fs.rs:121-136`, via `resolve_and_check` at
`fs.rs:23-34`, reached from `kernel/src/arch/syscall/dispatch.rs:288-292`).
`Vfs::rename` checks mount equality and non-empty paths and nothing else
(`kernel/src/vfs.rs:428-443`). No layer above the filesystem checks that the
source exists, that the destination does not, or that the two differ.

**tmpfs — unrecoverable.** `kernel/src/tmpfs.rs:96-109`. Lines 97-99 do
`self.files.remove(new)` and `file_cache::mark_deleted(target_id)`
unconditionally; lines 100-107 then look for `old` and return `NotFound`.
`mark_deleted` drops the file outright when nothing holds it
(`kernel/src/file_cache.rs:421-431` → `drop_file` at `file_cache.rs:447-452`),
and an open handle only defers it (`file_cache.rs:424`, then
`file_cache.rs:185-186`). tmpfs has no second copy: `open_file` returns no
backing (`tmpfs.rs:66-70`), pages are non-evictable (`tmpfs.rs:73`), and
`TmpfsBacking::read_page` zero-fills a miss out of the same cache
(`tmpfs.rs:18-27`). The bytes are gone.

**FAT32 (`/log`) — unrecoverable on the device.**
`kernel/src/fat32_adapter.rs:684-687` calls `self.delete(new)`, whose
`self.fs.remove(name)` frees the clusters (`fat32_adapter.rs:668-676`), before
`self.fs.rename(old, new)` at line 687. `Fat32::rename` resolves the source
first and fails there (`toyos-fat32/src/fs.rs:844-846`).

**bcachefs (`/home`) — narrower.** `kernel/src/bcachefs_adapter.rs:214-221`
runs `mark_deleted`, `name_to_id.remove(new)` and `revoke(new)` before
`self.fs.rename(old, new)` at line 223. `bcachefs::Mounted::rename` fails at
`bcachefs/src/fs.rs:881-882` before touching the tree, so the on-disk entry
survives — but the destination's unflushed dirty pages are dropped, writeback
is disarmed for it (`file_cache.rs:159-160, 185`), and every open handle's
backing is revoked (`kernel/src/file_backing.rs:34-38`).

## Impact

The destruction is silent and the return value denies it. This is not an
authority defect — a process that can rename over `/tmp/victim` can already
`unlink` it (`arch/syscall/fs.rs:88-97`, same gate) — it is a correctness
defect that bites correct programs. `/bin/cp`'s whole safety argument is that
the bytes land on a sibling and are renamed onto the destination only when
complete (`userland/toybox/src/cp.rs:50-59, 73`); if that partial ever goes
missing, the rename destroys the destination cp was protecting and reports
failure.

## Repro

- `rename("/tmp/absent", "/tmp/victim")` → `NotFound`, and `/tmp/victim` is
  unrecoverable. Same on `/log`.
- `/bin/mv /tmp/a /tmp` → `cp::destination` (`userland/toybox/src/cp.rs:39-44`)
  joins the basename onto the directory, so the two paths are equal; mv's own
  `fs::metadata` pre-check passes (`userland/toybox/src/mv.rs:27-30`); the
  rename destroys `/tmp/a` and mv exits 1 saying the rename was refused.
- `/boot` cannot be reached (`kernel/src/main.rs:434-436`), nor the initrd root
  (`kernel/src/bcachefs_adapter.rs:346-348`). `/home`, `/tmp` and `/log` are all
  `UserAccess::ReadWrite` (`main.rs:421, 424, 427, 440`).

## Why no test sees it

`tests/toyos-rust-tests/src/bin/toybox_file_tools.rs:212-214` reads as coverage
("PASS mv refuses a missing source before it renames anything") but its
destination `/tmp/toybox_x.bin` does not exist, so it asserts only that the
refusal did not *create* it — and `/bin/mv`'s userland `metadata` pre-check
means the syscall is never issued. That pre-check is also racy: another process
can unlink the source between the `metadata` and the `rename`.
`tests/toyos-rust-tests/src/bin/fs_large_file.rs:55-60` checks the *source*
survives a rejected rename, again onto a name that does not exist. No test in
the tree renames onto a destination that exists and fails.

## Second defect at the same site: tmpfs's namespace is two maps

`TmpFs` holds `files` and `symlinks` separately (`kernel/src/tmpfs.rs:33-38`)
and nothing keeps them disjoint: `create` consults only `files`
(`tmpfs.rs:72-78`), `create_symlink` inserts unconditionally
(`tmpfs.rs:125-128`), and `rename` never touches `symlinks` for the destination
(`tmpfs.rs:97-105`). Meanwhile `Vfs::open_file_depth` reads `read_link` first
(`vfs.rs:318-325`) while `Vfs::create_file` goes straight to `fs.create`
(`vfs.rs:330-336`), and `ops::open` falls through to it on `NotFound`
(`kernel/src/object/ops.rs:105-108`). So `symlink(dangling, "/tmp/f")` followed
by `open("/tmp/f", CREATE)` lands one name in both maps: `readdir` lists the
file (`tmpfs.rs:47-53`), `open` follows the symlink and never reaches it, and
freeing its non-evictable pages takes two `delete` calls because `delete`
returns after the first map that hits (`tmpfs.rs:85-94`). bcachefs already
solves exactly this — `retire_displaced` (`bcachefs/src/fs.rs:776-790`), used by
`put` (759-773) and by `rename` (889-904), deletes the entry the insert did not
replace because a file and a symlink of the same name key differently. Split
this out if the fixer prefers; it is filed here because it is the same
invariant: a name has one entry.

## Fix direction

Establish the source before disturbing the destination. In each `rename`,
resolve or remove the source entry first and displace the destination only once
the move is certain to complete; where the backend can fail after that point
(FAT32's `insert_entry`/`erase_entries`, `toyos-fat32/src/fs.rs:860-861`), free
the displaced destination last rather than first. Decide `old == new` once, at
`Vfs::rename` (`kernel/src/vfs.rs:442-443`) where both normalized filesystem
paths are in hand, and make it the no-op success it is everywhere else rather
than a path that destroys and then reports `NotFound`. Give `TmpFs` one map
whose value is a file-or-symlink enum, so `create`, `create_symlink`, `rename`
and `delete` cannot each see a different namespace.

The negative control is a test that renames a missing source onto an existing
destination on each of `/tmp`, `/home` and `/log` and reads the destination's
bytes back — it fails on today's code and on any fix that only reorders one of
the three adapters. The independent oracle for the FAT32 arm is
`toyos-fat32-check` over the log volume afterwards, and for `rename(p, p)` it is
POSIX, which defines that call as success with nothing changed.
