---
status: open
kind: defect
opened: 2026-08-28
---

# `FatFs::open_file` takes the file-cache reference before the fallible backing lookup, so a refused reopen pins the file for the rest of the boot

# `FatFs::open_file` takes the file-cache reference before the fallible backing lookup

`kernel/src/fat32_adapter.rs:617-621` — the reopen arm:

```rust
if let Some(&file_id) = self.by_name.get(name) {
    file_cache::open(file_id);
    return Ok((file_id, Some(self.backing(name)?)));
}
```

`file_cache::open` has already taken the reference when `backing` is asked, and
the `?` returns without giving it back. The fresh-open arm three lines below
gets the order right — `let backing = self.backing(name)?;` at
`fat32_adapter.rs:625` runs *before* `file_cache::create_file` at `:627` — and
so does the read-only initrd adapter (`kernel/src/bcachefs_adapter.rs:311-317`,
fallible `file_extents` first). Only this arm is inverted, and no comment at
the site says it is meant to be.

`backing` is genuinely fallible: `self.fs.metadata(name)` and
`self.fs.extents(name, MAX_EXTENTS)` at `fat32_adapter.rs:548-552` return
`NotFound`, `Io`, `CorruptChain`, `CorruptDirectory`, `BudgetExpired` or
`LimitExceeded`. The adapter's own `update_metadata` already treats a failing
`backing` as survivable at `fat32_adapter.rs:748-751` — *"a failure here only
costs evictability"* — which is the handling this arm does not have.

## Nothing gives the reference back

`file_cache::open` increments unconditionally (`kernel/src/file_cache.rs:111-116`).
The only decrement is `release_to_writeback` (`file_cache.rs:130-142`), whose
only caller in the tree is `Drop for OpenFileState`
(`kernel/src/object/file.rs:20-26`) — and no `OpenFileState` is ever built on
this path: `vfs::open_file` propagates the adapter's `Err` with `?`
(`kernel/src/vfs.rs:303-308`, `:326`) before `set_backing`, and
`object::ops::open` constructs the object only at `ops.rs:118`, after the
`Ok(v)` arm at `:115`; the `Err` arm at `:116` returns the code to userland with
no handle and no cleanup.

So the count is permanently one above the number of live handles. That is not a
deferred release — it never arrives:

- `finish_writeback` refuses to drop while `ref_count != 0`
  (`file_cache.rs:181-184`), and it is only reached from
  `writeback::drain_one` (`kernel/src/writeback.rs:137-140`), whose queue is fed
  solely by `OpenFileState::drop` on `TeardownOwed`. There is no periodic drain.
- `FatFs::close_file` (`fat32_adapter.rs:659-663`) — the one place `open` and
  `by_name` are cleared — hangs off `vfs::close_file` (`vfs.rs:389-395`) at
  `writeback.rs:138`, so the adapter's `OpenFile` (a `String` and a
  `toyos_fat32::File`) and its `by_name` entry are pinned too.
- The pinned file's dirty pages are never written back and never evictable
  (`evict_one` skips dirty pages, `file_cache.rs:510-512`), so each leaked file
  also holds non-evictable kernel heap and silently loses its unflushed bytes
  unless userland calls `SYS_FSYNC`. This is exactly the budget's one declared
  escape — eviction never takes a dirty page, the turnover line reports its
  dirty count, and `cache_eviction` stages and bounds it — given a
  userland-driven source that never flushes.
- `file.ref_count += 1` is not saturating, unlike the `saturating_sub` at
  `file_cache.rs:133`, and the kernel ships with `overflow-checks = true`
  (`kernel/Cargo.toml:343-348`, enforced by `src/build.rs`'s
  `assert_overflow_checked`). 2^32 repeats of one failing reopen panic the
  kernel from userland. Impractical in wall-clock terms; still the wrong shape.

## Reaching it from an unprivileged process

`/log` is `UserAccess::ReadWrite` by ruling (`kernel/src/main.rs:438-441`), and
the comment at `main.rs:433` states the assumption this breaks: *"the worst a
process can do is cost the diagnostic."*

`FatFs::rename` re-keys `by_name` only for the renamed name itself
(`fat32_adapter.rs:688-693`), and `toyos-fat32` renames directories
(`toyos-fat32/src/fs.rs:844-867`, the `moved_dir` arm at `:853`). Renaming a
directory therefore leaves every open file beneath it keyed under a path that
no longer resolves:

1. `open("/log/d/f", CREATE|WRITE)` — `ops.rs:105-107` falls to
   `vfs.create_file`, `FatFs::create` (`fat32_adapter.rs:634-657`) makes `d` via
   `ensure_parent` and sets `by_name["d/f"]` with `ref_count = 1`. Keep the handle.
2. `SYS_RENAME("/log/d", "/log/e")` — `kernel/src/arch/syscall/fs.rs:121-136`
   passes both paths on the ReadWrite mount; `by_name.remove("d")` at
   `fat32_adapter.rs:688` misses, because the key is `"d/f"`.
3. `open("/log/d/f", 0)` — read-only, so `sys_open` runs no permission check at
   all (`fs.rs:14-19`, `:44-53`). The by_name arm hits, `:619` increments,
   `backing` fails `NotFound`, and `refused` (`fat32_adapter.rs:527-533`)
   deliberately skips logging `NotFound`, so the leak is silent. Every repeat
   leaks again.
4. Close the handle: `2 -> 1`, `StillHeld`, pinned for the boot.

Repeating the sequence under fresh names leaks without bound, and it is not
bounded by disk either: `FatFs::delete` on the *new* path (`/log/e/f`) frees
the clusters while `by_name["d/f"]` survives, because `delete` keys its removal
on the name it was given (`fat32_adapter.rs:665-676`).

Two triggers reach the same line without any rename: a transient device error
or `BudgetExpired` inside `metadata`/`extents` on an otherwise legitimate
reopen, and a corrupt chain (`toyos-fat32/src/fs.rs:684`, `:697`) on an
untrusted volume. Fragmenting a file past `MAX_EXTENTS = 65_536`
(`fat32_adapter.rs:72`) is the *least* reachable of the set on the shipped
image: the log volume is `FAT32_MIN_BYTES` = 34 MiB with ~68,551 512-byte
clusters (`src/image.rs:176-182`, `:381-391`) and the flush path allocates a
whole 4 KiB page at a time (`vfs.rs:364-372` into `fat32_adapter.rs:711`), so
the reachable run count is ~8,500.

## Fix direction

Take the reference last: derive the backing first and call `file_cache::open`
only once it is in hand, matching `fat32_adapter.rs:625-627` and
`bcachefs_adapter.rs:311-317`. That is a two-line reorder, and it is not the
whole fix — the same "increment, then fail" shape exists at two more sites and
a fix that leaves them is incomplete:

- `kernel/src/bcachefs_adapter.rs:148-157` increments at `:150` and then
  `self.open_files.get(&file_id).ok_or(SyscallError::NotFound)?` at `:151`.
- `kernel/src/object/ops.rs:99-104`: `vfs.open_file` has already taken a
  reference (or created the file) when `vfs.file_mtime(path)` is asked, and a
  refusal there leaks the whole open — including the `create_file` arm's
  brand-new entry, which then leaks a `CachedFile` no handle ever names.

The structural answer is that the reference should be owned by a guard that
releases on drop, so that no early return between `file_cache::open` and the
handle install can lose it — the same property `HandleEntry`'s drop gives the
object layer. Anything less leaves the invariant stated nowhere and enforced by
reading order.

A fix needs a negative control: with the reorder reverted, a test that creates
a file under a directory, renames the directory, reopens the stale path N times
and reads the file-cache census must show the entry pinned and the count rising;
`fat-boot-reads-fail` (`kernel/src/actuator.rs:229-230`) stages the same
`backing` failure without the rename, on `/boot`, for an independent arm.
