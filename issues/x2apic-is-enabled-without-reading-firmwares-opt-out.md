---
status: open
kind: defect
opened: 2026-10-04
---

# x2APIC is enabled without reading firmware's opt-out

VT-d Rev. 4.1 §8.1, DMAR Flags bit 1 `X2APIC_OPT_OUT`: firmware asks system
software "to opt out of enabling Extended xAPIC (X2APIC) mode", and software
checks it "as part of detecting X2APIC mode support". This kernel puts every
local APIC in x2APIC wherever `CPUID.01H:ECX[21]` offers it, in
`arch::control_regs::init_apic`, reached from `arch::apic::init` in
`arch::boot::interrupts`, before the DMAR is first read in `iommu::init`
(`kernel/src/main.rs`). The flag is logged (`x2apic_opt_out=` on the `iommu:
DMAR` line) and decides nothing.

The owner's ruling on extended interrupt mode
(`issues/the-iommu-refuses-nothing-yet.md`) is about the width of a
remapping entry's destination, not whether x2APIC is on, so it does not cover
this. The T14 clears the flag (`flags=0x05`), so no machine in reach sets it.

The xAPIC arm exists, for CPUs without x2APIC, but no tier runs it beside
VT-d: QEMU's `intel-iommu` takes `eim=on` only with KVM's split irqchip, the
harness's default leaves `eim` off, the T14 runs x2APIC and the AMD laptop has
no VT-d. So `vtd::remappable`'s `x2apic &&`, which keeps `EIME` off under an
xAPIC, passes every test with it deleted.

Owner: the IOMMU track. Exit: the flag is read before `init_apic` declares
the mode, and a machine that sets it runs xAPIC, or is refused by name where
an APIC id does not fit eight bits; an actuator standing in for the flag
reaches both arms on the T14, its xAPIC arm with `EIME` off on a unit that
reports `ECAP.EIM`.
