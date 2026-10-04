---
status: open
kind: finding
opened: 2026-09-02
---

# No way of refusing an interrupt remapping entry can be reached here

`crate::iommu::Refused` has three producers, and no test reaches any of them
because no machine in reach can. `PciDevice::message` turns each into a device
it does not arm, and `ioapic::route` into `RouteError::NotRemappable`.

- **`DestinationTooWide`** — `interrupt.rs`'s `allocate`, when a destination does
  not fit the eight bits `DST` holds without `ECAP.EIM`. It needs an APIC id of
  0xFF or more; every interrupt here targets APIC 0, or APIC 1 under
  `iommu-dest-apic1`. A guest would need more than 255 vCPUs.
- **`TableFull`** from the same function, when all 256 entries are spoken for.
  The richest profile arms five. Its *other* producer, the `table` being `None`,
  is unreachable in a second way: every caller passes `is_armed()` first.
- **`ControllerUnnamed`** — `interrupt.rs`'s `pin`, when firmware's device scopes
  named no requester id for an I/O APIC. `remappable` refuses the whole machine
  unless every one is named, so reaching this needs a MADT and a DMAR that
  disagree between the two reads.

The refusal that preceded them was equally unreached — `RouteError::DestTooWide`
has never had a test either — so this is a weakness inherited and made explicit,
not one introduced.

Exit condition: a test-only ceiling on the table's entry count would reach
`TableFull` in one line, and is not added here because a size that differs
between a test kernel and a shipping one is exactly the kind of harness field
that can go silently inert (`tests/CLAUDE.md`). The honest exits are a guest
with enough vCPUs to make a wide destination real, and firmware — or an
actuator standing in for it — that names one I/O APIC and not another.
