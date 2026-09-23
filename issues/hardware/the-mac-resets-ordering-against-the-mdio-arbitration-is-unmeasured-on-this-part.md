---
status: open
kind: finding
opened: 2026-09-16
---

# The MAC reset's ordering against the MDIO arbitration is unmeasured on this part

`toyos-i219/src/lib.rs`'s `open` now resets the I219 the way Intel's own
Linux host driver for this family does (`toyos-i219/src/wake.rs`'s header):
`CTRL.RST` with `CTRL.PHY_RST` beside it where `FWSM` bit 6 allows, written
under the software flag, then 20 ms with no register access, then a bounded
wait on `STATUS.LAN_INIT_DONE`. The 82574 path is unchanged: `RST` alone,
§10.2.2.1's microsecond, a poll. Two questions stay open for the part in the
PCH, because the host driver's behaviour is one implementation's and not a
document:

1. whether the reset has to be issued under the software flag, or the flag is
   only that driver's habit;
2. whether a read inside the 20 ms really hangs the part, as that driver says
   of the family, given that runs 99 to 111 each read `CTRL` a microsecond after
   a MAC-alone reset and came back.

The 82574 document this driver cites answers neither for this part: it says
only that the bit is self-clearing and that designers "must wait approximately
1 µs" before checking it. The I219 datasheet (612523, rev 2.02) Table 5-3 gives
`TPHY_Reset`, "reset de-assertion to PHY reset complete", as at most 10 ms, and
nothing about the MAC side.

## Evidence

Metal run 51 issued exactly this reset with the PHY sequence absent and the
machine was healthy: `Boot: complete (3199ms)`, ssh back in 84 s, readback
written, stick survived. One boot.

## Owner

The I219 bring-up's author, and after it the network track.

## What would close it

A trail of the full reset from the T14 that comes back, beside the scout's
MAC-alone reset on the same boot (`toyos-i219/src/scout.rs`), and a boot that
reads `CTRL` inside the 20 ms and comes back or does not. An answer that rests
on one boot is one boot, and the failure mode it would be wrong about is the
machine not coming back.
