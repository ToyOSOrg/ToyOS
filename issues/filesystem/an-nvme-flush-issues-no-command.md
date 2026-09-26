---
status: open
kind: defect
opened: 2026-09-24
---

# An NVMe flush issues no command, so `fsync` on an NVMe disk answers `Ok` over a volatile write cache

`NvmeBlockDevice::flush` (`kernel/src/drivers/nvme.rs`) returns `Ok(())` without
sending anything to the controller; its doc says "writes are synchronous, so
there is nothing to flush". A write command's completion says the controller
took the data, not that it is on the medium: a controller that reports a
volatile write cache (the `VWC` field of Identify Controller) may hold
completed writes until a Flush command. So on such a disk `SYS_FSYNC` — a file's, logd's
`LOG_DURABLE_NS`, and a partition claim's — says durable about writes a power
cut can still lose.

QEMU's NVMe completes writes against its backing and every guest test passes
either way, which is why nothing here fails today. Whether the T14's
namespace reports a volatile write cache is not measured.

blockd, the NVMe driver in userland (`userland/blockd/src/nvme.rs`), reads
`VWC` and issues Flush, and `blockd_serves_partitions` reads the Flush commands
off QEMU's own trace. That is a second driver on a second controller: the
kernel's still serves `/apps`, `/home` and partition claims on the first, and
this defect is that driver's.

**Exit condition.** The kernel's driver reads `VWC` at bring-up and a flush on
a controller that has a volatile write cache issues Flush and waits for its
completion under the caller's operation budget, with a test that stages a
controller whose Flush fails and sees the fsync fail — or the driver is
deleted (`issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`,
step 9), whichever lands first.
