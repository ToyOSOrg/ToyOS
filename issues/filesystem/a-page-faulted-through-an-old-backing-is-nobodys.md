---
status: open
kind: defect
opened: 2026-09-01
---

# A page faulted through a backing taken earlier answers from that backing and not from the file cache

`kernel/src/process.rs`'s demand-paging fault reads `FileBacking::read_page`
directly; `kernel/src/loader/mod.rs` and `kernel/src/elf/mod.rs` do the same
for a spawn's segments and headers. None of them goes through
`file_cache::read_page`, so none of them sees what the file cache knows about
the file: not the pages a writer has dirtied, and not `CachedFile::shrunk_to`,
the mark that makes a shrunk tail read as zeros.

So an mmap or a spawn whose backing was derived before a write or a shrink
keeps answering from the extents that backing holds. The `read`/`write`
syscalls and the fault path are two readers of one file that can disagree, and
which one a caller gets is decided by how it opened the bytes.

Two of the three windows that produced this are closed at their own sites:
`Vfs::open_backing` now settles the write-back queue *and* flushes a file the
cache says owes one, so a backing is derived from a file that is on the device.
What is left is the backing that was already handed out. Naming what does cover
part of that, so the gap is the real one: a backing whose blocks are being
reused or freed *is* revoked — `FatFs::revoke` from `delete` and from a
rename's displaced destination (`kernel/src/fat32_adapter.rs:787`, `:684`), and
`BcacheFsAdapter::revoke` from `create`, `delete`, `create_symlink` and rename
(`kernel/src/bcachefs_adapter.rs:269`, `:295`, `:365`, `:209`). A shrink that
reached the device is re-derived too: `FatFs::truncate_to` and
`update_metadata` call `FatExtents::truncate_to`, and the bcachefs adapter's
`truncate_to` shortens its own block cell. Every one of those is keyed to the
mount's blocks changing hands. None of them is keyed to the file cache holding
something the device does not, which is the whole of what is left: a page a
writer dirtied and no flush has carried, and a `shrunk_to` mark before the
flush that trims it.

The end of it is one authority for a file's bytes: a `FileBacking` that reads
through the file cache from the fault path, and the pinning that makes that
sound. It is not a small change — the fault path takes no VFS lock today and
the cache's miss path drops its own — which is why the window at
`Vfs::open_backing` was closed where it stood instead.
