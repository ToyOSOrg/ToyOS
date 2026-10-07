---
status: open
kind: finding
opened: 2026-10-07
---

# Nothing reads whether an SCI source is enabled when ACPI_DISABLE is written

ACPI 6.5 Table 5.9, on `ACPI_DISABLE`, has the OS hand ownership back "by
relinquishing use to the ACPI hardware registers, masking off all SCI
interrupts, clearing the SCI_EN bit and then writing ACPI_DISABLE to the
SMI_CMD port from the boot processor."

`acpi_mode::release` (`kernel/src/arch/x86_64/acpi_mode.rs`) writes the
disable when the claim's last handle goes, which is after its holder stopped
running, and nothing in the kernel reads the PM1 enable register or the GPE0
enable bytes before it: what a killed `/system/bin/acpiserver` left enabled is
enabled still when the firmware takes the registers back. It does not clear
`SCI_EN` itself, by §4.8.2.5 and Table 4.13 of the same document, and says so
at the site. No failure is recorded: on the T14 `SCI_EN` read clear after
every recorded disable.

Owned by `issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`.

**Exit**: the release's `legacy mode again` line says what the PM1 enable
register and the GPE0 enable bytes read at the write on the T14's
`acpi_server_death` boot, and either every one reads zero there by the
release's own doing or this file is promoted with what the firmware did with
the ones that did not.
