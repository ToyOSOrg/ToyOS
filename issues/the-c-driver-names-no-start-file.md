---
status: open
kind: defect
opened: 2026-09-27
---

# The C driver names no start file

clang's ToyOS driver links a program's objects and `-ltoyos_c`, and lld takes
`_start` out of libc's `staticlib`; `-nostartfiles` is claimed and changes
nothing, so a program that brings its own `_start` gets two.

**Exit**: libc ships `crt0.o` in the C sysroot's `lib/` and no `_start` in its
archive, and the driver names `crt0.o` unless `-nostartfiles`.
