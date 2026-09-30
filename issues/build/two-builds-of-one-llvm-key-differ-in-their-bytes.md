---
status: open
kind: tooling
opened: 2026-09-30
---

# Two builds of one LLVM key differ in their bytes

`src/llvm.rs` holds that an LLVM is a function of its key. Key
`64453b64c91c17c2` built twice by `build_in_fork` — the stored one, made in the
linked worktree `toyos-mtime`, and one made under n2 in place of Ninja from a
fork checkout whose `src/llvm-project` shares the primary's repository — gave
the same 3841 paths, 181 of them different, each for a reason the key does not
name and none of them the build tool:

- **The LLVM checkout's `origin`.** LLVM's CMake writes it into
  `VCSRevision.h` as `LLVM_REPOSITORY`, and every `--version` prints it: the
  stored `lld` names `ToyOSOrg/llvm-project`, the other
  `rust-lang/llvm-project`. The primary's `rust/src/llvm-project` has origin
  `rust-lang`; the fork's `.gitmodules` names `ToyOSOrg`. 19 files: the header,
  `libclangBasic.a` (its `Version.cpp.o`), `libclang.dylib`,
  `libclang-cpp.dylib`, and fifteen executables, `clang-22` and `lld` among
  them.
- **The build directory.** `lld`'s `LC_RPATH` and `llvm-config`'s object and
  source roots name the `build/toyos-llvm` of the worktree that built it, which
  `place` removes: the stored ones name `toyos-mtime`, which no longer exists.
- **Archive dates.** 161 of the 162 static archives differ only in their
  members' dates (`ar tv`); every member is byte-identical.

Every other file, `llvm-ar`, `llc`, `opt` and `llvm-tblgen` among them, is
byte-identical.

**Exit**: two builds of one key, made in two worktrees, are byte-identical.
