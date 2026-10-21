---
status: open
kind: defect
opened: 2026-09-28
---

# A panic inside `mm::init` may not reach the panel

On x86-64, from `paging::init`'s CR3 load (`mm::init`, `kernel/src/mm/mod.rs`)
to `drivers::panic_console::remap`, the panel is written through the kernel's
own direct map, which reaches `toyos_bootmap::x86_64::direct_map_end`: the boot
map's 4 GiB, or the end of the highest memory range. A scanout past that end has
no mapping until `remap` maps it, so a panic in that span — `alloc::init`, and
`paging::seal_kernel_half`'s up to 255 table allocations — faults on the first
pixel instead of painting. The extent before it reached every descriptor in the
map, so a scanout the map describes was covered.

AArch64's `paging::init` maps the scanout it is handed before its tables go
live (`kernel/src/arch/aarch64/paging.rs`); x86-64's takes the same argument
and ignores it.

Nothing has been observed to reach it: every firmware this tree has booted
puts its scanout below 4 GiB.

Owner: orchestrator. Exit condition: the scanout is mapped in the kernel's
tables before the CR3 load that makes them live, and a boot actuator that
panics between `paging::init` and `remap` with the scanout above the direct map
paints its report.
