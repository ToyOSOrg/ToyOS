---
status: open
kind: defect
opened: 2026-09-30
---

# Bootstrap cannot build LLVM, clang and lld for a ToyOS host

M2's clang and lld (`issues/build/toyos-builds-itself.md`) are bootstrap's
`Llvm` step, with `clang = true`, and its `Lld` step for
`x86_64-unknown-toyos`, with the store's LLVM (`src/llvm.rs`) as the host's
`llvm-config`. Bootstrap and LLVM carry what is theirs: CMake is told the
system `ToyOS`, `clang-tblgen` is the host LLVM's, LLD is built for a target
whose LLVM bootstrap builds, and LLVM's `Support` reads POSIX's `<endian.h>`
and calls every ToyOS file local. What stops the build is not theirs, in the
order it stops it:

- **CMake knows no ToyOS**, so `UNIX` is unset and LLVM's configure refuses,
  `Unable to determine platform`
  (`issues/build/the-cxx-runtime-names-toyos-to-cmake-as-unix.md`).
- **Compile.** The first error is `sigemptyset`, undeclared in `Support`'s
  `CrashRecoveryContext.cpp`: stage 3's signal-set calls, `wait` and `wait4`
  (`issues/kernel/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`)
  and `alarm` (`issues/build/libc-has-no-alarm.md`). Then ORC's `shm_open` and
  `shm_unlink`, the interpreter's `scanf` and `llvm-objdump`'s `ctime`
  (`issues/build/libc-lacks-names-llvm-for-a-toyos-host-calls.md`), and
  clang's `std::ifstream`, which libc++ has only with `std::filesystem`. Built
  with it, libc++ asks libc for `setbuf`, `fseeko`, `ftello`, `utimes`,
  `truncate`, `pathconf`, `openat`, `unlinkat`, `fdopendir`, `_PC_PATH_MAX`,
  `O_DIRECTORY`, `O_NOFOLLOW`, `AT_FDCWD` and `AT_REMOVEDIR`
  (`issues/build/libcxx-is-built-without-std-filesystem.md`).
- **Link.** clang needs `lround`, which libc does not define
  (`issues/build/libc-lacks-names-llvm-for-a-toyos-host-calls.md`), beside
  those above. LLVM's shared libraries, `libLTO`, `libRemarks`, `libclang` and
  `libclang-cpp`, do not link against the C sysroot, whose archives are built
  for an executable
  (`issues/build/a-rust-std-binary-cannot-link-the-cxx-runtime.md`); clang and
  lld use none of them.

With each name declared and defined to abort, libc++ built with
`std::filesystem` against them, the four shared libraries' build options off,
and a `Platform/ToyOS.cmake` on CMake's module path, bootstrap installs clang
and `ld.lld` for `x86_64-unknown-toyos`.

**Exit**: bootstrap, with `clang = true`, installs a clang and an `ld.lld` for
`x86_64-unknown-toyos`.
