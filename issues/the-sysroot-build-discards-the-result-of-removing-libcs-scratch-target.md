---
status: open
kind: tooling
opened: 2026-10-09
---

# The sysroot build discards the result of removing libc's scratch target directory

`src/sysroot.rs`'s `build` ends libc's two builds with `let _ =
fs::remove_dir_all(&libc_target);`: a removal that fails leaves
`<key>.libc-target` beside the published sysroot in the store, and nothing
says so. The line above it, the miscompile check's scratch, fails through
`keystore::remove`; this one was left as it was because #790's fence named
that line alone. Owner: the build system.

**Exit**: the line is `keystore::remove(&libc_target);`, and a build that
makes a sysroot is green with it.
