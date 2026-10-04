---
status: open
kind: tooling
opened: 2026-09-28
---

# The `/tmp` leftover check can name another worktree's killed run

`src/ci.rs`'s `left_behind` names every root `toyos_tmpdir::gone_roots(short)`
finds that `before` does not: a `toyos_tmpdir::TempDir::short` whose owning
process died during the steps that ran between the two calls. `gone_roots`
reads `SHORT_BASE` (`/tmp`), and every worktree on the host shares that base
and its `GLOBAL` lock — the reclaim is deliberately cross-worktree
(`toyos-tmpdir/src/lib.rs`'s module header: "Every process that shares a
base — every worktree on the host — shares the lock file"). Nothing in
`gone_roots` or in `left_behind`'s call sites records which pids belong to
*this* job's own steps, so a root left by another worktree's harness,
SIGKILLed on the same host in the same window, is named as this job's own
leftover.

**Evidence:** `toyos-tmpdir`'s own tests demonstrate the mechanism the check
relies on — a SIGKILLed holder's root is reclaimed by the next process to make
a directory in the same base, whichever process that is
(`toyos-tmpdir/tests/reclaim.rs`'s `a_killed_process_is_reclaimed_and_a_live_one_is_never_touched`).
`left_behind` has no notion of a job or a worktree; it is a diff of
`gone_roots(short)` against a snapshot taken before the steps ran, over a base
every process on the host writes into. On the hosted runners the gate uses,
each job has its own host, so the check is exact there; a developer running
`cargo run -- --ci host` on a machine also running another worktree's harness
can see a red that is not this job's.

**Exit condition:** `left_behind` names only roots this job's own processes
created, or the check is scoped to a base this job does not share with another
worktree's harness; a test demonstrates the narrowed check passing a killed
run made outside the job's own process tree.
