---
status: open
kind: defect
opened: 2026-10-07
---

# The `acpi` claim's holder reaches every port and firmware range the kernel did not declare

The kernel reads and writes for the holder of the `acpi` claim whatever the
firmware's AML names (`SYS_ACPI`, `kernel/src/arch/x86_64/acpi_mode.rs`), and
decides each access by what its address is
(`toyos-userbound/src/firmware.rs`). What that decision passes is wider than
what any one machine's AML needs, because nothing tells the kernel which
regions a machine's tables define until the holder's interpreter has loaded
them:

- **Every port the kernel did not declare and no other row names**, both
  ways. An I/O BAR of a PCI function is such a port, a kernel driver's and a
  claim holder's alike, and so is every chipset register the firmware never
  mentioned.
- **All ACPI NVS and reserved memory**, both ways, outside the FACS and the
  windows the kernel mapped: the firmware's own state, which its SMI handlers
  read and trust.
- **A function's configuration space from 0x40 to 0xFF**, outside the five
  capabilities the kernel programs, on a function no driver holds: where a
  chipset keeps its lock and decode registers.

So a bug in `/system/bin/acpiserver`, or AML it runs, can reach those; the
kernel bounds where, and not what. The owner's ruling on the server reading
and writing firmware-owned memory, ports and PCI configuration space through a
kernel-checked call, as the orchestrator's brief for this slice records it:
"just do it properly the first time i dont know what it is but no workarounds
as always and do the work whatever that is". That the surface so built is
address-bounded and no narrower is this change's design, not his ruling, and
`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md` owns
this beside
`issues/the-acpi-servers-holder-drives-the-embedded-controller-unfiltered.md`.

What the T14's tables need was measured without the machine, on a model whose
reads answer zero: at load, reads of six ACPI NVS pages and of four functions'
configuration space, and no write; across its initialisation and query
methods, memory writes in three pages, port writes to two ports nothing
declared and to `SMI_CMD`, and no configuration write. Which UEFI types the
real bases fall in is unread.

**Exit**: an access is passed only inside a region the machine's loaded tables
define, checked by something other than the holder; or the owner rules the
address-only bound is the one ToyOS keeps.
