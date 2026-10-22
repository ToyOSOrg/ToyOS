---
status: open
kind: track
opened: 2026-10-22
---

# An unlinked open file answers Stale

A file the file server holds open answers `Stale` from the moment its name is
unlinked or renamed over (`system/fileserver/src/volume.rs`, the module
header), and a test asserts it. POSIX's file lives on until its last close or
unmap, and SQLite relies on that: it opens every temporary file and unlinks it
at once, and its last connection unlinks the `-shm` file while it is still
mapped. So any SQLite statement that spills to a temporary file fails today,
cargo's bundled one included.

**Ruled** (owner, 2026-10-21):
removing the last directory name prevents fresh opens through that name, and
existing handles and mappings keep the same file until their final release. A
new file made at the old path is a different file and never retargets them.
The file server owns the lifetime and the storage's reclaim; the kernel keeps
no filesystem identity or naming.

**Exit**: a file unlinked while open, and one renamed over, is read and written
through its open descriptor until the last goes, and its blocks are freed
then; a crash leaves the orphans for the next mount to reclaim; each asserted
by the file server's host volume tests, replacing today's `Stale` test
(`system/fileserver/src/data.rs`). A guest test reads a mapping of an
unlinked file once file mappings exist
(`issues/process-memory-in-4-kib-pages-still-copies-each-programs-text.md`).
