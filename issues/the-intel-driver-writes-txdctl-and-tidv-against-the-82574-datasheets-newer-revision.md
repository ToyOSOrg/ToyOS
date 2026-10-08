---
status: open
kind: defect
opened: 2026-10-08
---

# The Intel driver writes `TXDCTL` and `TIDV` against the 82574 datasheet's newer revision

`toyos-i219`'s header cites revision 2.7 of the *Intel 82574 GbE Controller
Family Datasheet*. The copy intel.com serves is revision 3.4 (June 2014), and
it reads differently in two places the driver's bring-up writes:

- `TXDCTL` bit 22: "Reserved. Must be set to 1b for proper operation." The
  revision history gives it to revision 3.1 ("changed bit 22 value to 1b").
  `regs::txdctl::SUGGESTED` is `GRAN` and a `WTHRESH` of 1, written whole, so
  the bit is cleared.
- `TIDV`: "Counts in units of 1.024 microseconds. A value of 0 is not allowed."
  `open` writes 0, which is also the register's reset value; the same section
  says a descriptor with `IDE` clear, which is every one the driver publishes,
  is not delayed by it.

Revision 3.4 also numbers `TXDCTL` §10.2.6.11 where `regs.rs` cites §10.2.6.10;
every other section the tree cites that was compared sits at the same number.

Neither has been seen to matter: the T14 has sent frames with both. Whether
either register means the same on the PCH's MAC no document says (631120 §8.2
publishes neither).

## Owner

Stage 2 of `issues/the-lan-is-not-yet-production-grade.md`.

## Exit

`toyos-i219`'s header names the revision its citations were last read in, the
bring-up writes `TXDCTL` with bit 22 set on the 82574 and leaves `TIDV` alone,
and the stub refuses a `TXDCTL` write that clears the bit.
