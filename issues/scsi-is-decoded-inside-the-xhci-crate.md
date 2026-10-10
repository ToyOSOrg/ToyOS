---
status: open
kind: defect
opened: 2026-10-10
---

# SCSI is decoded inside the xHCI crate

`toyos-xhci/src/scsi.rs` holds the SCSI half of a Bulk-Only disk: the
commands sent (SPC-4, SBC-3), the sense data and READ CAPACITY answers read,
and the bring-up from a configured interface to a disk with a size. None of
it is xHCI's. SCSI is the command set of the device behind the transport, and
those bytes are chosen by the device, so the module is an input boundary of
its own sitting in the crate whose subject is the root-hub port machine. Its
only consumer is the kernel's mass-storage driver
(`kernel/src/drivers/xhci/wait/msc.rs`, `stop.rs`), which takes it and the
xHCI decisions from one crate, so the two subjects are not yet pulled apart.
`toyos-xhci/src/bot.rs`, the Bulk-Only Transport, is USB's and stays.

Owned by usbd, the small-kernel track's step 10
(`issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`), which
takes mass storage out of the kernel and is the first program to consume the
SCSI decoder apart from the controller.

**Exit condition.** When usbd takes mass storage, the SCSI decoder is a crate
of its own, `no_std` with no dependency on `toyos-xhci`, and its host tests
move with it; `toyos-xhci` no longer has a `scsi` module.
