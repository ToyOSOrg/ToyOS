---
status: assigned
kind: defect
opened: 2026-10-22
---

# The ACPI server's death has no readback on the T14

`acpi_server_death` judges the T14's boot of `tests/images/acpicase.toml`.
Its job, `acpi_release`, makes the boot's first `acpi` claim off the boot
processor, hands it to `/system/bin/acpiserver`, and kills the server once it
has declared the root bridges' memory. The judge reads the kernel's
`ACPI_ENABLE` and `ACPI_DISABLE` lines off the log the boot leaves on the
bench's stick, and that log reaches the stick only through storage.

#935 made every PCI claim wait in the kernel for the declaration. The
supervisor made the disk drivers' claims before any job ran, so on this boot
the NVMe driver's claim waited out its 30 s deadline and the boot had no
storage. Measured at #935's `4e32df68c`: the boot ran its job and stopped
cleanly, its black box reads `Rebooting.` and DONE, and `kernel.log` came
back empty. The row, its judge and its job went with #935.

The owner's ruling of 1–2 Nov 2026 removed the kernel's wait: the
supervisor starts storage and the PCI drivers once the device set says the
declaration is held, after `logkeeper` and the rows the declaration comes
through, the job's runner among them. The row, its judge and its job are registered again on that
design. They have not run on the T14 yet.

**Owner**: the ACPI track
(`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`).

**Exit**: `acpi_server_death` passes on the T14. It needs `ACPI_ENABLE`
asked off the boot processor for the job's claim, `ACPI_DISABLE` written and
`SCI_EN` read clear at the server's death, the FACS outside
`AddressRangeMemory`, and the declaration made before the server's death.
