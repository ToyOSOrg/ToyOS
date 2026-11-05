---
status: open
kind: finding
opened: 2026-11-02
---

# A DATA commit flushes the drive twice

`Fs::sync` (`lib/bcachefs/src/fs.rs`) commits in three steps:
1. a Flush, so that every block the new superblock names is on the medium;
2. the superblock write;
3. a second Flush, so that the superblock itself is.

One `fsync` of a file on `/home` therefore costs two Flush commands. It costs
the whole volume's sync with them
(`issues/an-fsync-of-a-served-file-syncs-its-whole-volume.md`).

On the T14 (evidence `pr-1131` r7) the second Flush costs 0.36-0.43 ms warm.
After an idle wake from PS4 it costs 2.25-2.5 ms. That, with about 1 ms more of
host time between commands, is ToyOS's share of the first durable write after
idle over Linux's. The drive's wake itself, the first write and the first
Flush at about 13 ms, matches Linux's 12 ms. Whether Linux's ext4 issues one
Flush there, or writes its commit with Force Unit Access, is not measured.

A superblock written with Force Unit Access (NVM Express NVM Command Set,
Write's FUA bit) would make step 3 unnecessary on a drive that honours it. The
block service has no such request today.

**Owner**: the DATA volume's commit (`lib/bcachefs`) and the block service's
write request (`lib/blockserve`).
