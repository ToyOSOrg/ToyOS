---
status: assigned
kind: defect
opened: 2026-08-23
---

# A spawn of a just-written file reads the device the write-back queue has not written yet

Assigned to `wt/toyos-spawnwb`.

Write, close, spawn — the sequence a compiler that writes a binary and runs it
performs — refuses the spawn:

```
[kernel 0.370 cpu1] spawn: /home/disk_backtrace/child: ELF: fewer bytes than a file header
```

`tests/toyos-rust-tests/src/bin/disk_backtrace.rs` copies 1,659,240 bytes to
`/home/disk_backtrace/child`, prints `copied … bytes`, and spawns it; the
kernel answers `TooSmall` from `toyos_elf` for a file it has just been told is
1.6 MB. Reproduced locally on the first `cargo test disk_backtrace`
(2026-08-23), and twice in hosted CI run 32665435138 shard `guest (1)`.

**The read that sees stale state.** `loader::spawn` takes a *device* view:
`vfs::lock().open_backing(path)` (`kernel/src/loader/mod.rs:447`), which reaches
`BcacheFsAdapter::open_backing` and asks the btree —
`self.fs.file_extents(name)` — for the file's extents and length. The file
cache is not consulted at any point on that path; `Vfs::open_backing`'s own
doc-comment says so ("separate from handle-based I/O and doesn't use the file
cache"). Since the write-back queue landed (#257) the last close no longer
flushes: it pins the file and enqueues it for `iod`, so between the close and
the drain the btree still holds what `create` wrote — no extents and length 0.
`backing.file_size()` is then 0, `header_size = 4096.min(0)` is 0, and
`elf::parse_layout(&[])` is `Error::TooSmall`.

Handle I/O is coherent across the same window — `Vfs::open_file` returns the
same `FileId` and reads the pinned pages — so the hole is exactly the
backing-derived paths: `loader::spawn`, `load_needed_libs`, and `sys_dlopen`.

**Not a test bug.** Making the test wait for the drain would hide the defect
the drain rule in `tests/CLAUDE.md` covers only for a test reading the *backing
device* deliberately. A spawn is the ordinary path.

**A second, older hole this does not cover.** A file that is still *open* and
dirty has no queue entry at all, and a spawn of it reads the same stale btree.
That predates #257 and wants its own answer.
