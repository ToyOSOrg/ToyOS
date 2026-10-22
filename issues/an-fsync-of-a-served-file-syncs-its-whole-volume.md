---
status: open
kind: defect
opened: 2026-09-27
---

# An fsync of a served file syncs its whole volume

The file server answers `FSYNC` by syncing the volume, every open file's
entry and every dirty block of the cache (`system/fileserver/src/main.rs`,
`FSYNC`, which calls `sync` with the file's node and syncs the whole volume). So an
`fsync` or `File::sync_all` of one file on `/home` syncs all of DATA, and a
4 KiB create and fsync cost 3.5–3.6 ms p50 on QEMU TCG against the kernel's
1.0–1.3 ms, measured on the branch that moved DATA to the file server.

std's ToyOS `File::flush` is a no-op, as it is on every other platform: a
`File` buffers nothing, since each `write` is the server's when it answers
(`toyos::files`), so a flush has nothing to hand on and `Write::flush` asks
for no durability.

**Exit**: `FSYNC` makes durable the file it names and what that file's entry
depends on, with a guest measurement of one file's fsync beside the volume's.
