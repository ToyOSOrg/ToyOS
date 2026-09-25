---
status: open
kind: defect
opened: 2026-09-25
---

# netd never reaches its loop on the `iommu-userdev-foreign-dma` boot, and no userland line reaches that boot's console

`tests/common/iommu.rs`'s `USERDEV_FOREIGN` arm boots `tests/netcase` with
netd's first grant answered with an NVMe address. With a kernel that logged
every `pcidev::take_record` call (a throwaway diagnostic on `wt/toyos-logd`),
netd made **no** interrupt read in the 20 s after the fault — its loop's first
act — and the boot's console carried no userland line at all, before the fault
or after it: no `init: started`, no `netd: VirtIO: PCI`, nothing from logd.
The kernel's own lines ran on, and no `exit:` for netd was ever printed.

So netd is parked somewhere between its grant and its loop on that boot, and
`userdev_dma_fault` passes without asking where: its verdict is the machine,
not the driver. Where netd is parked, and why this config's userland output
never reaches the console, are unmeasured.

**Exit condition**: the place netd waits on that boot named — a blocked-task
dump or a line of its own — and either the wait removed or `userdev_dma_fault`
asserting what netd does after the fault.
