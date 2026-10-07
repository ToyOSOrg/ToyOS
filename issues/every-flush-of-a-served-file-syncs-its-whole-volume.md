---
status: open
kind: defect
opened: 2026-09-27
---

# Every flush of a served file syncs its whole volume

std's ToyOS `File::flush` is `File::fsync`
(`sdk/std/sys/fs.rs`), where every other platform's is a
no-op, and fsd answers `FSYNC` by syncing the volume, every open file's entry
and every dirty block of the cache (`userland/fsd/src/main.rs`, `FSYNC`). So a
`BufWriter` over a file on `/home` syncs all of DATA each time it flushes, and a
4 KiB create and fsync cost 3.5–3.6 ms p50 on QEMU TCG against the kernel's
1.0–1.3 ms, measured on the branch that moved DATA to fsd.

**Exit**: `File::flush` hands the server what is buffered and makes nothing
durable, as std's contract for `Write::flush` is, and `FSYNC` makes durable the
file it names and what that file's entry depends on, with a guest measurement
of a `BufWriter` flush beside an fsync.
