---
status: open
kind: defect
opened: 2026-09-30
---

# The C sysroot has no libm, so LLVM's configure fails

LLVM's configure adds `m` to every link probe on a `UNIX` system that is not
Apple, BeOS or Haiku (`llvm/cmake/config-ix.cmake`), and probes `pthread`, `c`,
`dl` and `rt` as libraries. The C sysroot's `lib/` holds `libtoyos_c.a` and no
archive under any of those names, so every probe that links fails.

Configuring `src/llvm-project/llvm` at `849da7d62` for `x86_64-unknown-toyos`,
with CMake's system `ToyOS` and `UNIX=ON`, against #637's C sysroot
(`de0de8ee7862147a`): `unable to find library -lm` 18 times, and exit 1 at
`cmake/modules/CheckAtomic.cmake:59`, "Host compiler appears to require
libatomic, but cannot find it". With empty `libm.a`, `libpthread.a`, `libdl.a`
and `librt.a` beside `libtoyos_c.a`, the same configure exits 0, and these
probes pass that failed: `HAVE_CXX_ATOMICS_WITHOUT_LIB`,
`HAVE_BUILTIN_THREAD_POINTER`, `HAVE_STRUCT_STAT_ST_MTIM_TV_NSEC`,
`C_SUPPORTS_WERROR_UNGUARDED_AVAILABILITY_NEW`, `sysconf`, `isatty`,
`strerror_r`, `setenv`, `dlopen`, `dlopen` in `dl` and `pthread_create` in
`pthread`.

`pread` is not found in either: no header declares it,
and once one does, LLVM reads through
`issues/build/libc-pread-and-pwrite-move-the-offset-another-thread-shares.md`.

**Exit**: that configure exits 0 against the C sysroot as `src/libc.rs` lays it
out, with those eleven probes passing, and a host test links a C probe with
each of `-lm`, `-lpthread`, `-lc`, `-ldl` and `-lrt` against that sysroot with
the toolchain's clang.
