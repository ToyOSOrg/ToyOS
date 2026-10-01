---
status: open
kind: tooling
opened: 2026-09-30
---

# A Finder file in a build directory fails the build

The macOS Finder writes a `.DS_Store` into a directory the build takes as
wholly its own, and the step that reads or removes it refuses:

- `keystore::sweep_by` takes every entry of a store directory as `<key>` or
  `<key>.<suffix>`, so the Finder's file names the key `""`, and
  `buildlock::keyed_idle` then opens the lock directory itself: `build lock:
  open <primary>/.git/toyos-build-locks/llvm/: Is a directory (os error 21)`.
  It happened after an LLVM placement, whose product was whole, so the next
  build went on; the sweep had removed nothing.
- `libcxx::build` ends with `fs::remove_dir_all(scratch)`, and the Finder
  wrote into that scratch while the removal ran: `remove
  <primary>/rust/build/sysroots/acd58e51940b13be.libcxx-x86_64: Directory not
  empty (os error 66)` at `src/libcxx.rs:95`, after the runtime had installed.
  The `cargo run -- --build-only` that hit it exited 101.

**Exit**: a host writer's file in a build directory fails neither a sweep nor
a scratch removal whose product is whole, each with a test.
