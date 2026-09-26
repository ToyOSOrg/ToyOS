---
status: open
kind: defect
opened: 2026-09-26
---

# The ToyOS-hosted rustc names a linker ToyOS does not have

`x86_64-unknown-toyos` names `rust-lld`, and the rustc `src/toolchain.rs`
builds for a ToyOS host carries that target spec into the guest, where no
`rust-lld` exists: LLD runs on ToyOS only once clang and libc++ do. The linker
that does run there is the frozen `/system/bin/toyos-ld`, which that rustc
reaches only when told `-C linker=toyos-ld`. No image ships the hosted rustc
today — `src/build.rs` refuses `hosted-rustc = true` until its licences are
read — so nothing links through it yet.

Exit: the hosted rustc links a program inside ToyOS through a linker the image
carries, and a guest test compiles and runs one.
