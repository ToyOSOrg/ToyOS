---
status: open
kind: defect
opened: 2026-11-11
---

# A DATA directory listing is read whole for every READDIR request

**Owner.** The storage track, which owns the file server's DATA volume and
`lib/bcachefs`.

osbench's `fs.disk.readdir.1000`, listing a directory of 1 000 files, at
#1152's a1f07627e: 2.86 ms on the T14 and 1.99 ms on the AMD, p50, one boot
each; main dc1a8642e 0.91 and 0.69 ms (nightly 2026-10-31-2316), about 3x.

**Cause.** Every READDIR request lists the whole directory again before
taking its batch from the cursor (`system/fileserver/src/main.rs`). A client
asks in 64 KiB batches, so 1 000 short names are one or two requests. On the
host model of the T14's DATA (CPU only), one whole listing of 1 000 entries
cost 534 us at a1f07627e: the dirents 39 us, an inode lookup from the root
for each entry 416 us, sorting and encoding the rest (#1152's evidence,
`storage-integration/r10-known-gaps`, `perf.rs`'s `listing_a_thousand`).

The inode lookups are gone: a listing reads its inodes in one pass over each
run of them (`Mount::listing`), held by `lib/bcachefs`'s
`a_listing_reads_its_inodes_in_a_pass_over_each_run_of_them`. What that
leaves of the row on the benches is not measured yet.

**Exit.** `readdir.1000` within 1.5 times main's on both benches; if the
single pass does not reach it, the listing kept across a directory's READDIR
batches.
