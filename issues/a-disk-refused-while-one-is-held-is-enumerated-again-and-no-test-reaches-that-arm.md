---
status: open
kind: defect
opened: 2026-09-27
---

# A disk refused while one is held is enumerated again, and no test reaches that arm

`refuse_for_now` (`kernel/src/drivers/xhci/device.rs`) gives a disk refused as
not ready, or for want of a pool block, a Disable Slot with
`AfterSlot::Again` while a disk on this controller is held for its device. The
arm in `slot_gone` (`kernel/src/drivers/xhci/mod.rs`) frees the refused
device's block, tears its port down so it is enumerated again, and logs
`xHCI: port N is enumerated again while a disk is held for its device`.

Nothing reads that line: `rg 'enumerated again while a disk' tests/` finds
nothing, and no run has shown whether any guest test reaches the arm. The
decision is taken in the kernel and not in `toyos-xhci`, so no host test can
stage it either.

**Owner**: the xHCI driver, `kernel/src/drivers/xhci/`.

**Exit**: a test stages a disk refused while another is held, and asserts that
its port is enumerated again and the disk bound. With `AfterSlot::Again`
replaced by `AfterSlot::Refused`, that test goes red.
