---
status: open
kind: defect
opened: 2026-10-10
---

# PCI is walked from one root bus per ECAM window, on one segment group

`pci::enumerate` (`kernel/src/drivers/pci.rs`) enters each ECAM window at its
start bus and from there only the buses a bridge forwards
(`toyos_pci::buses`). A host bridge whose root bus is any other bus of the
window — a second root complex in one segment group, as on multi-socket and
many-die parts, or QEMU's `pxb-pcie` — is never entered, and its functions are
not enumerated. Functions on a segment group other than the first window's are
not enumerated either: `toyos_acpi::ecam_allocations` refuses those windows
`OtherSegment`, and `PciDevice` carries no segment.

What names every root bus is each host bridge's bus range: `_BBN` and `_CRS`
in AML, or the bus descriptor of `EFI_PCI_ROOT_BRIDGE_IO_PROTOCOL`'s
`Configuration()`, which the loader already reads for memory windows
(`bootloader/src/rootbridge.rs`) and with its segment number beside it. q35's
and `virt`'s guests have one root bus, at their window's start.

Exit: the kernel enters every root bus firmware names, on every segment group
it names, and a guest with a `pxb-pcie` enumerates the functions behind it.
