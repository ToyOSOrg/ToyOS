---
status: open
kind: tooling
opened: 2026-10-11
---

# A test in a guest-built crate no step reads runs nowhere

`build/hostws.rs`'s `guest_built` takes a member out of the host members, so
`--ci host`'s workspace `cargo test` and clippy's workspace shapes skip it.
Each such member is then tested by a step of its own, or not at all:

- `kernel` (its library) and `sdk/toyos` each have a step in `build/ci.rs`;
- every program directly under `system/`, `apps/` and `tests/guest/`
  (`build::PROGRAM_ROOTS`) is read by `build/userlandhost.rs`'s survey, which
  runs a program's tests once it holds one;
- `loader`, `sdk/window`, `sdk/font`, `sdk/filepicker`, `lib/logkeeper-api`
  and `lib/pci-claim` have neither.

A `#[test]` added to one of those six runs in no `--ci host` step, and
nothing reds. None of them holds a test today. The three `sdk/` crates were
surveyed as `userland/` crates until they moved under `sdk/`
(`issues/the-tree-says-who-uses-each-thing.md`, stage 5), and the two `lib/`
crates until they moved out of `userland/` at stage 7: they link `toyos` too.

They are not host members because `toyos`, which all three depend on, is
not clippy-clean on a host: `cargo clippy -p toyos-font -p toyos-window -p
filepicker-api --all-targets` with the adopted lints and `-D warnings`
exited 101 with 14 errors in `toyos`'s own source, measured just before the
move.

**Owner:** the orchestrator.
**Exit:** a `#[test]` added to any `guest_built` member either runs in a
`--ci host` step or reds it, and a fixture shows both for one of the six.
