---
status: open
kind: defect
opened: 2026-09-30
---

# A Rust std binary cannot link the C++ runtime

LLVM is C++, so a rustc that carries it links the C++ runtime into
`librustc_driver.so` beside std. The runtime #637 builds into the C sysroot,
`lib/libc++.a` (sysroot `de0de8ee7862147a`), does not link there. Linking
rustc's LLVM wrapper (`compiler/rustc_llvm/llvm-wrapper`, whole), the 71 LLVM
archives rustc links, `libc++.a` and the Rust sysroot's archives with `ld.lld
-shared` gives:

- **Two unwinders.** `libc++.a` carries LLVM's libunwind
  (`LIBCXXABI_ENABLE_STATIC_UNWINDER`) and std carries the `unwinding` crate,
  and both define the Itanium `_Unwind_*` interface: 17 duplicate symbols,
  `_Unwind_Resume` among them, at libunwind's `UnwindLevel1.c` and at
  `unwinding`'s `src/unwinder/mod.rs:346`.
- **Local-exec thread-locals.** libc++abi's `eh_globals` is reached through
  `R_X86_64_TPOFF32`, twice `cannot be used with -shared`: the runtime is
  compiled as position-independent executable code, not for a shared object.

**Exit**: a Rust std shared object and a Rust std executable each link the C++
runtime with one unwinder and no relocation error, and a guest case runs a Rust
std program in which C++ code throws and catches an exception.
