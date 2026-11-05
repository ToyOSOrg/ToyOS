---
status: open
kind: track
opened: 2026-09-03
---

# Storage is four layers, and a role names a filesystem rather than a partition

The stages that build the storage layers, block, volume, filesystem and path,
on the block services and file servers of the small-kernel track
(`issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`, stages 3
and 4). Two owner rulings bound it: block services and file servers are
userland programs and the VFS is a client library (2026-09-26), and the kernel
keeps no filesystem of ROOT (2026-10-21). NTFS is postponed behind every other
stage (owner).

## Stages, in order

The block services and the file servers are the small-kernel track's steps;
what is left here is built on them.

1. The users track, `issues/a-user-is-a-home-tree-and-a-login-row.md`.
2. Real bcachefs under ROOT and DATA — the format swap; nothing above changes.
   **Built** (#1152).
3. The installer, written together with the layout it lays down: GPT, ESP,
   ROOT, a designated DATA, one boot entry.
4. Updates: one ROOT partition per release, a second beside the first, the
   bootloader choosing — no snapshot logic in the kernel's read path.
5. Multi-device, replicas, tiering, snapshots, as the bcachefs crate grows into
   them.
6. A full secure boot chain, the end state of `/system`'s immutability:
   firmware verifies the bootloader, the bootloader every section of the slot
   it boots (built: the signed header binds the kernel, the two boot programs
   and ROOT), and a link that fails is refused by name.
7. NTFS read-only, postponed here by the owner.

**A file's pages are the kernel's paged objects'** (memory stage 7a, built by
this track): one per file, supplied by its file server, read by its holders
without a request (`system/fileserver/src/pages.rs`). The server's block cache
keeps the volumes' structures alone. Mapping them is memory stage 7b, the
process-memory track's
(`issues/process-memory-in-4-kib-pages-still-copies-each-programs-text.md`).

**A served file's metadata without a request**, in two stages:

- **3a, not built:** a held descriptor's `fstat` asks its file server, whose
  metadata is the authority. Answering it from the paged object instead
  (`SYS_PAGED_STAT`, #1144) was superseded in #1152; whether `fstat`'s round
  trip needs it is unmeasured.
- **3b, deferred until memory stage 7b's mappings:** a client cache of names,
  validated by a per-connection page the file server writes. The page carries
  generations of the directories in the connection's view and of the files
  it has answered about, so that `stat` of an existing file's path hits the
  cache; that row is what stands against Linux's 1.5 µs. Its slots carry
  identity and generation, and a client compares both under acquire. Server
  death clears the page through the paged object's `Gone`.
  - Why deferred: building it before mappings means reading slots by
    syscall, plus a names-only pager for SYSTEM and a dirty allowance for
    BOOT. That is an interim mechanism the mappings would replace.
  - Exit: `fsrtt.std.stat` and `open_close`, measured on both benches.

LOG stays its own FAT32 partition through the dev phase, because a Mac has to
read the stick; folding it into DATA in the installed product is deferred by
the owner's ruling of 2026-09-26, and no stage above owes it yet.
