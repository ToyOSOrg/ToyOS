---
status: open
kind: defect
opened: 2026-09-26
---

# MSI and pin routing take an x86 vector and APIC id in generic code

`kernel/src/drivers/pci.rs`'s `enable_msix` and `enable_msi` take
`vector: u8`, and `kernel/src/iommu/mod.rs`'s `remap_pin` takes
`apic_id: u8` beside it. Both are x86's interrupt addressing: an 8-bit IDT
vector delivered to an xAPIC id. A GICv3 message names an LPI through the ITS
(a 32-bit event id, translated to an INTID above 8191) and a redistributor,
and neither fits a `u8`.

Owned by stage 4 of `issues/kernel/toyos-runs-on-arm64.md`, which brings up
the GIC and its ITS.

**Exit condition**: a PCI function's interrupt is programmed from an
arch-provided message (address and data, as `arch::msi` already provides the
doorbell), the routing names its target by the arch's own type, and no
generic signature carries `vector: u8` or `apic_id`.
