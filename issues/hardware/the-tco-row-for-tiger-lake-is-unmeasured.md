---
status: open
kind: defect
opened: 2026-09-06
---

# `toyos-tco`'s chipset table has no row for the T14, so its watchdog is unarmed

`toyos-tco::CHIPSETS` names where a chipset keeps the TCO block's base port,
and it has one row: QEMU q35's ISA bridge `8086:2918`, whose PMBASE is bits
15:7 of PCI config `0x40` with bit 0 the enable, and whose TCO block sits `0x60`
into that window. Every other machine is refused by name, so
`--kernel-param watchdog` on the T14 logs a refusal and arms nothing.

The T14 needs a second row and it cannot be written from anything in this tree.
Its TCO block is reached through the SMBus function `8086:a0a3` rather than the
LPC bridge — `issues/hardware/the-t14-boots-toyos-unattended.md` records that
Linux 6.8.0-138's `lpc_ich` claims neither of the machine's ids and `i2c_i801`
claims the SMBus one — and the register that holds the base, the bits of it
that are the address, and the enable bit are numbers this repository does not
carry. Writing a guessed base is an I/O write to whatever else answers there.

## What closes it

One reading off the machine, which the metal loop can take over SSH:

```
lspci -nn -s 00:1f.4
sudo setpci -s 00:1f.4 0x50.l 0x54.l
```

The first says the function really is `8086:a0a3`; the second gives the base
register and the one beside it that Linux's `i2c-i801` treats as the enable.
A row asserted against that reading, and a `toyos-tco` test carrying it the way
`the_q35_row_reaches_the_port_qemu_puts_the_block_at` carries QEMU's, is the
fix. Intel's Tiger Lake-LP PCH datasheet would do instead, read rather than
recalled.

QEMU models no SMBus TCO base at all, so nothing about that row is judgeable in
the harness: its judge is the machine, and until it exists a wedged kernel on
the T14 still costs a hand on the power button.
