---
status: open
kind: defect
opened: 2026-09-27
---

# The kernel reserves its stack's offset as a physical region

`KernelArgs::kernel_stack_addr` is an offset into the kernel's own allocation:
the loader sets it to `StackedImage::place`'s `stack` (`bootloader/src/main.rs`),
and both entries add it to `kernel_memory_addr` before loading the stack
pointer (`kernel/src/arch/x86_64/boot.rs`'s `_start`,
`kernel/src/arch/aarch64/boot.rs`).

`kernel/src/main.rs`'s `reserved` list takes it as an address:
`mm::Region { start: kernel_stack_addr, end: kernel_stack_addr +
kernel_stack_size }`. On the rust-lld kernel that is physical
`0x411000..0xc11000`, low memory that is nobody's stack, withheld from the
allocator for the whole boot. The real stack is inside `kernel_memory_addr ..
+ kernel_memory_size`, which the first entry of the same list already reserves,
so nothing is handed out twice; the defect is 8 MiB of memory reserved for
nothing, at whatever physical range the image's size names.

Exit: the region goes, or names `kernel_memory_addr + kernel_stack_addr`, and
a host-checkable decision or a boot assertion states which physical ranges the
reserved list covers.
