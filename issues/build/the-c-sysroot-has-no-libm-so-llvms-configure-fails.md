---
status: open
kind: defect
opened: 2026-09-30
---

# The C sysroot has no libm, so LLVM's configure fails

LLVM's configure adds `m` to every link probe on a `UNIX` system that is not
Apple, BeOS or Haiku (`llvm/cmake/config-ix.cmake`), and probes `pthread`,
`dl` and `rt` as libraries. The C sysroot's `lib/` holds `libtoyos_c.a` and no
archive under any of those names, so every probe that links fails.

Configuring `src/llvm-project/llvm` at `849da7d62` for `x86_64-unknown-toyos`,
with CMake's system `ToyOS` and `UNIX=ON`, against #637's C sysroot
(`de0de8ee7862147a`): `unable to find library -lm` 18 times, `sysconf`,
`isatty` and `setenv` reported absent though `libtoyos_c.a` defines them, and a
stop at `cmake/modules/CheckAtomic.cmake:59`, exit 1. With empty `libm.a`,
`libpthread.a`, `libdl.a` and `librt.a` beside `libtoyos_c.a`, the same
configure exits 0.

**Exit**: that configure exits 0 against the C sysroot as `src/libc.rs` lays it
out, and finds every function `libtoyos_c.a` defines.
