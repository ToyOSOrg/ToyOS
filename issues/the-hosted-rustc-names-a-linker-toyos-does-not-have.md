---
status: open
kind: defect
opened: 2026-09-26
---

# The ToyOS-hosted rustc names a linker ToyOS does not have

`x86_64-unknown-toyos` names `rust-lld`, and a rustc built for a ToyOS host
carries that target spec into the guest, where no `rust-lld` exists: LLD runs
on ToyOS only once clang and libc++ do, and no other linker runs there. No
image ships a hosted rustc today, because nothing builds one
(`issues/nothing-builds-the-toyos-hosted-rustc.md`), so nothing links through
it yet.

Exit: the hosted rustc links a program inside ToyOS through a linker the image
carries, and a guest test compiles and runs one.
