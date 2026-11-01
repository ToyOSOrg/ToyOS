---
status: open
kind: defect
opened: 2026-10-23
---

# A read of ROOT verifies its whole extent every time

`RootVolume::read` (`system/fileserver/src/root.rs`) asks bcachefs's reader
for each client read, and each one looks up the inode, walks the extents btree
from its root and checks the CRC-32C of every extent it touches, all of it:
one extent covers up to 256 KiB. A client reading 4 KiB at a time re-verifies
one extent up to 64 times. The interim format's ROOT copied blocks unverified.

Measured on the host (release build, a 32 MiB file on a fitted volume, PR #1039's
evidence `tools/speed-test.rs`): 43.9 MB/s at 4 KiB reads, 278.9 MB/s at
64 KiB, 339.7 MB/s at 1 MiB. Every ROOT read pays it; no boot-time effect has
been measured.

A program or library load reads nothing: SYSTEM answers `fs::IMAGE` with ROOT's
own pages (`RootVolume::in_place`), verifying the file's extents once a boot,
at its first load, and keeping that verdict. That one pass is a byte-at-a-time
software CRC-32C over the whole binary on SYSTEM's single thread, during which
it serves no other client; its latency on a bench is not measured.

Owner: the storage track.

**Exit**: C2's node core serves SYSTEM through the bcachefs reader with its
own clean cache, so an extent verified once is served from the cache until it
is evicted, and the 4 KiB measurement above is repeated to show it.
