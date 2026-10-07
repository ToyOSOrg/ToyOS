---
status: open
kind: defect
opened: 2026-10-07
---

# `fstat`'s answer is declared twice, and nothing holds the two layouts together

`SYS_FSTAT` copies out `kernel/src/object/ops.rs`'s `Stat`, three `u64`s, and
`toyos_abi::syscall::fstat` reads the answer into `toyos-abi/src/syscall.rs`'s
`Stat`, whose first field is `FileType`, a `#[repr(u64)]` enum. The kernel's
copy exists because a struct holding the enum is not valid for every bit
pattern and so cannot be `UserSafe`; it is sound, since the kernel only
writes it, with `FileType::… as u64`.

Read, nothing run: the two declarations agree today field for field. A field
added to one and not the other compiles on both sides, and userland then reads
a layout the kernel did not write.

## Exit condition

One declaration of the answer's layout that both the kernel's copy-out and
`toyos_abi::syscall::fstat` read, or a compile-time assertion beside one of
them that fails when the two differ in size or in any field's offset.

## Owner

`toyos-abi/src/syscall.rs`, `kernel/src/object/ops.rs`. Nobody holds it.
