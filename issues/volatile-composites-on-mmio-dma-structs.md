---
status: open
kind: defect
opened: 2026-08-20
---

# `write_volatile`/`read_volatile` over a whole DMA/MMIO struct

`clippy::volatile_composites` (nursery) measured ten sites in the kernel that
hand a composite type straight to `write_volatile` or `read_volatile` instead
of touching each primitive field on its own. The NVMe driver's queue entries,
virtio-gpu's EDID answer and the xHCI's `ErstEntry` and `Trb` left the kernel
with their drivers. What is left is the virtio descriptor: `Virtqueue`'s
submission writes a whole `VirtqDesc` (`kernel/src/drivers/virtio.rs`) through
`Dma::write`, whose generic `write_volatile` (`kernel/src/mm/dma.rs`) takes any
`T: Copy`.

The lint's own docs put the reason plainly: volatile ops are well-defined for
a primitive because the compiler must emit exactly the load/store the source
says, in order — the property MMIO needs. That guarantee is **not** extended
to composite types; how rustc lowers a whole-struct `write_volatile` (one
store, several, in what order) is implementation-defined and can change
between compiler versions. The fix the lint wants is per-field volatile
access.

Fixing it is driver work: read the virtio specification for what field
ordering the descriptor table requires before the available ring publishes
it, decide whether one `write_volatile` is provably fine or needs splitting,
and add the coverage that would catch the difference.
`issues/clippy-stage-two-is-lints-one-at-a-time.md` records the measurement.
