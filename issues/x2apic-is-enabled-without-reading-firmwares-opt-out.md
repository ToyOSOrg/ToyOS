---
status: open
kind: defect
opened: 2026-10-04
---

# x2APIC is enabled without reading firmware's opt-out

VT-d Rev. 4.1 §8.1, DMAR Flags bit 1 `X2APIC_OPT_OUT`: firmware asks system
software "to opt out of enabling Extended xAPIC (X2APIC) mode", and software
checks it "as part of detecting X2APIC mode support". This kernel enables
x2APIC unconditionally in `arch::apic::init` (`enable_x2apic`), reached from
`arch::boot::interrupts`, before the DMAR is first read in `iommu::init`
(`kernel/src/main.rs`). The flag is logged (`x2apic_opt_out=` on the `iommu:
DMAR` line) and decides nothing.

The owner's ruling on extended interrupt mode
(`issues/the-iommu-refuses-nothing-yet.md`) is about the width of a
remapping entry's destination, not whether x2APIC is on, so it does not cover
this. The T14 clears the flag (`flags=0x05`), so no machine in reach sets it.

Owner: the IOMMU track. Exit: the flag is read before the local APIC is put in
x2APIC mode, and a machine that sets it either runs xAPIC or is refused by
name; an actuator standing in for the flag reaches both arms on the T14.
