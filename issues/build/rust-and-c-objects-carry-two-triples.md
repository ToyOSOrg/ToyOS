---
status: open
kind: defect
opened: 2026-09-27
---

# Rust and C objects for ToyOS carry two triples

rustc's ToyOS targets tell LLVM they are `x86_64-unknown-none-elf` and
`aarch64-unknown-none-elf` (`rust/compiler/rustc_target/src/spec/targets/*_unknown_toyos.rs`,
`llvm_target`), from before LLVM knew ToyOS. clang compiles ToyOS's C as
`x86_64-unknown-toyos`, which the LLVM fork now knows. A binary linking both
holds objects of two triples, and nothing checks that the two agree on what
the target means to LLVM — the TLS model, the relocation defaults, the data
layout.

**Exit**: both targets' `llvm_target` is `<arch>-unknown-toyos`, and the guest
suite is green on that compiler.
