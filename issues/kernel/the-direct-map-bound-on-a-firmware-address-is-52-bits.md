---
status: open
kind: defect
opened: 2026-09-03
---

# A firmware address the direct map does not cover passes `readable` and faults on the read

`DirectPhys::readable` (`kernel/src/drivers/acpi.rs:37-39`) accepts any physical
address below `MAX_PHYS`, which is `1 << 52` — x86-64's architectural ceiling,
not this machine's. The direct map covers installed RAM only: the bootloader
maps `size` bytes at `PHYS_OFFSET` in `build_boot_page_tables`
(`bootloader/src/main.rs:469-505`), so an XSDT entry naming an address above the
top of memory passes `readable`, reaches `read_volatile` in `DirectPhys::byte`
(`acpi.rs:41-43`), and faults in Ring 0 on firmware's word.

Pre-existing: the shape this replaced bounded `table_at` by the same
`MAX_PHYS`. Nothing has been observed to reach it — every firmware this tree has
booted publishes its tables inside RAM — and the input is untrusted, which is
the whole reason the bound is supposed to be one.

**Exit condition.** `readable` is bounded by the memory map's top rather than by
`MAX_PHYS`, and an address past it is refused by name like every other
`TableError`. The map is already in the kernel: `mm::init` takes
`&[MemoryMapEntry]`, so what is missing is a reader for its ceiling, not the
number.
