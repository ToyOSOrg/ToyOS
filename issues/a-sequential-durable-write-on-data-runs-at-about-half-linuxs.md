---
status: assigned
kind: defect
opened: 2026-11-11
---

# A sequential durable write on DATA runs at about half Linux's

**Owner.** The storage track: the DATA volume's write-back
(`system/fileserver/src/pages.rs`, `lib/bcachefs/src/engine/mount/data.rs`),
the block session (`system/diskserver/src/disk.rs`) and the volume's commit
(`lib/bcachefs/src/engine/mount/mod.rs`). Assigned to the task that opened
this record.

osbench's `persist.seq_write_fsync`, 8 MiB written in 1 MiB calls to a new
file and `sync_all`ed, on main f9b5d7f12 and the benches' Linux (ext4 on the
same NVMe) in one session, four boots and three Linux runs a bench
([evidence](https://github.com/ToyOSOrg/evidence/blob/7efbe1295aa54370b213719b4d4f460e7a103554/ab/README.md)):

| bench | ToyOS, each boot | Linux, each run |
|---|---|---|
| AMD | 274, 264, 243, 269 MB/s | 493, 502, 507 MB/s |
| T14 | 180, 258, 259, 198 MB/s (the first and last in its drive's slow mode) | 588, 582, 593 MB/s |

`fs.disk.seq_write.1048576`, 64 MiB with no sync, read 467 MB/s on the AMD
against Linux's 791, and 390 on the T14 against Linux's 347. The small
durable rows are ahead of Linux on the AMD (`write_4k_fsync` 2.36 ms against
3.58 ms), so the commit of a small change is not where the row is lost.

**What the device was asked, counted on the host model** (the file server's
DATA volume over a recording device, the row's own calls; counts, not
times). Before #1216 a round sent the 8 MiB as 128 writes of 64 KiB, one
extent each, each waited for before the next, though 127 of them followed
the one before on the device; then the commit wrote three journal entries
of 64 KiB between four Flushes. Each extent is a transaction of its own,
about 1.5 KiB of journal.

**#1216** sends a reserved write's bytes as one device write for each
stretch of contiguous sectors, its requests all in flight together: four
writes of 2 MiB a round, sixteen requests of 128 KiB each. The pointers and
the commit are as they were.

**Not yet established.** Where the round's time goes on the benches: the
share of the device round trips, the three entries' four Flushes, the 128
transactions, and the copies of the bytes (three on the way into the file's
pages, four on the way out since #1216). No bench has run #1216.

**Exit.** `persist.seq_write_fsync` and `fs.disk.seq_write.1048576` at or
above Linux's on both benches, in every boot of a back-to-back session, or
each remaining difference measured to its parts.
