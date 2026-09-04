---
status: open
kind: defect
opened: 2026-09-03
---

# The shared-object cache's two refusals hold over less than the cache reaches

`kernel/src/elf/cache.rs` now refuses a path whose file changed and a load past
its byte budget, which closed the record that tracked the cache's lack of both.
Three things that record carried are not covered by either refusal, and they
went out of the tracker with it. They are one file because they are one
residual: what the refusals do **not** reach.

## The identity cannot see a same-size rewrite on a FAT32 mount

`vfs::BackingId` is size plus the mount's mtime. On `/home` (bcachefs) the mtime
is `nanos_since_boot` at the flush, so two writes are always apart —
`so_cache_policy`'s `stale-mtime` arm asserts exactly that.

`/log` is FAT32, mounted `UserAccess::ReadWrite` (`kernel/src/main.rs:461`), and
**FAT stores seconds in units of two**: `toyos-fat32/src/time.rs:5-15` states
the encoding's three lossy properties, `dir.rs:92` passes 0 for the tenths
field, and `kernel/src/fat32_adapter.rs:849` stamps whatever `now()` gives. So
two writes of the same length inside one 2-second bucket carry one mtime, and
the second load is served the first image — the staleness the refusal exists to
prevent, on the one writable FAT mount a process can reach.

**Mechanism read off the code; not reproduced.** Planting it needs a same-size
rewrite of a library inside 2 s on `/log`, and a 1.9 MB write to the
USB-backed log volume takes about 5.8 s, so the window closes before the second
write lands.

*Exit condition:* an identity that does not rest on a clock — a content hash, a
per-file generation the mount bumps on every write, or a `FileId` plus a write
counter — or a demonstration that no library can be reached on a FAT mount.

## The budget is a machine-wide, boot-permanent denial

256 MiB over every cached image, and nothing is ever evicted, so the bytes an
unprivileged process spends on distinct `dlopen` paths are gone for the boot and
the cache is closed to every later loader — including `/system/bin/init`'s. It is
bounded, unlike `no-physical-memory-fairness.md`'s unbounded case, and that is
the difference: the memory comes back at reboot and not before.

This was the ruling's choice, not an oversight — eviction of an image that is
mapped into every process that loaded it needs a lifetime rule nothing here has.

*Exit condition:* the Ring-3 loader move deletes this cache, or the lifetime
track gives a mapped image a rule that lets one be taken back.

## The key is the caller's spelling, so an alias walks past the refusal

`Vfs::resolve_absolute` (`kernel/src/vfs.rs:237`) normalises `.` and `..` and
resolves no symlink, so one file under two spellings is two entries and two
physical images. The base had the same key, so this is not a regression — but
the refusal is now **per spelling**, which is worse than the key alone: a
rewritten library refused under one name loads fresh under an alias, and the
double-map the refusal exists to prevent happens anyway to a caller that knows
the second name.

*Exit condition:* the cache is keyed by what the mount identifies the file as
(`FileId`) rather than by the string the caller wrote.
