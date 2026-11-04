---
status: open
kind: defect
opened: 2026-11-04
---

# A reserved DATA block holds a transaction's metadata credit until it is written

**Owner.** The storage track, which owns `lib/bcachefs` and the file server's
DATA volume.

The file server reserves the blocks of a write before it copies the bytes,
and `Reservations::pending` (`lib/bcachefs/src/engine/mount/data.rs`) counts
one ordinary transaction's metadata credit, `K_TX * 8` bytes, for every
reserved block not yet written. A block of 4 KiB therefore holds as much
metadata room as data until write-back, and a large write needs about twice
its size free. Main's private format had no such limit.

**Evidence.** On QEMU's 128 MiB DATA, osbench's 16 MiB setup file is refused
for space (#1152's QEMU run of the `fs.disk` rows). The benches' DATA
partitions are large enough that no row reaches it.

**Exit.** Credit by what write-back will journal: one transaction's credit
for each run of contiguous reserved blocks, which `write_run` writes as one
extent, or for each transaction write-back is expected to take, instead of
for each block. A test that reserves a write of most of a small volume's free
space and writes it back.
