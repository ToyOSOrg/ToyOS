---
status: open
kind: defect
opened: 2026-09-30
---

# LLVM and rustc's build have no arm for a ToyOS host

Read from the fork at `rust/` and from `src/llvm-project` at `849da7d62`, not
run. The first two stop any LLVM built for a ToyOS host, so M2's clang and lld
(`issues/build/toyos-builds-itself.md`) before M3's rustc; the last two are
rustc's alone.

- **Configure.** Bootstrap names no CMake system for a ToyOS target and falls
  back to `Generic` (`src/bootstrap/src/core/build_steps/llvm.rs`,
  `configure_cmake`), under which LLVM sets `LLVM_ON_UNIX` to 0
  (`llvm/cmake/modules/HandleLLVMOptions.cmake`) and compiles no `Unix/`
  implementation of `Support`.
- **Compile.** `llvm/include/llvm/ADT/bit.h` includes `<endian.h>` on the
  systems it lists and `<machine/endian.h>` on any other, ToyOS among them, and
  `llvm/lib/Support/Unix/Path.inc`'s `is_local_impl` reads the BSDs'
  `MNT_LOCAL` on a system it does not list.
- **Build.** With `clang = true`, bootstrap's LLVM step for a target that is
  not the host panics without `clang-tblgen` in the host's own LLVM build
  directory, and every compiler here links the store's LLVM (`src/llvm.rs`),
  which bootstrap did not build.
- **`rustc_llvm`.** `compiler/rustc_llvm/build.rs` links `stdc++` for a target
  it does not list, links no C library, and finds a cross target's LLVM by
  replacing the host triple in the host `llvm-config`'s paths, which the
  store's paths do not contain.

The fixes to `bit.h`, `is_local_impl`, bootstrap's CMake system and
`rustc_llvm`'s C++ runtime and C library are ToyOS arms at existing dispatch
sites, written as upstream would take them: ToyOS joins `bit.h`'s `<endian.h>`
list, so libc carries POSIX's `endian.h` and no BSD name
(`issues/build/toyos-libc-lacks-the-posix-surface-llvm-compiles-against.md`).
`clang-tblgen` and the cross target's paths are not arms: each is fixed in
ToyOS's build, or written as upstream would take it and carried as
`issues/build/lld-beside-an-external-llvm-config-is-carried-without-an-upstream-pr.md`
carries LLD's.

**Exit**: bootstrap builds `rustc_llvm` for `x86_64-unknown-toyos`, and the
guest's `rustc -vV` prints `LLVM version:`.
