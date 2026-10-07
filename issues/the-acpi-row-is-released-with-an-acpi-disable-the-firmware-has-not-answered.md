---
status: open
kind: defect
opened: 2026-10-07
---

# The ACPI row is released with an ACPI_DISABLE the firmware has not answered

`acpi_mode::release` (`kernel/src/arch/x86_64/acpi_mode.rs`) writes
`ACPI_DISABLE` and reads `SCI_EN` until it is clear, and only then hands the
row back, so that no claimant finds the bit set by a holder whose disable is
still to come. The read is bounded by `HANDBACK`, 100 ms. Past it the kernel
logs `acpi: still in ACPI mode` and hands the row back all the same, with
the disable outstanding. The next claimant's mint then finds `SCI_EN` set,
logs that nothing is written and answers `Ok`. A firmware that acts on the
disable after that takes the machine out of ACPI mode under a holder that
wrote no enable, and nothing tells that holder or the kernel: the server
goes on waiting for an SCI the machine no longer raises.

What is known. `HANDBACK` is this kernel's own number: ACPI 6.5 §4.8.2.5 has
OSPM poll `SCI_EN` until it reads reset and names no time. The one firmware
read, the T14's, answered in 17068 ns on its `acpi_server_death` boot at
`922a6b7c7` (`acpi: legacy mode again: ACPI_DISABLE 0xf1 written to SMI_CMD,
PM1a_CNT reads 0x0000 17068ns after, SCI_EN clear`). No tier reaches the
expiry: OVMF hands q35 over in ACPI mode, so a guest's release writes
nothing, and the T14 answers inside the bound. No failure is recorded.

Owned by `issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`,
whose stage 1 landed the release.

**Exit**: no claimant is handed the row while an `ACPI_DISABLE` this kernel
wrote is unanswered, and a test reds where one is; the expiry is reached on
some tier by that test.
