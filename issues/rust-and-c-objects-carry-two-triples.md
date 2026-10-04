---
status: open
kind: defect
opened: 2026-09-27
---

# Rust and C objects for ToyOS carry two triples

rustc's ToyOS targets give LLVM `<arch>-unknown-none-elf` (`llvm_target`), and
clang compiles ToyOS's C as `<arch>-unknown-toyos`, so one binary links objects
of two triples.

**Exit**: both targets' `llvm_target` is `<arch>-unknown-toyos`, and the guest
suite is green on that compiler.
