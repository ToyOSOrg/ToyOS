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
stretch of contiguous sectors, its requests all in flight together, and
points up to eight fresh extents of one bucket in a transaction, as a
truncation removes up to eight. On the model a round is four writes of
2 MiB, sixteen requests of 128 KiB each, and its commit one journal entry
between two Flushes.

**Where a round's time went on main, on the benches** (2026-11-11, main
ad5db5a65 with stage timers, two boots a bench; a round is about 31 ms on
the AMD against Linux's 16):

- AMD: the create that truncates the last round's file 4.1 ms (Linux
  2.4); the eight writes into the file's pages 4.5 ms (Linux 5.5); the
  fsync 22.8 ms (Linux 8.0). Of the fsync: the 128 device round trips 9.4
  ms, of which the drive answered in 6.1; the extents' transactions,
  checksums and buffers 6.1 ms; the commit 5.1 ms, 4.5 of it five Flushes;
  reading the pages out 0.9 ms.
- T14, every boot in its drive's slow mode: the fsync 35 to 47 ms, of
  which the round trips 15 ms and the commit 12 to 23 ms, five Flushes
  of 2.3 to 3 ms and more. Linux's fsync on the same drive read 11.5 ms
  in the fast mode and 25 to 27 ms in the slow one.
- Linux sends the round as 66 writes of at most 128 KiB, the queue's
  limit on both drives, and one Flush.

With the device writes at depth alone (#1216's first commit) the AMD's
fsync fell to 19.5 ms and the T14's did not move: there the Flush after
the burst took what the round trips had, 5.6 to 6.6 ms against 3.3. The
bytes reach the drive faster than its slow mode writes them.

**With #1216** (its code at cc718f61f against main, 2026-11-11, four
boots an arm a bench, interleaved, and Linux three times in the same
session;
[evidence](https://github.com/ToyOSOrg/evidence/blob/b4d2fd4264f7e295702dade3a7bafe11ee30fd78/README.md)),
`persist.seq_write_fsync` and `fs.disk.seq_write.1048576`, MB/s, each
boot:

| bench | main | #1216 | Linux |
|---|---|---|---|
| AMD | 239 to 262; 452 to 460 | 375 to 409; 668 to 681 | 468 to 495; 800 to 806 |
| T14, slow mode | 156 to 182; 455 to 485 | 202 to 228; 563 to 573 | none this session |
| T14, fast mode | 245; 368 (one boot) | 349; 498 (one boot) | 579 to 609; 344 to 349 |

Where a round's time goes with #1216 (one boot a bench with stage timers):

- AMD: the fsync about 14 ms against Linux's 9 to 10. Writing the 8 MiB
  back is 9.7 ms: 5.3 ms in the device's call, with up to 15 commands
  held and 1.2 ms of it copying the blocks for the session, and 4 ms of
  the extents' lookups, building and checksums before it and their
  pointer transactions after it, none of it beside the device's time.
  The commit is 2.2 to 2.6 ms, two Flushes. Reading the file's pages out
  is 0.8 ms; about 1.3 ms is not split.
- T14, slow mode: the fsync 29 to 31 ms. The device took 11 to 13 ms for
  the 8 MiB and the commit's two Flushes 8.5 to 11.4 ms; the drive's slow
  mode decides it.

**Not yet established.**

- #1216's code after its review on the benches: a group of fresh extents
  now changes its bucket once (one LRU move) rather than once an extent.
  The figures above are of cc718f61f.
- The T14's fast mode, with one boot an arm.
- On the T14 the free figure DATA's mount prints fell by 512 KiB after one
  boot of cc718f61f, and after no other boot of either bench: one more
  occupied bucket or one more btree node. Every mount's figure across the
  next boots settles it: back up means transient metadata, steady means a
  one-time change, a further drop after a boot of #1216 means a leak.

**Exit.** `persist.seq_write_fsync` and `fs.disk.seq_write.1048576` at or
above Linux's on both benches, in every boot of a back-to-back session, or
each remaining difference measured to its parts.
