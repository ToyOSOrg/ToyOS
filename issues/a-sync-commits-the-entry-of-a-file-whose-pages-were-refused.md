---
status: open
kind: defect
opened: 2026-11-01
---

# A sync commits the entry of a file whose pages the volume refused

`Files::sync` (`system/fileserver/src/pages.rs`) writes every dirty file's
pages back and then calls the volume's `sync` even where a file's write-back
was refused. DATA's `sync` writes every open file's entry, that file's
among them, so the commit makes durable an entry whose new extents name
blocks that never received its pages. In the same boot the file still reads
right: its pages stay dirty in its object and the next sync tries them
again. After a stop before that, it reads what those blocks last held,
which may be a deleted file's bytes
(`issues/a-reallocated-extent-on-data-keeps-the-deleted-files-bytes.md`).

An fsync does not: a carried file whose pages are refused refuses the
commit (`Files::fsync`).

Found by reading, while making an fsync commit one file; no test drives a
refused write-back into a stop.

**Exit**: a sync's commit carries no entry of a file whose pages were
refused, with a host test that refuses one file's write-back, stops the
volume after the sync and reads that file as its last commit left it.

**Owner**: the storage track (`issues/storage-is-layers-and-a-role-is-a-filesystem.md`).
