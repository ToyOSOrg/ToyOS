---
status: open
kind: defect
opened: 2026-10-10
---

# PCI is enumerated on segment group 0 alone

`toyos_acpi::ecam_allocations` (`toyos-acpi/src/mcfg.rs`) refuses an MCFG
window on any segment group but 0 (`AllocationRefused::OtherSegment`), so
`pci::enumerate` (`kernel/src/drivers/pci.rs`) never reads its functions, and
`PciDevice` and `pcidev`'s `Machine` carry one segment for every function. A
machine with more than one segment group has functions this kernel does not
see. The `acpi` claim's configuration accesses are bounded to segment group 0
too (`toyos_userbound::firmware::config`).

**Evidence:** the code. Every machine this tree has booted publishes segment
group 0 alone: QEMU's q35 and `virt` MCFGs (`toyos-acpi/tests/fixtures.rs`,
`virt_low_ecam`) and the T14's.

Owner: orchestrator. Exit: a function carries its segment group from
enumeration to `pcidev`'s inventory, every window the MCFG names is
enumerated whatever its group, and a host test of the decode accepts two
groups' windows.
