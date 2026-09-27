---
status: open
kind: tooling
opened: 2026-09-27
---

# The release tag hashes none of the build system that builds the toolchain

`src/release.rs`'s tag is the hash of the rust fork, the sysroot's sources and
manifests, `src/clang.rs` and `src/release.rs`. What turns those into the
tarball's bytes is also `src/toolchain.rs` (`write_config`: the hosted rustc's
linker, archiver and codegen backend, `lld`), `src/sysroot.rs` (its `RECIPE`,
the std build and the C sysroot's layout) and `src/libc.rs` (the features and
the archive merge), and none of them is hashed. A change to any of them keeps
the old tag, so CI installs the tarball built before it and runs against a
toolchain its tree does not describe; `check_installed_toolchain` compares only
the sources, so nothing refuses it.

**Exit**: every declaration those three files make about the tarball is hashed
into the tag — moved to a file the tag hashes, or the tag hashing theirs — and a
test in `src/release.rs` commits a change to each and sees the tag move.
