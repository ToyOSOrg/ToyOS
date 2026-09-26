---
status: open
kind: tooling
opened: 2026-09-26
---

# The release path stages in `$TMPDIR` outside the scratch guard

`src/release.rs` takes `std::env::temp_dir()` twice: the toolchain install
downloads its tarball to `$TMPDIR/<asset>`, and the publish writes
`TOOLCHAIN`, `notes.md` and the built tarball straight into `$TMPDIR`. Nothing
removes them, on success or on failure, and two runs on one host share the
same fixed names. Every other host scratch is a `toyos_tmpdir::TempDir`,
which is gone with its holder; `src/sourcegate.rs`'s
`every_host_scratch_is_the_guard` counts these two as named exceptions.

**Exit**: both paths stage in a `toyos_tmpdir::TempDir`, and `src/release.rs`
leaves `TEMP_DIR_ALLOWED`.
