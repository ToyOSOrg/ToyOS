---
status: open
kind: defect
opened: 2026-09-03
---

# A domain's addresses are never given back

`Domain::reserve` (`kernel/src/iommu/vtd/table.rs:213-224`) is a monotonic
bump: an address is never handed out twice, unmapped or not, and `unmap`
returns no range. A driver that maps and unmaps repeatedly therefore consumes
its domain's device address space for good. The display is the first such
driver — every `set_resolution` maps a new scanout and unmaps the old one, and
one boot with three mode changes walked `device=0x400000200000`,
`0x400000800000`, `0x400000a00000`, `0x400000e00000` with nothing between them
reusable. From `first_address = 1 << (bits - 2)` and the `1 << bits` ceiling, a
39-bit unit (the harness's `narrow` shape) has 196,608 leaves to give before a
mode change is refused with `ResourceExhausted`; the refusal is the right
answer to exhaustion and the wrong thing to reach by changing modes.

Exit: `unmap` returns its range to a free list, or the reservation is
per-attachment and recycled when the attachment ends — either way a domain's
consumption is bounded by what it has mapped at once, not by what it ever
mapped.
