---
status: open
kind: defect
opened: 2026-09-06
---

# `process_stats` exits 101 beside other guests

Red once in a full `cargo test` on this dev host while the scanout fold was
being gated: `FAIL process_stats: exit code Some(101)` after 4 s, with no
assertion text carried out of the guest, and the harness's re-run pass reported
`ALONE process_stats: GREEN — it fails only beside other guests, so its
Sched::Parallel is wrong.` It is the third name to do this in three foldings of
this branch — `log_reserve_window_negative` and `blocked_dump` were the other
two, each a different name on a different run — and nothing in the diff being
gated goes near any of them.
`issues/diagnostics/blocked-time-is-invisible-while-the-park-lasts.md` names
`process_stats` as where a defect was found, not as a name that flakes, so this
is its own entry. Nothing here investigates the mechanism: `ALONE: GREEN` is the
harness naming a hypothesis, and one red is not a rate.

Two more, each with its assertion. `640r3-loaderlines-r3-whole.log` (`wt/toyos-loaderlines`
`6e0d7da82`, "fastest boot 480 ms … ceilings paid at 1.00x") at `process_stats.rs:280`: "a
child that parked writing a full connection charged 0 ns to ipc and 0 ns to pipe".
`648-648-whole.log` (`wt/toyos-proclife1` `60ec86df3`, load average 84) at
`process_stats.rs:263`: "a child that parked reading a connection charged 0 ns to ipc and 0 ns
to pipe". Neither branch touches the test, `WaitClass` or the charge. The premise both arms read
is `roster::await_true` seeing the child's main thread `BLOCKED`, and nothing ties that park to
the connection: a park on anything else before the child reaches its `read` or `write`
satisfies it, the parent releases, and the connection's wait never parks. The assertion prints
two of the five classes, so which park was charged is not on record. Owner: the orchestrator.

Exit: each arm waits for a park it can name as the connection's, so a park on anything else
fails the arm by name, and `process_stats` passes on the T14's shared boot.
