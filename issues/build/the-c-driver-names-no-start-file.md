---
status: open
kind: defect
opened: 2026-09-27
---

# The C driver names no start file

clang's ToyOS driver (`rust/src/llvm-project`'s
`clang/lib/Driver/ToolChains/ToyOS.cpp`) links a program's objects and
`-ltoyos_c` and nothing else: libc's `staticlib` defines `_start`, and lld pulls
it out of the archive as the entry. Every other ELF driver names a `crt0.o` (or
`crt1.o`) for that, and `-nostartfiles` is what leaves it out; here the driver
claims `-nostartfiles` and does nothing with it, because there is nothing to
leave out. A program that brings its own `_start` and passes `-nostartfiles`
gets two definitions, and a link that must not take the entry from libc cannot
say so. `c_hello` is what measures the interim.

**Exit**: libc ships `crt0.o` in the C sysroot's `lib/` and no `_start` in its
archive, and the driver names `crt0.o` unless `-nostartfiles`, with the lit
test `clang/test/Driver/toyos.c` checking both.
