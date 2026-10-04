---
status: open
kind: defect
opened: 2026-10-04
---

# A unit this kernel does not program keeps what firmware left on

`vtd::init` (`kernel/src/arch/x86_64/vtd/mod.rs`) switches off what firmware
left on (`Unit::hand_over`) only inside `enable`, which a unit reaches only if
`plan` accepts it and it is among the first `MAX_UNITS` described. A unit
`plan` refuses, and every unit past `MAX_UNITS`, keeps whatever firmware left
on while the kernel treats it as absent: translation through firmware's tables,
and interrupt remapping, which blocks every compatibility-format message this
kernel then sends through the unit unless firmware also left `CFI` on with
`EIME` off (VT-d Rev. 4.1 §5.1.4). The module header's "leaves the unit
switched off" is false of such a unit.

Evidence: read off the tree; no machine here has one. The T14's four units are
all programmed, and its firmware leaves each at `gsts=0x40000000`.

**Exit**: a boot that leaves a unit translating, remapping and queueing and
then refuses its plan logs each of those going off and boots on, judged on
QEMU.
