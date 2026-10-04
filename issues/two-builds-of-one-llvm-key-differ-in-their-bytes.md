---
status: open
kind: defect
opened: 2026-09-30
---

# Two builds of one LLVM key differ in their bytes

`src/llvm.rs` holds that an LLVM is a function of its key. Two builds of one
key by `build_in_fork`, in two fork checkouts, differed in three things the key
does not name:

- **The LLVM checkout's `origin`.** LLVM's CMake writes it into
  `VCSRevision.h` as `LLVM_REPOSITORY` (`get_source_info` in
  `llvm/cmake/modules/VersionFromVCS.cmake`), and clang and LLD put it in their
  version (`clang/lib/Basic/Version.cpp`, `lld/Common/Version.cpp`), so in the
  guest's bytes: LLD writes it into the `.comment` of the `libstd` a sysroot
  links, and clang into that of each C and C++ object of its `c/lib/libc++.a`.
  Bootstrap's `update_submodule` sets a checkout's origin from the fork's
  `.gitmodules`, `ToyOSOrg`, when it moves the checkout to the gitlink's commit,
  and leaves it when the checkout holds that commit already. A worktree's
  checkout is cloned from the URL the fork's shared config names, `ToyOSOrg`.
- **The build directory.** `lld`'s `LC_RPATH` and `llvm-config`'s object and
  source roots name the `build/toyos-llvm` of the checkout that built it.
- **Archive dates.** The members of every static archive carry the time they
  were built (`ar tv`).

This is not `issues/two-checkouts-of-one-tree-build-different-guest-bytes.md`,
which holds the LLVM fixed and varies rustc's inputs: every checkout on a host
links the one LLVM its key names, so that gate sees no origin, and a comparison
of two LLVM installs sees no panic path rustc writes.

**Exit**: `config_text` sets `LLVM_FORCE_VC_REPOSITORY` and
`LLVM_FORCE_VC_REVISION`, since the repository alone drops the revision, and a
host test that runs `llvm/cmake/modules/GenerateVersionFromVCS.cmake` with
`config_text`'s defines on two checkouts of one commit whose `origin`s differ
writes one header from both, where today each names its own origin. What only
a configure or a build writes, the build directory and the archive dates, a
nightly check sees: it builds one key in two fork checkouts at different paths
whose origins differ, checks after each build that they still do, and finds
every file of the two installs byte-identical.
