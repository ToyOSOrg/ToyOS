---
status: open
kind: defect
opened: 2026-08-29
---

# A TYPE_64 BAR in slot 5 makes the sizing probe write the CardBus CIS pointer

`toyos_pci::bar::decode` answers `Width::Wide` for any register whose Type
field says 64-bit, without knowing which slot it decodes for — but a Type 0
header has six BARs and slot 5 has no neighbour: the dword after it (offset
0x28) is the CardBus CIS pointer. `PciDevice::memory_bar` then reads it as the
address's high half (`kernel/src/drivers/pci.rs:114`), and `bar_size` writes
`u32::MAX` into it and restores it around the probe (`pci.rs:131-134`).
`toyos-pci/src/bar.rs`'s own module header names the read as the shape of the
defect that file was written to end — "for BAR 5 is the CardBus CIS pointer" —
and the sizing probe (this branch's) extended the read into a write.

Found by the adversarial review of PR #339; pre-existing for the read,
extended by the probe. A device publishing a 64-bit BAR in slot 5 is
malformed, which is exactly why it must be refused by name rather than
decoded past: the fix is `decode` (or its callers) refusing `Width::Wide` at
`MAX_INDEX`, with a host vector in `toyos-pci` beside the existing slot-5
sentence, so neither the read nor the write can reach offset 0x28.
