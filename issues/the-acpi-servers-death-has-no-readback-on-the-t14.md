---
status: open
kind: defect
opened: 2026-10-22
---

# The ACPI server's death has no readback on the T14

`acpi_server_death` judged the T14's boot of
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
to judge. Measured at #935's `4e32df68c`: the boot ran its job and stopped
cleanly (its black box reads `Rebooting.` and DONE), and `kernel.log` came
back empty. The row's registration went with #935; `acpi_release` stays,
run by nothing (QEMU's firmware hands over in ACPI mode, so its release has
no legacy mode to return to).

**Owner**: the PCI apertures track (#935).

**Exit**: `acpi_server_death` is registered again, its judge
(`ACPI_ENABLE` asked off the boot processor for the job's claim,
`ACPI_DISABLE` written and `SCI_EN` read clear at the server's death, the
FACS outside `AddressRangeMemory`) restored from the tree before #935, on a boot
that runs its job with the job's claim the boot's first and whose log reaches
the stick, and it passes on the T14.
