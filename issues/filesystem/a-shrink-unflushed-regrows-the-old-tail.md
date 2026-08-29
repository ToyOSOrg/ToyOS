---
status: open
kind: defect
opened: 2026-08-29
---

# A shrink with no flush before the regrow serves the old tail through the backing

The flush-time extent trim (`BcacheFsAdapter::update_metadata`,
`FatFs::update_metadata`) closes the shrink that was fsynced: the shortened
record goes to the device and the dropped blocks are freed. What it cannot see
is a shrink that no flush observed. `ftruncate(fd, small)` then
`ftruncate(fd, big)` with no `fsync` between leaves `update_metadata` a single
call carrying only the final size, so nothing trims: on /home and /log a read
of a dropped page — same open, or after a close whose drain flushed only the
final size — goes to the backing, whose shared extent cell still names the old
blocks, and serves the discarded bytes where POSIX requires zeros. /tmp is
correct (no backing). The straddled page is correct everywhere
(`file_cache::set_size_locked` zeroes it at the shrink).

`tests/toyos-rust-tests/src/bin/fs_transactional.rs` holds the fsynced arms
green (`shrink_then_regrow_reads_zeros(_, 3 * PAGE, true)` on /home and /log);
no test drives the unflushed multi-page shape on a device mount.

Fix direction: the shrink's fact has to be captured when it happens, not
re-derived at flush. Either `file_cache` keeps a per-file low-water mark since
the last settled metadata write, passed to `FileSystem::update_metadata` so the
mount trims to the mark before applying the final size — rewritten pages above
the mark are dirty and flushed in the same call, so the order is trim, then
write; or the shared extent cell (`FileBlocks`, `FatExtents`) is truncated at
`file_cache::resize` time with the dropped runs parked on the cell until the
next successful metadata write frees them. On FAT the regrow arm is already
sound — `set_len` zero-fills what it grows — so only the cell's staleness needs
the mark there.
