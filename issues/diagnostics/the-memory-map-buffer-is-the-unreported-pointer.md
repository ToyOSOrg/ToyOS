---
status: open
kind: finding
opened: 2026-09-06
---

# The one pointer the loader cannot say whether the kernel can reach

`kernel_main` dereferences three firmware pool pointers before `mm::init` gives
it page tables of its own, while the bootloader's 4 GiB boot map is the whole of
what is mapped: the framebuffer (`panic_console::arm`), the boot parameter
(`core::str::from_utf8` on `cmdline_addr`), and the memory map
(`framebuffer_is_reclaimed_ram` walks it). The loader now prints, before
`ExitBootServices`, whether the first two are inside `BOOT_MAP_BYTES` — a Tiger
Lake framebuffer is not, and nothing constrains a UEFI pool allocation to stay
below it either.

The third is not reported. `memory_map`'s buffer is allocated from
`memory_map_size()`, and `loaderlog::close()` has to run before that sizing: a
console write, a FAT write and a handle drop each add a descriptor, and the
`+ 8` margin the sizing carries is fixed. Printing the buffer's extent after it
exists would put a FAT write between the sizing and the exit, which is the thing
that margin is protecting.

So a T14 whose memory map lands above 4 GiB faults in `panic_console::arm` with
nothing on the panel and nothing in `loader.log`, and the two lines that are
printed will both read "inside".

Closed by either half: a sizing whose margin covers one more write, or the
buffer allocated before the last printed line rather than after it.
