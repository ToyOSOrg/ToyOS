---
status: open
kind: defect
opened: 2026-10-01
---

# A shared object does not link against the C sysroot with `-z defs`

ToyOS loads shared objects, a program's `DT_NEEDED` and `dlopen`'s, and its
clang links one with `-shared`. None links against the C sysroot with
`-z defs`. A C one linked without it leaves `__tls_get_addr` undefined, and
lld refuses the executable that links it for that name. LLVM built for
`x86_64-unknown-toyos`
(`issues/bootstrap-cannot-build-llvm-clang-and-lld-for-a-toyos-host.md`)
builds `libLTO`, `libRemarks`, `libclang` and `libclang-cpp`, linked
`-shared -z defs`, and each stops on what the sysroot's archives were built
for, an executable:

- `main`, undefined: `libtoyos_c.a`'s entry point, `start_c`, sits in an
  object the link takes for other names.
- `__tls_get_addr`, undefined: a shared object reaches a thread-local, its own
  or libc's, through it, and only std defines it.
- `R_X86_64_TPOFF32` against libc++abi's `eh_globals`, and `R_X86_64_PC32`
  against libc++'s vtables: the C++ runtime is compiled for an executable
  (`issues/a-rust-std-binary-cannot-link-the-cxx-runtime.md`).

**Exit**: a C and a C++ shared object link against the C sysroot with
`-z defs`, LLVM's four build for ToyOS with their options on, and a guest case
`dlopen`s the C one and calls into it.
