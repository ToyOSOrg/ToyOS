---
status: open
kind: finding
opened: 2026-09-16
---

# The MAC reset's ordering against the MDIO arbitration is unmeasured on this part

`toyos-i219/src/lib.rs`'s `open` writes `CTRL.RST` without holding §4.5.2's
MDIO ownership bit, waits §10.2.2.1's microsecond, and then polls `CTRL` for
the bit to clear. Two questions about the part in the PCH are open:

1. whether the MAC reset may be issued while the Management Engine is
   mid-transaction on the PHY the reset reaches, or has to be issued under the
   software ownership bit;
2. whether a register read may follow `CTRL.RST` at once, or the part wants a
   settling interval with no access at all.

The 82574 document this driver cites answers neither for this part: it says
only that the bit is self-clearing and that designers "must wait approximately
1 µs" before checking it. The I219's own document is on this machine and is
encrypted; nothing here claims its contents.

## Evidence

Metal run 51 issued exactly this reset with the PHY sequence absent and the
machine was healthy: `Boot: complete (3199ms)`, ssh back in 84 s, readback
written, stick survived. One boot.

## Owner

The I219 bring-up's author, and after it the network track.

## What would close it

A read of the I219 document's reset section, or a measurement: a `lancase`
boot at a head that takes the ownership bit before `CTRL.RST` and waits rather
than polls, beside the arm that does neither. An answer that rests on run 51
alone is one boot, and the failure mode it would be wrong about is the machine
not coming back.
