---
status: open
kind: tooling
opened: 2026-09-26
---

# Two suites in one worktree race on the C corpus's libc archive

`tests/common/compile.rs`'s `libc_archive_toyos` runs `cargo rustc
--crate-type staticlib` into `userland/libc/target/sysroot-<key>/` once per
process, then every C test links `-ltoyos_libc` from there. Nothing holds the
archive across the build and its use, so a second `cargo test` in the same
worktree, running its own `cargo rustc` into the same directory, removes and
re-creates the archive while the first suite's C links read it. `src/build.rs`'s
`build_programs` holds `buildlock::artifact` across build and read for exactly
this reason; this path does not.

Seen on PR #524's A/B, three `cargo test --test toyos-build --
blocking_read_window` at once per worktree, on both main (`d65446cc`) and the
branch: four of thirty runs panicked before any guest booted, each naming a C
case that "stopped building" (`resolve_libs failed: cannot parse -ltoyos_libc:
library not found`, or `expected staticlib at …/libtoyos_libc.a` from the
`NOT_RUN` check). The harness reports it as a C test that no longer builds.

**Exit condition**: the archive is built and read under one hold, as
`build_programs` does, and two suites started at once in one worktree both get
past the C corpus's build.
