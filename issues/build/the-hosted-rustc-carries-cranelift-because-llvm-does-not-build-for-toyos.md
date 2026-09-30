---
status: open
kind: track
opened: 2026-09-30
---

# The hosted rustc carries Cranelift because LLVM does not build for ToyOS

M3's rustc half of `issues/build/toyos-builds-itself.md`: the ToyOS-hosted
rustc links LLVM, as the host's does, instead of `rustc_codegen_cranelift`
(`src/toolchain.rs`'s `write_config`). *Exit*: a Rust program compiled and run
inside ToyOS by that rustc.

**Blocked on**, each measured against main `649ea51d4` with #637's C sysroot
(`de0de8ee7862147a`, LLVM `849da7d62`):
- The C++ runtime (#637), and linking it beside std:
  `issues/build/a-rust-std-binary-cannot-link-the-cxx-runtime.md`.
- The C library: `issues/build/toyos-libc-lacks-the-posix-surface-llvm-compiles-against.md`,
  `issues/build/the-c-sysroot-has-no-libm-so-llvms-configure-fails.md` and
  `issues/build/libc-mmap-ignores-the-file-it-is-asked-to-map.md`.
- A linker in the guest (M2). toyos-ld refuses every executable with
  thread-local storage, so every std program: `hello.rs`, compiled for
  `x86_64-unknown-toyos` by the host's rustc and linked by a host build of
  toyos-ld, stops at `an executable with thread-local storage: toyos-ld is
  frozen, and places it where the loader does not`, exit 1; the same program
  through `rust-lld`, exit 0. Whatever its backend, the hosted rustc links no
  std program inside ToyOS before LLD runs there
  (`issues/build/the-hosted-rustc-names-a-linker-toyos-does-not-have.md`).
- Testing it before it lands:
  `issues/build/a-worktree-cannot-build-a-hosted-rustc-of-its-own.md`.

Threads, memory and `dlopen` at run time are unmeasured: nothing links to run.
rustc's LLVM wrapper, the 71 LLVM archives it links, `libc++.a` and std,
linked `-shared`, carry 41,131,639 bytes of text (`llvm-size`).

**What the build must also do, read from the fork and not yet run:**
- Bootstrap names no CMake system for a ToyOS target, and its fallback is
  `Generic` (`src/bootstrap/src/core/build_steps/llvm.rs`, `configure_cmake`),
  under which LLVM sets `LLVM_ON_UNIX` to 0
  (`llvm/cmake/modules/HandleLLVMOptions.cmake`) and compiles no `Unix/`
  implementation of `Support`.
- With `clang = true`, bootstrap's LLVM step for a target that is not the host
  panics without `clang-tblgen` in the host's own LLVM build directory, and
  every compiler here links the store's LLVM (`src/llvm.rs`), which bootstrap
  did not build.
- `compiler/rustc_llvm/build.rs` links `stdc++` for a target it does not list,
  links no C library, and finds a cross target's LLVM by replacing the host
  triple in the host `llvm-config`'s paths, which the store's paths do not
  contain.
- LLVM's `llvm/include/llvm/ADT/bit.h` takes `machine/endian.h` on ToyOS, and
  `llvm/lib/Support/Unix/Path.inc`'s `is_local_impl` has no ToyOS arm.
