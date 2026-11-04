---
status: open
kind: defect
opened: 2026-10-15
---

# An x86 machine with no ACPI row hands over no PCI claim

No PCI claim is handed over before the holder of the `acpi` claim declares the root
bridges (`kernel/src/pcidev`'s `declare`, made by `/system/bin/acpiserver`),
and on x86-64 that claim exists only over a row `acpi_mode::init`
(`kernel/src/arch/x86_64/acpi_mode.rs`) fills. It fills none where the FADT is
unusable, its fixed hardware is refused (`HW_REDUCED_ACPI` among the
refusals), no PM1a control block is declared, its `SMI_CMD` was not declared,
no I/O APIC carries the SCI, or the SCI's line is another row's. On such a
machine acpiserver gets no claim and declares nothing, so the supervisor
starts no PCI driver, the disk drivers among them, and says so once its wait
for the declaration fails. Before #935 the loader's window list handed those
claims over.

Measured: none of the machines in reach is such a machine. q35, the
ThinkPad T14 and the AMD bench each fill the row (their `acpi: the ACPI row:`
lines); a hardware-reduced x86 guest (Cloud Hypervisor, QEMU's `microvm`)
would not, as `issues/a-hardware-reduced-x86-machine-has-no-power-off-and-no-power-button.md`
records for its power-off.

**Owner**: the ACPI track
(`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`).

**Exit**: such a machine declares its root bridges through a path that needs
no `acpi` claim row, or the gap is measured absent on every supported machine.
