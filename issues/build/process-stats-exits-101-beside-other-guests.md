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

Exit: a rate — the same suite run repeatedly with and without a second
worktree's build on the host — that says whether this is contention the harness
should schedule around or a defect the guest has, and the name is either
re-tiered or fixed at the cause.
