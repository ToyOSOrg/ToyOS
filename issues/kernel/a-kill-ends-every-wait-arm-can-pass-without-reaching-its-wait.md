---
status: open
kind: tooling
opened: 2026-09-28
---

# A `kill_ends_every_wait` arm can pass without reaching its wait

`tests/toyos-rust-tests/src/bin/kill_ends_every_wait.rs` kills each child once
it reads the child's `parked in <wait>` marker, which the child prints just
before the syscall that parks. A kill that lands between the marker and the
park ends the child at that syscall's exit boundary instead, and the arm passes
without the wait it names ever being killed. The per-arm mutations that make one
wait uncancellable each went red when measured, so each arm reached its wait in
those runs; nothing makes it reach it in every run.

Owner: orchestrator. Exit condition: the parent sees the child parked in the
named wait, by the kernel's word, before it kills it.
