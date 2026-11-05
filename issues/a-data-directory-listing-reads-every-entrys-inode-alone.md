---
status: open
kind: defect
opened: 2026-11-05
---

# A DATA directory listing reads every entry's inode alone

**Owner.** The storage track, which owns the file server's DATA volume and
`lib/bcachefs`.

osbench's `fs.disk.readdir.1000`, listing a directory of 1 000 files, at
#1152's a1f07627e: 2.86 ms on the T14 and 1.99 ms on the AMD, p50, one boot
each; main dc1a8642e 0.91 and 0.69 ms (nightly 2026-10-31-2316), about 3x.

**Cause.** A listing entry carries its file's size, so `DataVolume::list`
(`system/fileserver/src/data.rs`) reads each entry's inode with a lookup of
its own, from the root of the inodes btree, after one readdir of the
dirents; and every READDIR request lists the whole directory again before
taking its batch from the cursor. On the host model of the T14's DATA (CPU
only), one whole listing of 1 000 entries costs 534 us: the dirents 39 us,
the 1 000 inode lookups 416 us, sorting and encoding the rest (#1152's
evidence, `storage-integration/r10-known-gaps`, `perf.rs`'s
`listing_a_thousand`). A client asks in 64 KiB batches, so 1 000 short
names are one or two requests.

**Exit.** The inodes of a listing read in one ordered pass over the inodes
btree, or the listing kept across a directory's READDIR batches, with
`readdir.1000` within 1.5 times main's on both benches.
