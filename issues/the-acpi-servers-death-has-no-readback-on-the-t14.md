---
status: open
kind: defect
opened: 2026-10-22
---

# The ACPI server's death has no readback on the T14

`acpi_server_death` (`tests/toyos.rs`) judges the T14's boot of
`tests/images/acpicase.toml`: its job, `acpi_release`, makes the boot's first
`acpi` claim off the boot processor, hands it to `/system/bin/acpiserver` and
kills the server, and the judge reads the kernel's `ACPI_ENABLE` and
`ACPI_DISABLE` lines off the log the boot leaves on its stick. That log
reaches the stick through usbd's claim of the boot controller.

Every PCI claim waits for the `acpi` claim holder's declaration of the root
bridges' memory (`kernel/src/pcidev`'s `declare`), and the supervisor makes
the disk drivers' claims while it starts its rows, before any job runs
(`Supervisor::boot`). On a boot whose first holder is a job no row declares,
so usbd's claim would wait out its 30 s deadline and leave every claim of the
boot refused. `acpicase` therefore starts no storage, and the row has no log
to judge. Derived from the boot order, not booted.

**Owner**: the PCI apertures track (#935).

**Exit**: the row's boot runs its job and its log reaches the stick, with the
job's claim still the boot's first.
