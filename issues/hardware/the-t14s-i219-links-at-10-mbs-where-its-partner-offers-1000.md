---
status: open
kind: defect
opened: 2026-09-23
---

# The T14's I219 links at 10 Mb/s where its partner offers 1000

Metal run 112 brought the PHY up and got a link: `exit: netd pid=5 code=66`,
`toyos_i219::phy::Outcome::LinkAt10Full`. Ubuntu on the same machine and cable
reports `enp0s31f6 carrier=1 speed=1000`. The bring-up advertises
10/100 half and full (register 4, `0x01e1`) and 1000 full (register 9,
`0x0200`) and restarts auto-negotiation, so the advertisement is not what
chose 10.

## What the documents say decides it

I219 datasheet (612523, rev 2.02) §8.1 and §9.5.8.2: the OEM Bits register
(PHY address 01, register 25) carries Low Power Link Up (bit 2),
under which auto-negotiation resolves in the order 10 full, 10 half, 100 full,
100 half — **10 full first, which is what the part came up at** — and
1000 Mb/s disabled (bit 6), whose footnote says its value
"becomes 1b" when `PE_RST_N` goes low and the PHY switches to SMBus. Both take
effect only on a PHY soft reset or on the register's own restart of
auto-negotiation (bit 10), and both can be set by MDIO write or by "signal
toggling" from the MAC.
§10.3.1.12: NVM word 0x16 bit 12, OEM Write Enable, has the MAC load the OEM
bits from `PHY_CTRL` into the PHY (`EXTCNF_CTRL` bit 3); the T14 reads
`EXTCNF_CTRL 0x00300089`, so that load is on. `PHY_CTRL` reads `0x0004000c`:
GbE disable and LPLU both set for non-D0a states (PCH Vol 2 631120 §8.2.5).

So three agents can have put LPLU into the PHY before netd asked it anything:
the previous operating system's shutdown, the MAC's own load of `PHY_CTRL`'s
non-D0a bits, and this driver's wake, which forces SMBus on the T14 (run 112,
rung `smbus-forced`) and so sets 1000 Mb/s disabled by the footnote above.

## What decides the speed

The linked speed is decided by two bits of OEM Bits (PHY address 01, register
25), Low Power Link Up (bit 2) and 1000 Mb/s disabled (bit 6), on the
authority of I219 §9.5.8.2 above; the D0a values the MAC holds for them are
`PHY_CTRL` bit 1 and bit 6 (PCH Vol 2 631120 §8.2.5), both clear on the T14.
Linux's driver for this family sets those two bits at link time, and reaches
the register at page 768 of PHY address 01, where the datasheet's table prints
page 0. This driver sets neither.

## Why it is not fixed in the branch that found it

The first byte on this machine (PR #453's lease probe) is judged at 10 Mb/s so
that its verdict is about frames and not about the two bits above,
which this driver does not set, and the page disagreement is one only the part can settle.

## Owner

The I219 bring-up's author, and after it the network track.

## What would close it

A T14 boot that reads OEM Bits at page 768 and page 0 of PHY address 01
before and after the bring-up, writes the D0a bits with the restart of
auto-negotiation, and links at 1000 Mb/s full duplex with the lease probe still
answering.
