---
status: open
kind: defect
opened: 2026-09-28
---

# The shared-object cache's identity is blind on an undated machine

`vfs::BackingId` is a file's size plus its mount's mtime, and on a machine whose
RTC never answered every write stamps 0 (`kernel/src/clock.rs`'s `mtime_now`).
So a same-size rewrite of a library on any writable mount carries the identity
it had, and `kernel/src/elf/cache.rs` serves the next load the first image —
the staleness its refusal exists to prevent, on every mount rather than only
FAT's two-second one.

**Mechanism read off the code; not reproduced.**

**Exit condition.** An identity that does not rest on a clock — a content hash,
a per-file generation the mount bumps on every write, or a `FileId` plus a
write counter — with a test that rewrites a library at the same size on the
`rtc-dead` machine and loads the second image.
