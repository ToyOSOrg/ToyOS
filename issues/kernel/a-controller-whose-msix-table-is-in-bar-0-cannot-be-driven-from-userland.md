---
status: open
kind: defect
opened: 2026-09-26
---

# A controller whose MSI-X table is in BAR 0 cannot be driven from userland

`pcidev` never maps the BAR holding a function's MSI-X table or PBA
(`kernel/src/pcidev/mod.rs`, `msix_bar`): a holder that could write the table
could aim the device's message at any address the LAPIC decodes. A window is a
whole 2 MiB page, so the refusal takes the whole BAR. NVMe keeps its registers
and doorbells in BAR 0, and a controller that puts its MSI-X table there too
has no BAR left to hand over: the claim is refused.

Measured on QEMU 11.1's NVMe, whose default puts the table in BAR 0: blockd's
claim answers `NotSupported` and the kernel says `pcidev: PCI 00:03.0 NOT
HANDED OVER — it publishes no memory BAR this claim may map, so its holder
would have no registers to drive it through`. The blockd tests boot the
controller with `msix-exclusive-bar=on`, which moves the table to a BAR of its
own (`tests/common/qemu.rs`, `BootOptions::userland_nvme`). Where the T14's
NVMe keeps its table is not measured; it matters from the small-kernel track's
step 9 (`issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`),
when blockd is the only NVMe driver.

**Exit condition.** A claim hands over the pages of a BAR that hold neither
the table nor the PBA, at the granularity those structures need (4 KiB, which
the user mappings do not have yet:
`issues/kernel/process-memory-is-2-mib-pages-and-that-caps-the-process-count.md`),
and a test drives QEMU's NVMe with its table in BAR 0 through blockd while a
write to the table's page is refused.
