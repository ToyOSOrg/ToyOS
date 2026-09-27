---
status: open
kind: tooling
opened: 2026-09-27
---

# The release tag hashes none of the build system that builds the toolchain

`src/toolchain.rs`'s `write_config`, `src/sysroot.rs` and `src/libc.rs` decide
the tarball's bytes and are not in `release::trees()`, so a change to them keeps
the old tag and CI installs a toolchain its tree does not describe.

**Exit**: a commit to each moves the tag, shown by `src/release.rs`'s tests.
