---
status: open
kind: defect
opened: 2026-11-05
---

# A 4 KiB overwrite on DATA costs about 2.4 times main's

**Owner.** The storage track, which owns `lib/bcachefs` and the file
server's DATA volume.

osbench's `fs.disk.write_4k_at`, 4 KiB writes at scattered pages of a
16 MiB file, at #1152's a1f07627e: 23.9 us on the T14 and 30.8 us on the
AMD, p50, one boot each. Main dc1a8642e, on its private format, took
10.2 and 12.2 us (nightly 2026-10-31-2316); Linux ext4 0.7 and 2.3 us.

Each overwrite is a copy-on-write replace: the write lands in new sectors
and one transaction removes or trims the old extent, inserts the new one,
and updates its backpointer, bucket accounting and the inode. On the host
model of the T14's DATA (CPU only, no device latency), taking the sectors
costs 0.8 us and that transaction 30.6 us an overwrite. A macOS `sample` of
it puts about 11% of the busy samples in `Cached::put`'s moves of a node's
keys (`memmove` under `mount::put` in `apply`); the rest is spread over the
pending overlay, allocation and the block writes, with no single cost
(#1152's evidence, `storage-integration/r9-small-writes`, `perf.rs`'s
`scattered_overwrites` and `scatter-sample.txt`).

**Exit.** `write_4k_at` within 1.5 times main's on both benches, measured
by osbench against the same nightly's main, with the host model's
transaction cost and a sample of it published.
