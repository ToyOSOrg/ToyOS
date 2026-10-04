---
status: open
kind: defect
opened: 2026-10-03
---

# n2 does not compile for ToyOS

n2 is the only Ninja the build runs (`src/n2.rs`), pinned at `b1fead52ccda`.
At that commit its `process::run_command` exists under `cfg(unix)`,
`cfg(windows)` and for `wasm32`, and its `terminal::use_fancy` and `get_cols`
under the same three; its `task.rs`, `run.rs` and `progress_fancy.rs` call
them under no `cfg`. `x86_64-unknown-toyos` is none of the three: its target
names no family (`compiler/rustc_target/src/spec/base/toyos.rs` in the `rust/`
fork). Read from the source, not built.

Its unix arm is `posix_spawn` of `/bin/sh -c <command>` with `/dev/null` as
stdin, a pipe and `waitpid`, so a ToyOS arm waits on what
`issues/ninja-runs-every-command-through-a-bin-sh-toyos-does-not-have.md`
and `issues/there-is-no-dev-null.md` wait on.

Owner: `issues/toyos-builds-itself.md`, whose M5 builds LLVM in the
guest.

Exit condition: n2 builds for `x86_64-unknown-toyos`, and inside ToyOS it
builds a file of two edges, one depending on the other.
