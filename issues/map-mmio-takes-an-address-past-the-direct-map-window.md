---
status: open
kind: defect
opened: 2026-09-28
---

# `map_mmio` takes an address past the direct map's window

`paging::map_mmio` (`kernel/src/arch/x86_64/paging.rs`) maps `phys` at
`PHYS_OFFSET + phys` and bounds nothing. The direct map's window is
`toyos_bootmap::DIRECT_MAP_WINDOW`, `0x800000000000` (128 TiB): root slots
256 to 511. A firmware-named register base at or past it overflows that sum.
`vtd::window` (`kernel/src/arch/x86_64/vtd/mod.rs`) bounds a DMAR register base
by its own `MAX_PHYS`, `1 << 52`, so a base between the two reaches it; a BAR
in a firmware window past 128 TiB reaches it through `pcidev::probe_dword`.

Nothing has been observed to reach it: every firmware this tree has booted
puts its registers far below.

Owner: orchestrator. Exit condition: `map_mmio` refuses by name an address past
`DIRECT_MAP_WINDOW`, and each firmware-named base is checked against that bound
rather than against the architecture's width.
