---
status: open
kind: defect
opened: 2026-10-12
---

# A hardware-reduced x86 machine has no power-off and no power button

On x86-64 the kernel powers off only through the FADT's PM1a control block
(`kernel/src/arch/x86_64/power.rs`), with the `\_S5` sleep type the holder of
the `acpi` claim supplies, and that claim exists only over the fixed hardware:
`toyos_acpi::fixed_hardware` refuses a FADT with `HW_REDUCED_ACPI` set
(`lib/acpi/src/fadt.rs`), and `acpi_mode::init`
(`kernel/src/arch/x86_64/acpi_mode.rs`) then mints no row. So on such a machine
acpiserver gets no claim, nothing supplies S5, a shutdown is refused with
`no ACPI server supplied S5`, and a press of the power button reaches nobody.

A hardware-reduced platform has neither PM1 block nor SMI_CMD. ACPI 6.5 gives
it the FADT's `SLEEP_CONTROL_REG` and `SLEEP_STATUS_REG` for sleep and
soft-off (§4.8.3.7, Sleep Control and Status Registers), and the Generic Event
Device, `ACPI0013`, for its events, the power button's among them: an
interrupt whose `_EVT` method the AML runs (§4.1.1.1). Every AArch64 machine
is one, which this tree meets with PSCI (`kernel/src/arch/aarch64/power.rs`).
These ACPI sections are as a third-party copy of ACPI 6.6 numbers them; 6.5
itself is unread.

Cloud Hypervisor's x86-64 FADT sets `HW_REDUCED_ACPI` and names the two sleep
registers and a reset register, and PM1a blocks only under an option of its
own, at otherwise unused ports. Its power button, which its API's
`/vm.power-button` presses (its `docs/api.md`), is a GED whose `_EVT` notifies
`\_SB_.PWRB`. QEMU's `microvm` is hardware-reduced too. q35,
crosvm, VirtualBox and bhyve publish the fixed hardware.

**Owner**: whoever holds
`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`, whose
server holds the machine's power button and S5.

**Exit**: a hardware-reduced FADT and DSDT, recorded by ToyOS from a Cloud
Hypervisor guest with their provenance as `lib/acpi/fixtures/` records QEMU's,
decode in a host test to the sleep control register and the GED's
power-button method, and acpiserver's host test runs that method to a press;
and the image booted by hand under Cloud Hypervisor on x86-64, its log on the
evidence repository under the hypervisor's version and command line, powers
off on `shutdown` and on a press through `/vm.power-button`.
