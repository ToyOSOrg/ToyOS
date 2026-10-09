---
status: open
kind: defect
opened: 2026-09-30
---

# Bootstrap cannot build LLVM, clang and lld for a ToyOS host

The track holds one build of one fork (`issues/toyos-builds-itself.md`), and
the tree builds LLVM for a ToyOS host by two paths. M2's clang and `ld.lld`
come from a CMake configure and n2 build of their own (`src/hostedclang.rs`).
M3's rustc carries an LLVM that bootstrap's `Llvm` step builds for
`x86_64-unknown-toyos` (`issues/rustc-llvm-cannot-build-for-a-toyos-host.md`),
and bootstrap's `Llvm` step, with `clang = true`, and its `Lld` step make that
host's clang and lld too. One LLVM, one host, two code paths.

Bootstrap's build names no `llvm-config` for the build triple, and so builds
that triple's LLVM, and its `clang-tblgen`, itself. CMake takes its toolchain
file from `CMAKE_TOOLCHAIN_FILE_x86_64_unknown_toyos`: one that includes the C
sysroot's `toolchain.cmake` and adds the ToyOS LLVM's install to
`CMAKE_FIND_ROOT_PATH`, because the `Lld` step names that LLVM to
`find_package` by a hint, and the sysroot's file has CMake find a package
under a root alone. libc and libc++ now give what stopped a CMake build of
clang and lld from the same commit, measured in #809: `alarm`, `wait`, `wait4`,
`lround` and `std::filesystem`. Bootstrap's build, which builds all of LLVM,
has not run since; what is known to stop it besides:

- ORC's `shm_open` and `shm_unlink`, the interpreter's `scanf` and
  `llvm-objdump`'s `ctime`
  (`issues/libc-lacks-names-llvm-for-a-toyos-host-calls.md`).
- LLVM's shared libraries, `libLTO`, `libRemarks`, `libclang` and
  `libclang-cpp`, do not link against the C sysroot
  (`issues/a-shared-object-does-not-link-against-the-c-sysroot-with-z-defs.md`).
- Bootstrap configures the build machine as LLVM's host
  (`issues/a-toyos-hosted-llvm-is-configured-as-running-on-the-build-machine.md`).

Owner: `issues/toyos-builds-itself.md`, M3.

**Exit**: ToyOS's build, with nothing supplied by hand, has bootstrap install
a clang and an `ld.lld` for `x86_64-unknown-toyos`, `cargo run --
--hosted-clang` places those, and `src/hostedclang.rs`'s CMake configure and
build are gone.
