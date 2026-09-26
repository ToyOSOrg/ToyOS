---
status: open
kind: defect
opened: 2026-09-26
---

# The boot map reaches 4 GiB, and firmware decides what lands in it

`toyos-bootmap`'s plan maps physical `0..BOOT_MAP_BYTES` (4 GiB) plus the
scanout, and everything the kernel touches before `mm::init` has to be inside
it. The loader places none of it there: the kernel image
(`alloc_kernel_memory`), the memory map it hands over, the boot parameter and
ROOT's image all come from the pool allocator or `AnyPages`, wherever firmware
puts them. `report_reach` then refuses the boot by name for the three it
checks — and does not check the memory map at all, which the kernel reads
through the direct map before `mm::init` too.

On q35 OVMF allocates below 4 GiB, so the x86 suite has never met it. On QEMU
`virt` RAM starts at 1 GiB and AAVMF allocates from its top: with `-m 4G`
the kernel image lands at 0x13b400000 and the loader refuses the boot
("Kernel image at 0x13b400000+0xb07000 is outside the boot map"). The
harness's `Profile::Virt` boots with 2 GiB for that reason
(`tests/common/qemu.rs`, `qemu_command`). A real machine of either
architecture with RAM above 4 GiB and a firmware that allocates top-down is
the same defect.

**Exit condition**: the loader allocates what the kernel reads before
`mm::init` with `AllocateType::MaxAddress` inside the map (or the plan grows to
reach all of firmware's write-back memory), the memory map is among what
`report_reach` checks, and `Profile::Virt` boots with the 4 GiB every other
profile has.
