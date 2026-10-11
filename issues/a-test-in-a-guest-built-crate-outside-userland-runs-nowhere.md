---
status: open
kind: tooling
opened: 2026-10-11
---

# A test in a guest-built crate outside `userland/` runs nowhere

`src/hostws.rs`'s `guest_built` takes a member out of the host members, so
`--ci host`'s workspace `cargo test` and clippy's workspace shapes skip it.
Each such member is then tested by a step of its own, or not at all:

- `kernel` (its library) and `sdk/toyos` each have a step in `src/ci.rs`;
- every crate under `userland/` is read by `src/userlandhost.rs`'s survey,
  which runs a crate's tests once it holds one;
- `bootloader`, `sdk/window`, `sdk/font` and `sdk/filepicker` have neither.

A `#[test]` added to one of those four runs in no `--ci host` step, and
nothing reds. None of them holds a test today. The three `sdk/` crates were
surveyed as `userland/` crates until they moved under `sdk/`
(`issues/the-tree-says-who-uses-each-thing.md`, stage 5), and stage 7 moves
the survey's root along with `userland/`.

They are not host members because `toyos`, which all three depend on, is
not clippy-clean on a host: `cargo clippy -p toyos-font -p toyos-window -p
filepicker-api --all-targets` with the adopted lints and `-D warnings`
exited 101 with 14 errors in `toyos`'s own source, measured just before the
move.

**Owner:** the orchestrator, with stage 7 of the track above.
**Exit:** a `#[test]` added to any `guest_built` member either runs in a
`--ci host` step or reds it, and a fixture shows both for one of the four.
