---
status: open
kind: defect
opened: 2026-10-07
---

# The kernel writes SMI_CMD from whichever CPU its caller runs on

ACPI 6.5 Table 5.9, on `SMI_CMD`: "OSPM issues commands to the SMI_CMD port
synchronously from the boot processor." On `ACPI_DISABLE`: "An OS can hand
ownership back to SMI by relinquishing use to the ACPI hardware registers,
masking off all SCI interrupts, clearing the SCI_EN bit and then writing
ACPI_DISABLE to the SMI_CMD port from the boot processor."

`kernel/src/arch/x86_64/acpi_mode.rs` writes `ACPI_ENABLE` on the CPU the
claimant mints on and `ACPI_DISABLE` on the CPU the claim's last handle goes
on. On the T14's `acpi_server_death` boots at `ff4945d6d` and `8d7004d3b` the
enable was written from cpu7 (`acpi: ACPI mode: ACPI_ENABLE 0xf0 written to
SMI_CMD 0xb2, SCI_EN set 16687ns after; cpu7's SMI count 4819 before the
write and 4820 after`) and the disable from cpu0; on every other recorded
boot the enable came from cpu0. Each took: `SCI_EN` read set, and clear after
the disable. No failure is recorded, and no firmware but the T14's has been
read.

The release follows §4.8.2.5 of the same document, which has OSPM write
`ACPI_DISABLE` and poll `SCI_EN` until it reads reset, and Table 4.13, which
makes the bit the hardware's to set and reset and has OSPM preserve it. Table
5.9's sequence, in which the OS clears `SCI_EN` itself before the write,
contradicts both, and the kernel does not follow it. Whether the release
leaves any SCI source enabled when it writes the disable is not read here.

Owned by `issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`,
whose stage 1 landed these writes.

**Exit**: every write to `SMI_CMD` is made on the boot processor by
construction, and the kernel's `ACPI mode:` and `legacy mode again` lines name
it on the T14's `acpi_server_death` boot; or the owner rules that the CPU does
not matter, and that ruling is recorded at the site.
