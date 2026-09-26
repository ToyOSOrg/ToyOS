---
status: open
kind: defect
opened: 2026-09-26
---

# `fpu_isolation`'s probe thread is joined as not found, on main

`fpu_isolation` (nightly tier) is red on `main` at `17eb66a4`, and on every
branch built on it. Each `check` child spawns `thread_probe`, which records
its entry state and calls `SYS_THREAD_EXIT`. `thread_join` on that tid then
answers `18446744073709551614` (−2), where 0 is expected
(`tests/toyos-rust-tests/src/bin/fpu_isolation.rs:346`). The kernel logs the
thread's `exit … code=0`, so the thread ran, and `sys_thread_join` answers
`NotFound` from `wait_thread_zombie`'s `Err(())`: no zombie for a thread that
existed. The leak arm therefore reports `a process started with the previous
one's FP registers` in all three rounds; that is the arm's wording, and not
what failed.

Measured by `cargo test --test toyos-build -- fpu_isolation --nightly`:
- at `17eb66a4`: EXIT=1, the harness's alone re-run red the same way;
- on PR #524's branch at `235c5a5b`, and on its nightly (run 36266825579,
  `guest (12)`): the same.

It passed on main's nightly at `e8d7c9c0` (run 36228604597, `guest (11)`).
Thirteen landings fall between the two, and none is bisected. One of them,
#513 ("One way to wait", `46056bfa`), rewrote `sys_thread_join`'s wait.

**Exit**: `fpu_isolation` green, and a test that joins a thread which has
already exited by the time of the join, red on the tree this issue was
filed against.
