---
status: open
kind: defect
opened: 2026-10-10
---

# VT-d keeps `EIME` off under an xAPIC, and no test reaches it

`arch::x86_64::vtd::remappable` sets extended interrupt mode only where every
unit reports `ECAP.EIM` and the local APICs run x2APIC, because an xAPIC
compares the 8-bit destination field (VT-d Rev. 4.1 Section 9.9). No tier runs
the xAPIC arm beside VT-d: QEMU's `intel-iommu` takes `eim=on` only with KVM's
split irqchip, the harness's default leaves `eim` off, the T14 runs x2APIC and
the AMD laptop has AMD-Vi, not VT-d. So the `x2apic &&` term passes every test
with it deleted.

Owner: the IOMMU track, beside
`issues/x2apic-is-enabled-without-reading-firmwares-opt-out.md`, whose exit
runs xAPIC on the T14. Exit: a T14 row boots its CPUs in xAPIC on its VT-d
unit, which reports `ECAP.EIM`, reads `EIME` off in the unit's log, and goes
red with the `x2apic &&` term deleted.
