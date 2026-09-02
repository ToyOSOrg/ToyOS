---
status: open
kind: finding
opened: 2026-09-02
---

# Neither way of refusing an interrupt remapping entry can be reached here

`kernel/src/iommu/vtd/interrupt.rs`'s `allocate` refuses two ways, and
`crate::iommu::Refused` carries which: `DestinationTooWide`, when a destination
does not fit the eight bits `DST` holds without `ECAP.EIM`, and `TableFull`,
when all 256 entries are spoken for. `PciDevice::message` turns either into a
device it does not arm, and `ioapic::route` into `RouteError::NotRemappable`.

No test reaches either arm, and no machine in reach can.

- **`DestinationTooWide`** needs an APIC id of 0xFF or more. Every interrupt on
  every profile targets APIC 0: `pci.rs`'s `MSG_DEST` is 0 and the i8042 routes
  to `apic::id()` on the boot CPU. A guest would need more than 255 vCPUs.
- **`TableFull`** needs 256 armed sources. The richest profile has five.

The refusal that preceded them was equally unreached — `RouteError::DestTooWide`
has never had a test either — so this is a weakness inherited and made explicit,
not one introduced.

Exit condition: a test-only ceiling on the table's entry count would reach
`TableFull` in one line, and is not added here because a size that differs
between a test kernel and a shipping one is exactly the kind of harness field
that can go silently inert (`tests/CLAUDE.md`). The honest exits are a guest
with enough vCPUs to make a wide destination real, or the point at which
per-CPU interrupt affinity gives a source a destination that is not 0 — which
is also what would discriminate the two destination encodings recorded in
`issues/kernel/qemu-passes-compatibility-format-interrupts.md`.
