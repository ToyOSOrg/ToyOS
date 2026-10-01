---
status: open
kind: defect
opened: 2026-09-30
---

# Bootstrap cannot build LLVM, clang and lld for a ToyOS host

M2's clang and lld (`issues/build/toyos-builds-itself.md`) are bootstrap's
`Llvm` step, with `clang = true`, and its `Lld` step for
`x86_64-unknown-toyos`, in a bootstrap build that names no `llvm-config` for
the build triple and so builds that triple's LLVM, and its `clang-tblgen`,
itself. CMake takes the C sysroot's `toolchain.cmake` from
`CMAKE_TOOLCHAIN_FILE_x86_64_unknown_toyos`. What stops the build, in the
order it stops it:

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
  `libclang-cpp`, do not link against the C sysroot
  (`issues/build/a-shared-object-does-not-link-against-the-c-sysroot.md`).
  clang and lld use none of them, and ToyOS's build turns them off.

**Exit**: ToyOS's build, with nothing supplied by hand, has bootstrap install
a clang and an `ld.lld` for `x86_64-unknown-toyos`.
