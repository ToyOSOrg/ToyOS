---
status: open
kind: defect
opened: 2026-09-30
---

# Two builds of one LLVM key differ in their bytes

`src/llvm.rs` holds that an LLVM is a function of its key. Two builds of one
key by `build_in_fork`, in two fork checkouts, differ in two things the key
does not name:

- **The build directory.** `lld`'s `LC_RPATH` and `llvm-config`'s object and
  source roots name the `build/toyos-llvm` of the checkout that built it.
- **Archive dates.** The members of every static archive carry the time they
  were built (`ar tv`).

This is not `issues/two-checkouts-of-one-tree-build-different-guest-bytes.md`,
which holds the LLVM fixed and varies rustc's inputs: every checkout on a host
links the one LLVM its key names, so that gate sees no origin, and a comparison
of two LLVM installs sees no panic path rustc writes.

The checkout's `origin` was the third, until `config_text` named the revision
and the repository (`llvm::stamp`): of that, only the configuration's text is
tested, and no test builds two checkouts whose `origin`s differ.

**Exit**: a nightly check builds one key in two fork checkouts at different
paths whose origins differ, checks after each build that they still do, and
finds every file of the two installs byte-identical.
