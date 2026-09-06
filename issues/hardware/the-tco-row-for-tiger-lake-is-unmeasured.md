---
status: open
kind: tooling
opened: 2026-09-06
---

# Everything the TCO watchdog assumes about the T14 is unread

`toyos-tco::CHIPSETS` carries a row for the T14's SMBus function `8086:a0a3`:
config dword `0x50` masked with `!1` is the TCO block's 32-byte I/O base, and
`0x54` bit 8 must be set or the row refuses by name. Its reference is Linux
v6.8's `drivers/i2c/busses/i2c-i801.c`. QEMU models no SMBus TCO base, so
nothing in the harness exercises any of what follows: the judge is the machine.

Five things are unread on the T14, and a measurement is owed for each.

- **`TCOBASE` and `TCOCTL`.** `sudo setpci -s 00:1f.4 0x50.l 0x54.l` says
  whether firmware enables the block at all and what base it programmed. A
  `--kernel-param watchdog` boot either logs a port and arms or logs a refusal,
  and which is not known.
- **The second-expiry rule.** `hw/acpi/ich9_tco.c:61-70` acts on the *second*
  consecutive expiry, and `toyos_tco::timer_for` halves every bound for it.
  That is a QEMU fact; whether the PCH counts the same way is not established.
- **`TCO_LOCK`.** Firmware may set it and leave `TCO_TMR_HLT` unclearable, in
  which case `arm`'s read-back refuses and this kernel arms nothing. Whether
  the T14's firmware does is unknown.
- **`NO_REBOOT`.** The PCH has a strap that turns an expiry into no reset at
  all. This kernel neither reads nor clears it, so an armed watchdog on a
  machine holding it set is one that does nothing quietly. Reading it back and
  refusing by name is owed, and needs the register's location.
- **How `TCO2_STS` clears.** `arm` writes the two status bits back so a reset
  is reported by one boot rather than by every boot after it. That the write
  clears them is verified for QEMU alone, whose store masks both bits out
  (`hw/acpi/ich9_tco.c:167`); whether the PCH's are write-one-to-clear, and
  what the same word does to the rest of that register there, is unread. A T14
  that keeps reporting a reset it already reported is the symptom.

Closed by the metal loop's first run with the watchdog armed: the kernel's own
`watchdog:` line off the log partition answers the first, third and fifth, and
a starved boot that comes back to Linux answers the second and fourth together.
