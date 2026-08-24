---
status: open
kind: defect
opened: 2026-08-23
---

# A spawn reads round an open file's dirty pages

Found while fixing the write-back queue's half of this, and older than that
queue.

`Vfs::open_backing` hands out a *device* view — the extent list and the length
a filesystem has recorded — and `loader::spawn`, `load_needed_libs` and
`sys_dlopen` are its callers. It now settles the write-back queue first
(`kernel/src/vfs.rs`), so a file that was **closed** is on the device by the
time a backing is derived. A file that is still **open** and dirty is on no
queue at all: its bytes are in the file cache, its `dirty_meta` is set, and
nothing on the spawn path looks at either. A process that writes a program,
keeps the handle, and spawns the path reads the pre-write metadata — an empty
file for one created in this boot, and the *old* bytes for one overwritten in
place.

Handle I/O is coherent across the same window, because `Vfs::open_file` returns
the file's one `FileId` and reads the cached pages. So the two readers of one
file disagree, and which one a caller gets is decided by which syscall it used.

Two shapes of answer, and neither is the drain:

- flush a file the cache says owes one (`file_cache::dirty_meta`) before
  deriving a backing, which needs the mount to be able to name the `FileId` for
  a path — there is no `FileSystem` accessor for that today; or
- serve the backing *from* the file cache when the cache holds the file, which
  is the end state: one authority for a file's bytes rather than two readers
  that can disagree. It costs a `FileBacking` that reads through the cache from
  the page-fault path, and the pinning that makes that sound.

Nothing gates it. `writeback_spawn` holds the *closed* case.
