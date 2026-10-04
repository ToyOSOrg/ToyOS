---
status: open
kind: defect
opened: 2026-09-06
---

# The MCFG entry's start bus is never read

`toyos_acpi::ecam_base` (`toyos-acpi/src/lib.rs:256-263`) takes the first MCFG
allocation structure's base address and nothing else from it. The structure
carries a PCI segment group, a start bus and an end bus as well (PCI Firmware
Specification 3.3, Table 4-3), and the base is the address of *that entry's
start bus*, not of bus 0: on a machine whose first entry begins at a bus above
zero, or whose bus 0 lives in a later entry, `base + (bus << 20)` names another
segment's configuration space or nothing at all. Nothing downstream can notice,
because the range is not returned: `kernel/src/drivers/acpi.rs:96-107` logs the
base and hands it on, and every caller then indexes it as though bus 0 sat at
offset zero. Every machine this tree has booted has one entry starting at bus
0, so the defect is unobserved rather than absent.

Exit: the decode returns the entry's segment and bus range beside its base, and
every caller refuses a bus outside that range by name rather than computing an
address from it.
