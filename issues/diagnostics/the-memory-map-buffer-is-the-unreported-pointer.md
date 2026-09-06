---
status: open
kind: defect
opened: 2026-09-06
---

# The one pointer the loader cannot say whether the kernel can reach

`kernel_main` dereferences three firmware pool pointers before `mm::init` gives
it page tables of its own, while the bootloader's 4 GiB boot map is the whole of
what is mapped: the framebuffer (`panic_console::arm`), the boot parameter
(`core::str::from_utf8` on `cmdline_addr`), and the memory map, which
`framebuffer_is_reclaimed_ram` walks. The loader reports the first two before
`ExitBootServices`. A Tiger Lake framebuffer is not below 4 GiB, and nothing
constrains a UEFI pool allocation to stay there either, so a T14 whose memory
map lands high faults in `arm` with nothing on the panel and nothing in
`loader.log` — while both printed lines read "inside".

The third is unreported because its buffer is sized from `memory_map_size()`,
and `loaderlog::close()` runs before that sizing: a console write, a FAT write
and a handle drop each add a descriptor, and the `+ 8` margin is fixed.
Reporting the buffer means allocating it first, which puts the three console
writes and the FAT close between the sizing and the exit.

**Measured, and the margin looks ample.** A `metal-sim` boot asked
`memory_map_size()` immediately before those three writes and again immediately
after `loaderlog::close()`: `MEASURE: entries before 109 after 109 (entry_size
48)`. They add **zero** descriptors against a margin of eight. That is QEMU,
whose FAT write goes to a file the host owns; the T14's goes to a real USB
stick, and no boot has asked the same question there.

Closed by the same reading on the T14, after which the sizing and the
allocation move above the three lines and the third `report_reach` joins them.
