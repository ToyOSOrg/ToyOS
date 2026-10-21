---
status: open
kind: defect
opened: 2026-09-03
---

# A domain's addresses are never given back

A domain's device addresses are a hand-out-once window
(`kernel/pure/iova.rs`, `Window::reserve`, through
`kernel/src/iommu/domains.rs`'s `Domains::map`): an address is never handed
out twice, unmapped or not, and an unmap returns no range. A driver that maps
and unmaps repeatedly therefore consumes its domain's device address space for
good. The kernel's virtio-gpu driver was the first such driver — every mode
change mapped a new scanout and unmapped the old one, and one boot with three
mode changes walked `device=0x400000200000`, `0x400000800000`,
`0x400000a00000`, `0x400000e00000` with nothing between them reusable; the
compositor lends its scanouts now, in a window whose addresses are lent again
(`kernel/src/pcidev/mod.rs`). A window runs from the second leaf to
`1 << bits` less the reserved ranges, so a 39-bit unit has under 262,144
2 MiB leaves to give before a map is refused with `AddressesExhausted`; the
refusal is the right answer to exhaustion and the wrong thing to reach by
remapping. A claim's grants are the case
`issues/a-claim-spends-device-addresses-its-slot-never-gets-back.md` holds.

Exit: `unmap` returns its range to a free list, or the reservation is
per-attachment and recycled when the attachment ends — either way a domain's
consumption is bounded by what it has mapped at once, not by what it ever
mapped.
