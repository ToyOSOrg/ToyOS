---
status: open
kind: defect
opened: 2026-09-30
---

# Bootstrap cannot build LLVM, clang and lld for a ToyOS host

M2's clang and lld (`issues/build/toyos-builds-itself.md`) are LLVM built for
`x86_64-unknown-toyos`. Read from the fork at `rust/` and from
`src/llvm-project` at `849da7d62`, not run, four things stop bootstrap's LLVM
step for that target:

- **`clang-tblgen`.** With `clang = true`, the step for a target that is not
  the host panics unless `clang-tblgen` is in the CMake build directory of a
  host LLVM bootstrap built itself
  (`src/bootstrap/src/core/build_steps/llvm.rs`, `CLANG_TABLEGEN`). Every
  compiler build here names the store's LLVM (`src/llvm.rs`) as the host's
  `llvm-config`, so bootstrap builds no host LLVM and the file is never there.
- **Its CMake system.** `configure_cmake` names no system for a ToyOS target and
  falls back to `Generic`, under which LLVM sets `LLVM_ON_UNIX` to 0
  (`llvm/cmake/modules/HandleLLVMOptions.cmake`) and compiles no `Unix/`
  implementation of `Support`.
- **`bit.h`.** `llvm/include/llvm/ADT/bit.h` includes `<endian.h>` on the
  systems it lists and `<machine/endian.h>` on any other, ToyOS among them.
- **`is_local_impl`.** `llvm/lib/Support/Unix/Path.inc` reads the BSDs'
  `MNT_LOCAL` on a system it does not list.

The last three are ToyOS arms at existing dispatch sites, written as upstream
would take them. ToyOS joins `bit.h`'s `<endian.h>` list, so libc carries
POSIX's `endian.h` (`userland/libc/include/endian.h`) and no BSD name. With
those two arms on `849da7d62` and CMake told the system by hand, the 71
libraries rustc links compile against the C sysroot but for five objects of
`LLVMSupport`, which stop on `alarm` and on stage 3's `wait`, `wait4` and
signal-set calls; without them, every object that includes `bit.h` stops.
Bootstrap's arm names the system `ToyOS`, which LLVM's configure refuses,
`Unable to determine platform`, until CMake knows ToyOS and sets `UNIX`
(`issues/build/the-cxx-runtime-names-toyos-to-cmake-as-unix.md`).

`clang-tblgen` is no arm, and neither bootstrap nor LLVM changes for it: ToyOS's
build builds the LLVM for a ToyOS host in a bootstrap build that built the
host's LLVM itself, as `src/llvm.rs` builds the store's, never in a compiler
build that names the store's.

**Exit**: bootstrap, with `clang = true`, installs a clang and an `ld.lld` for
`x86_64-unknown-toyos`.
