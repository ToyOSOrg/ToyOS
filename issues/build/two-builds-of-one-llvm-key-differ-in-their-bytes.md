---
status: open
kind: defect
opened: 2026-09-30
---

# Two builds of one LLVM key differ in their bytes

`src/llvm.rs` holds that an LLVM is a function of its key. Two builds of one
key by `build_in_fork`, one in a worktree and one under n2 in place of Ninja
from a fork checkout whose `src/llvm-project` shares the primary's repository,
differed in three things the key does not name:

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
  The primary's `rust/src/llvm-project` names `rust-lang`, and so does a git
  worktree of its repository.
- **The build directory.** `lld`'s `LC_RPATH` and `llvm-config`'s object and
  source roots name the `build/toyos-llvm` of the checkout that built it.
- **Archive dates.** The members of every static archive carry the time they
  were built (`ar tv`).

**Exit**: a nightly check builds one key in two fork checkouts at different
paths whose `src/llvm-project` origins name different repositories, and every
file of the two installs is byte-identical. Until it is, every pair of
checkouts builds one key to different bytes: the primary's and a worktree's,
two worktrees', and one made by hand and any other.
