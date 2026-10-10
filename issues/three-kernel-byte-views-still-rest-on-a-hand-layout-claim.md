---
status: open
kind: defect
opened: 2026-10-08
---

# Three kernel byte views still rest on a layout claim made by hand

`toyos_abi::usersafe::bytes` is the one view of a `UserSafe` value as bytes,
and `user_safe!` is what proves no byte of the value is padding. Three structs
the kernel copies out are not declared through the macro, and each reaches
user memory through an `unsafe` `from_raw_parts` of the kernel's own whose
`SAFETY` comment cites an assertion beside the declaration:

| struct (`toyos-abi/src/pci.rs`) | the kernel's view | what the comment rests on |
|---|---|---|
| `DmaGrant` | `grant_bytes`, `kernel/src/syscall/device.rs` | `size_of == 4 + 4 + 8 + 8`, a sum written by hand |
| `DmaMapping` | inline in `sys_device_dma_map`, the same file | `size_of == 8 + 8`, a sum written by hand |
| `DeviceIrqRecord` | `record_bytes`, `kernel/src/object/ops.rs` | `SIZE == 4 + 4 + 8 + 8`, a sum written by hand |

Read, nothing run: none of the three has padding today. Each assertion
compares the struct's size with a number a person wrote, and nothing ties
that number to the fields: a field added to any of the three fails the build
until the number is moved, and passes once it is moved to the new size,
whether or not that size holds a gap. The bytes of a gap are the kernel's
stack.

Found while the eleven `as_bytes` in `toyos-abi` moved to `usersafe::bytes`;
these three were outside that change's brief.

## Exit condition

The three are declared through `user_safe!`, so a byte of any that belongs to
no field fails the build with the macro's message; the kernel's three
`from_raw_parts` over them are `usersafe::bytes`, and the two hand sums are
deleted.

## Owner

`toyos-abi/src/pci.rs`, `kernel/src/syscall/device.rs`,
`kernel/src/object/ops.rs`. Nobody holds it.
