---
status: open
kind: tooling
opened: 2026-10-04
---

# The host suite assumes a user that is not root

`cargo run -- --ci host` on `main` at `4d46c8e5`, run as root (uid 0) on
Ubuntu 24.04 in a cloud trial, reported `2 of 75 step(s) red`, with five tests
failing. Each provokes its failure by taking a permission away, and root's
access is not checked against permission bits, so the denial the test waits for
never comes. That root is the cause is read from the tests: the run was not
repeated as a non-root user. Nothing in the tree says the host suite needs a
user that is not root.

| test | the denial it relies on |
|---|---|
| `build::tests::a_stamp_that_cannot_be_written_panics` | `src/build.rs:2449` makes `kernel/target` `0o555` and expects writing `.deps-stamp` into it to panic |
| `llvm::tests::a_placed_llvm_is_never_written`, red with `bin/lld could be written` | `src/llvm.rs:676` expects `PermissionDenied` from opening, removing and adding files in a tree `read_only` (`src/llvm.rs:381`) only took the write bits from |
| `sysroot::tests::a_switch_that_cannot_remove_records_nothing_and_the_next_one_removes` | `src/sysroot.rs:1465` makes the build directory `0o555` and expects removing `bootstrap` from it to panic |
| `toolchain::tests::dep_info_that_cannot_be_read_is_refused` | `src/toolchain.rs:1546` makes `dist` `0o000` and expects reading it to panic |
| `a_directory_the_sweep_cannot_remove_is_reported_and_the_next_process_still_works`, red at `toyos-tmpdir/tests/reclaim.rs:308` | `toyos-tmpdir/tests/reclaim.rs:298` makes `locked` `0o555`, so that the sweep cannot unlink the file in it and must move the root aside as `stuck-*`; as root it removes the root and no `stuck-*` exists |

These tests are unchanged between `4d46c8e5` and `0613f93f`. In four of them
only the test is wrong as root: the code under test is fine, but the fixture
does not produce the failure it needs to test. `a_placed_llvm_is_never_written`
is different, because its claim is false as root: a placed LLVM is protected
only by its mode bits (`src/llvm.rs:381`), so a root build process can write
into it.

**Owner**: the host job, `src/ci.rs`, and the build-system modules the five
tests sit in.

**Exit**: either `cargo run -- --ci host` is green when run as uid 0, or the
requirement for a non-root user is written where the host suites are described
and the `host` job refuses to start as uid 0 and names that requirement.
Running the host job as root on Linux shows which one landed.
