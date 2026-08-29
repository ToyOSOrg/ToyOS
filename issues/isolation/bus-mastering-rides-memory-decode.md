---
status: open
kind: defect
opened: 2026-08-29
---

# Three drivers parse their device bus-master-armed, because memory decode and bus mastering are one helper

Found sweeping every `enable_bus_master` site for PR #340's virtio ordering
fix. `PciDevice::enable_bus_master` (`kernel/src/drivers/pci.rs:150`) sets
COMMAND bits 1 and 2 together (`cmd | 0x06`), so a driver that needs memory
decode to *read its device's registers at all* cannot get it without also
arming DMA. The virtio family parses from config space and needed neither,
which is why parse-before-enable closed cleanly there; the other three
cannot:

- `nvme.rs:717` enables after its one config-space refusal (BAR 0 not
  memory) and before every MMIO read the controller answers — and its later
  bind refusals (*"a controller refusing any of these has given no namespace
  to serve"*) return with the bit still set.
- `xhci/wait/boot.rs:211` enables after the same BAR 0 check and before the
  extended-capability walk in BAR space; its `arm_interrupt` refusal returns
  `None` with the bit still set.
- `hda.rs:445` enables after its refusals but its `arm_interrupt` failure on
  the next line returns with the bit still set.

So a device those drivers decline to bind keeps bus mastering — the end
state the virtio close removed, still reachable one device class over — and
every MMIO-side parse in those drivers runs against a device that could
already master the bus. `src/sourcegate.rs` pins the exact set of
`enable_bus_master` sites so the set cannot grow silently; making the pinned
sites *right* is this issue.

Fix direction: split the helper — memory decode (bit 1) armed to read BAR
registers, bus mastering (bit 2) armed only at each driver's trust point,
where its refusals are behind it — and `disable_bus_master` on the bind
refusal paths that currently walk away armed. Each driver's trust point is
its own design decision; none of the three has a crafted-config selftest the
way virtio does, so the checks owe their own instruments.
