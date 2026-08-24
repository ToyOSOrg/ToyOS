---
status: open
kind: defect
opened: 2026-08-23
---

# `read_link` sizes one kernel allocation from the volume, not from the heap ceiling

`bcachefs::Mounted::read_link` is on the kernel's `FileSystem` adapter
(`kernel/src/bcachefs_adapter.rs`, both the NVMe and initrd arms), and the
target it returns comes out of `read_extents`, which does `vec![0u8; size]`
with `size` off the disk. That `size` is now held against the volume's own
block count — a file cannot be longer than the thing it is stored on — and
the volume is the wrong ceiling for a kernel `Vec`: `mm::MAX_HEAP_ALLOC` is
2,093,056 and `KernelAllocator::alloc` *asserts* above it rather than
returning null. A `/home` of any real size therefore still has a band —
2 MiB up to the volume's size — in which a crafted leaf value's declared
size panics the kernel from a mounted disk.

Reachable only from a volume somebody else wrote, which is the threat model
`issues/isolation/probe-mounts-on-a-checksum.md` describes: `probe()` mounts
any disk whose block 0 carries `BCFS`, version 1 and a CRC that checks out.
Nothing userland can do through the syscall API produces one — a symlink
target is bounded by `user_ptr::MAX_USER_STR` (64 KiB) on the way in.

The shape of the fix is in the tree already, one filesystem over:
`kernel/src/fat32_adapter.rs`'s `MAX_EXTENTS` is a bound *derived against*
`MAX_HEAP_ALLOC` and enforced at the adapter, because the ceiling is the
kernel's knowledge and `toyos-fat32` is a portable crate that does not have
it. `bcachefs` is the same kind of crate and this is the same kind of bound,
so the comparison belongs at the adapter — either a cap on what `read_link`
will materialise, or a `read_link` that takes a maximum and refuses past it.

Measured: `a_size_longer_than_the_volume_never_reaches_the_allocator`
(`bcachefs/src/fs.rs`) records that a symlink declaring 1 GiB asked the host
allocator for 1,073,741,824 bytes before the bound went in. The residual is
the same instrument with a smaller number.
