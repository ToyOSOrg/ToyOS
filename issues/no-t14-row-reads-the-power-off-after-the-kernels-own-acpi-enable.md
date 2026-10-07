---
status: open
kind: tooling
opened: 2026-10-04
---

# No T14 row reads the power-off after the kernel's own ACPI_ENABLE

On the T14 the firmware hands the machine over in legacy mode, the kernel
writes `ACPI_ENABLE` for the server's claim, and a power-off then quiets the
events and writes `SLP_EN` on a machine this kernel itself put in ACPI mode
(`kernel/src/arch/x86_64/acpi_mode.rs`, `power::off`). A write the platform
does not act on is `power::off`'s panic. Nothing reads that path on the T14:
a power-off that takes leaves the machine in S5, and nothing in the metal
loop powers it on again. `ride_the_reboot` (`src/metal.rs`) waits
`return_secs()` for Ubuntu's ssh and refuses before `read_log`, so the boot
leaves no readback.

**Ruled** (owner, 2026-10-05): "A test that requires manual steps from me is
forbidden." **Ruled** (owner, 2026-10-05, on what a T14 test may need): "No
automated test is allowed that requires physical buttons to be pressed or
anything we cant do now with the t14. I can test it on demand but no ci there
not always someone available physically". The row that read this path, `acpi_power_off`, was
judged only on a boot the owner powered on again by hand. It went with those
rulings, and so did the harness's acceptance of a boot whose log ends asking
for a power-off.

Its last readings, both green and both powered on again by the owner: at
`ff4945d6d`, and at `8d7004d3b`, where the kernel logged `acpi: ACPI mode:
ACPI_ENABLE 0xf0 written to SMI_CMD 0xb2, SCI_EN set 13371ns after` at
13.535 s, the supervisor's `(Shutdown)` line followed at 13.540 s, and the
loader pass after it found no panic record.

**What QEMU reads of it.** `machine_shutdown`, `machine_shutdown_short_stop`
and `acpi_power_button` power q35 off in ACPI mode, and none of them after
the kernel's enable: OVMF hands q35 over with `SCI_EN` set, so the mint
writes nothing. A guest can be put in legacy mode from outside, with nothing
shipped for it: on QEMU 11.1.1's q35 under OVMF and TCG, `PM1a_CNT` at 0x604
read 0x0001 as the firmware left it, 0x0000 after the monitor wrote
`ACPI_DISABLE` (3) to 0xb2, and 0x0001 again after it wrote `ACPI_ENABLE`
(2). No test does this: the write has to land before the claim is minted, and
the one config whose claim a job mints, `tests/acpicase`, runs its job list
unprompted. Such a test reads QEMU's model of the ICH9 and never the T14's
firmware.

Owned by the stage "power-off through the server" of
`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`, which
rewrites the power-off this exit's test reds on. The orchestrator's
placement, not the owner's.

**Exit** (the orchestrator's placement, not the owner's; not built in stage 1
of `issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`):
a QEMU guest test whose harness clears `SCI_EN` through the monitor before
the claim is minted, so that the kernel writes `ACPI_ENABLE` itself, ends in
ToyOS's power-off, and is red where `SLP_EN` does not take after that enable.
The T14's own firmware on this path stays unread by any row.
