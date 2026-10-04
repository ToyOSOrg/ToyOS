---
status: open
kind: defect
opened: 2026-10-04
---

# The T14 stayed on after a power-off in ACPI mode

On the attended `acpi_power_button_pressed` boot of #713 at `b3b9ccd69`, the
owner pressed the power button once: the log on the stick ends with
`acpiserver`'s press at 16.705 s and the supervisor's
`power: the machine stops, ... (Shutdown)` at 16.705 s, and the machine stayed
on, the screen unchanged, until he pressed again about five seconds later and
it went off at once.

What is known: the same stop path, run for a reboot in ACPI mode on the same
image's `acpi_server_events` boot, went from the supervisor's line to the
kernel's `Rebooting.` in 8 ms. A power-off differs from it only in
`arch::power::off`: every PM1 and GPE0 event disabled and cleared, then
`SLP_TYP` 7 and `SLP_EN` written to PM1a_CNT at 0x1804. So the machine did not
act on that write within five seconds, or something between the supervisor's
line and it stalled where the reboot did not. Which, and why, is unread:
nothing after the supervisor's flush is durable, and the next loader pass
found the black box empty, as a power cut leaves it.

Unknown too is what the second press did. In ACPI mode with `PWRBTN_EN`
cleared, a short press asks nothing of the platform, so the firmware acting on
it means `SCI_EN` was clear by then or a sleep sequence was under way; ToyOS
had nothing left to do once `SLP_EN` was written.

The instrument: `off` now panics if the machine still runs two seconds after
`SLP_EN`, naming PM1a_CNT before and after, `SCI_EN`, the PM1 status and
enable, and the SMI count either side of the write. The press happened within
milliseconds of the stop, with the button likely still down; Linux powers off
seconds after a press. The `acpi_power_off` row asks for the same power-off
with no hand on the button, which tells the two apart.

**Exit**: `acpi_power_off` and `acpi_power_button_pressed` pass on the T14,
each read off a boot of the same head.
