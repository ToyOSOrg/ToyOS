---
status: open
kind: defect
opened: 2026-10-21
---

# The HDMI audio function 1002:1637 is driven at no address width

The AMD bench's HDMI audio function `03:00.1` (`1002:1637`) sets GCAP's 64OK
(`0x4001`, HDA 1.0a §3.3.2) and keeps all 32 bits of its upper base registers.
But it issues only bits 0 to 39 of an address, in both its command engine and
its stream engine. Bit 40 and every bit above it are dropped, and bit 39 is
issued as programmed.

So its 64OK is contradicted, and `hda` refuses the function by name
(`address_bits` in `system/hda/driver/src/controller.rs`) rather than state a
width for it. The 40 bits are this machine's measurement, not a property of
every `1002:1637`: neither a vendor document nor an AMD engineer's code has
been read for it.

Evidence: the probe boot at `c8acfbc07` on the AMD bench (LENOVO 82A2, BIOS
DMCN38WW), the link and stream ladders and the bit patterns:
https://github.com/ToyOSOrg/evidence/tree/30bf1f4f70c2ce4771f0e7393fcc9a64d1c08cd1/probe
(`amd-round1/kernel.log` lines 396–501; the probe itself is
`c8acfbc07-probe.patch`).

Exit: a width for `1002:1637` that an independent source supports. That is
AMD's documentation, or AMD's own code read clean-room with the fact's
provenance in evidence. `hda` then drives the function at that width, and the
AMD bench's `testcases` boot walks `03:00.1`'s codec with no `DMA FAULT`.
