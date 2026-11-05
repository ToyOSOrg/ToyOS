---
status: open
kind: defect
opened: 2026-11-05
---

# Smaller DATA rows trail main by up to half

**Owner.** The storage track, which owns `lib/bcachefs` and the file
server's DATA volume.

osbench at #1152's a1f07627e, one boot a bench, p50, against main
dc1a8642e on its private format (nightly 2026-10-31-2316); T14 / AMD:

| row | #1152 | main | ratio |
|---|---|---|---|
| `fs.disk.tree.100` | 21.39 / 17.94 ms | 13.96 / 12.73 ms | 1.53 / 1.41 |
| `fs.disk.fstat` | 10.86 / 12.08 us | 8.72 / 9.92 us | 1.25 / 1.22 |
| `fs.disk.direct.fstat` | 10.64 / 11.86 us | 8.45 / 9.75 us | 1.26 / 1.22 |
| `fs.disk.stat` | 13.58 / 14.93 us | 11.63 / 12.54 us | 1.17 / 1.19 |
| `fs.disk.direct.stat` | 12.49 / 13.88 us | 10.66 / 11.54 us | 1.17 / 1.20 |
| `fs.disk.seq_write.4096` | 162 / 164 MB/s | 133 / 211 MB/s | 1.22 / 0.78 |

The larger gaps have records of their own
(`issues/a-4-kib-overwrite-on-data-costs-about-2-4-times-mains.md`,
`issues/a-data-directory-listing-reads-every-entrys-inode-alone.md`).

**What is known of the causes.**
- `tree.100` creates, writes and removes 100 small files: each create,
  write and unlink is its own journaled transaction of several btrees
  (inode, dirent, extent, backpointer, accounting). Which of them makes
  the difference is not measured.
- `stat` resolves its path from the server's name cache and reads one
  inode (round 5 of #1152: 5.2 us of server time on QEMU/KVM). `fstat`
  reads no path. Where their remaining 1.7 to 2.3 us goes is not
  established; it was not measured that it is the engine.
- `seq_write.4096` is above main on the T14 and below it on the AMD, after
  #1152's write that continues the last extends its reservation. The
  AMD's remaining cost is not established.

Evidence: #1152's evidence, `storage-integration/r10-known-gaps`
(`osbench-a1f07627e/` and `osbench-table-a1f07627e.md`).

**Exit.** Each row within 1.1 times main's on both benches, or its cause
shown to be inherent.
