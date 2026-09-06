---
status: open
kind: defect
opened: 2026-09-06
---

# `toyos-tco`'s Tiger Lake row has never been read off the T14

`toyos-tco::CHIPSETS` carries a row for the T14's SMBus function `8086:a0a3`:
config dword `0x50` masked with `!1` is the TCO block's 32-byte I/O base, and
`0x54` bit 8 must be set or the row refuses by name. Its reference is Linux
v6.8's `drivers/i2c/busses/i2c-i801.c`, and the decode around it is held by
`toyos-tco/tests/decode.rs`.

**What has never happened is the read.** Nobody has run
`sudo setpci -s 00:1f.4 0x50.l 0x54.l` on the machine, so it is unknown whether
its firmware enables the block at all or what base it programmes; and QEMU
models no SMBus TCO base, so nothing in the harness exercises this row. A
`--kernel-param tco-arm` boot on the T14 either logs a port and arms or logs a
refusal, and which is not known.

Closed by the metal loop's first run with the watchdog armed: the kernel's own
`watchdog:` line off the log partition says which happened, and a starved boot
that comes back to Linux says the reset works.
