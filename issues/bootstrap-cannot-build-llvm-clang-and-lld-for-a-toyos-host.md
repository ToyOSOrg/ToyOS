---
status: open
kind: defect
opened: 2026-09-30
---

# Bootstrap cannot build LLVM, clang and lld for a ToyOS host

M2's clang and lld (`issues/toyos-builds-itself.md`) are bootstrap's
`Llvm` step, with `clang = true`, and its `Lld` step for
`x86_64-unknown-toyos`, in a bootstrap build that names no `llvm-config` for
the build triple and so builds that triple's LLVM, and its `clang-tblgen`,
itself. CMake takes its toolchain file from
`CMAKE_TOOLCHAIN_FILE_x86_64_unknown_toyos`: one that includes the C sysroot's
`toolchain.cmake` and adds the ToyOS LLVM's install to `CMAKE_FIND_ROOT_PATH`,
because the `Lld` step names that LLVM to `find_package` by a hint, and the
sysroot's file has CMake find a package under a root alone. What stops the
build, in the order it stops it:

- **Compile.** The first errors are `Support`'s: `Unix/Watchdog.inc` and
  `Unix/Program.inc` call `alarm` (`issues/libc-has-no-alarm.md`), and
  `Program.inc` stage 3's `wait` and `wait4`
  (`issues/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`).
  Then ORC's `shm_open` and `shm_unlink`, the interpreter's `scanf` and
  `llvm-objdump`'s `ctime`
  (`issues/libc-lacks-names-llvm-for-a-toyos-host-calls.md`), and
  clang's `std::ifstream`, which libc++ has only with `std::filesystem`
  (`issues/libcxx-is-built-without-std-filesystem.md`).
- **Link.** clang needs `lround`, which libc does not define
  (`issues/libc-lacks-names-llvm-for-a-toyos-host-calls.md`), beside
  those above. LLVM's shared libraries, `libLTO`, `libRemarks`, `libclang` and
  `libclang-cpp`, do not link against the C sysroot
  (`issues/a-shared-object-does-not-link-against-the-c-sysroot-with-z-defs.md`).
  clang and lld use none of them, and ToyOS's build turns them off.

**Exit**: ToyOS's build, with nothing supplied by hand, has bootstrap install
a clang and an `ld.lld` for `x86_64-unknown-toyos`.
