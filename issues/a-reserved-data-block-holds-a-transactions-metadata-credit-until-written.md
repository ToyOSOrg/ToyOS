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

Holders' writes widen it. A DATA file a holder may write holds a pool of 64
reserved blocks for as long as a write holder of it lives, room for the
credit its holders are granted (`system/fileserver/src/pages.rs`'s `BULK`),
and each block holds its metadata credit beside it: 512 KiB of free space a
file open to write, whether it writes or not, until its last holder goes and
its pages are written back. Many files open to write at once, as a build
holds, take that much free space each before they write a byte.

**Evidence.** On QEMU's 128 MiB DATA, osbench's 16 MiB setup file is refused
for space (#1152's QEMU run of the `fs.disk` rows). The benches' DATA
partitions are large enough that no row reaches it.

**Exit.** Credit by what write-back will journal: one transaction's credit
for each run of contiguous reserved blocks, which `write_run` writes as one
extent, or for each transaction write-back is expected to take, instead of
for each block, a holders' pool included. A test that reserves a write of
most of a small volume's free space and writes it back.
