---
status: open
kind: defect
opened: 2026-09-28
---

# `kernel_stack_addr` names an offset, not an address

`toyos_abi::boot::KernelArgs::kernel_stack_addr` holds an offset into the
kernel's own image — `StackedImage::place`'s `stack` — never a physical
address. Both architectures' entry code already treat it as an offset
(`kernel/src/arch/aarch64/boot.rs` reads it into a local named
`stack_offset`); `kernel/src/main.rs` reads it under its `KernelArgs` name and
must not repeat the confusion the name invites.

Owner: the author of PR #583 (`wt/toyos-loader1`), which is already changing
`KernelArgs`'s layout.
Exit: the field renamed to `kernel_stack_offset` (or equivalent), landed as
part of that change so `KernelArgs` moves once.
