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
publishes neither). The transmit wake rests on a write-back per descriptor
with bit 22 clear, and on the I219 the reading of that is the T14 boot
`issues/the-intel-nics-room-and-wake-are-unread-on-hardware.md` exits on.

**Now the T14's upload lead.** With #1075 setting every `TCTL` field,
the upload reads 82–87 MB/s against Linux's 110–111, and the transmit ring
still sits full 8.3–8.7 s of each 15 s row with no completion unreported
(`issues/netstack-sends-tcp-at-a-third-of-what-the-t14s-wired-link-carries.md`).
Two of `TXDCTL`'s fields bear on how fast the part takes what it is given:

- bit 22, which rev 3.4 says "Must be set to 1b for proper operation";
- `PTHRESH` and `HTHRESH`, at 0: §7.2.4.1 fetches "as soon as any
  descriptors are made available" only "when the on-chip descriptor queue
  is empty", and prefetches more only "if the number of on-chip
  descriptors drop below TXDCTL.PTHRESH", with "at least HTHRESH valid
  descriptors in host memory".

§10.2.6.11's initial values are all 0, bit 22 among them, so by the
document the bring-up's whole write clears nothing the reset set; what the
T14's part comes out of reset with is unread. A change to these reads
against the current driver in one interleaved session.

## Owner

Stage 2 of `issues/the-lan-is-not-yet-production-grade.md`.

## Exit

`toyos-i219`'s header names the revision its citations were last read in, the
bring-up writes `TXDCTL` with bit 22 set on the 82574 and leaves `TIDV` alone,
and the stub refuses a `TXDCTL` write that clears the bit.
