---
status: open
kind: finding
opened: 2026-09-23
---

# `EXTCNF_CTRL` bit 7 is read as manageability's MDIO ownership on a MAC where it gates the PHY's automatic configuration

`toyos-i219/src/regs.rs`'s `extcnf::MDIO_MNG_OWNERSHIP` names bit 7 of
`EXTCNF_CTRL` after the 82574's §4.5.2, and `phy::Others`, the
`SoftwareFlagStoodBeside*` refusals and exit codes 73 to 75 read it that way on
the T14 too. The PCH's own datasheet (631120, rev 002, §8.2.4) calls bit 7
reserved, and Intel's Linux host driver for this family uses that bit to gate
the PHY's automatic configuration by hardware — setting it before it touches
the PHY on every part from the 82579 on.

The T14 has read `EXTCNF_CTRL` as `0x00300089` (runs 101 and 102), and the
readings in the tracker and in `phy.rs`'s `Owned::claim` doc call that
"manageability's own bit standing". On this reading it is the gate standing,
which is the state that driver leaves behind, and says nothing about the
Management Engine.

## Owner

The I219 bring-up's author.

## What would close it

`Others` and the refusals it feeds named for what the bit is on each part, or
the 82574 reading confined to the 82574 — with no exit code renumbered, since
every code is on some boot log.
