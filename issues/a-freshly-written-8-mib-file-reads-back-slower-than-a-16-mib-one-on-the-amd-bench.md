---
status: open
kind: finding
opened: 2026-11-11
---

# A freshly written 8 MiB file reads back slower than a 16 MiB one on the AMD bench

`persist.seq_read_cached`'s pattern writes 8 MiB in 1 MiB calls, syncs, and
reads the file back whole in 1 MiB calls. On the AMD bench, ToyOS at
`422915e2c` with stage timers in a scratch kernel, its paged reads copied
8.4 and 8.7 µs per chunk of 16 pages, against 6.1 and 6.2 µs for 1 MiB reads
of a 16 MiB file written without a sync and read once through first. Both
files were on the memory file system, both buffers at a page's start, so
neither copy is the store-forwarding stall
(`issues/a-userland-forward-copy-a-short-way-above-its-source-modulo-4-kib-stalls-on-amd.md`).
The row's samples ran from 6.3 to 10.8 GB/s within a boot, p50 7.8 and 7.3
GB/s, against Linux's 9.0 and 9.3. On the T14 the order is the other way:
4.3 and 4.4 µs per chunk for the 8 MiB file, 5.5 for the 16 MiB one
([evidence](https://github.com/ToyOSOrg/evidence/tree/cb8da5fc2168bfe0395b6bce364212b4117a170d/diagnosis)).

Cause not found: the two differ in the sync, the file's size against the
caches and the frames each file was written into.

Exit: the cause measured, for example the same 8 MiB file read with and
without the sync and at the 16 MiB file's size, and fixed or recorded where
it lives.
