---
status: open
kind: tooling
opened: 2026-09-26
---

# A defect only contention exposes is classified as a wrong `Sched::Parallel`

When a test fails in the wide run and passes in the lone re-run, and the wide
run shared the host, `alone_line` (`tests/toyos.rs`) prints one verdict:
`GREEN — it fails only beside other guests, so its Sched::Parallel is wrong`.
That names a cause. What the two runs establish is only that the failure needs
the timing a loaded host produces, and a kernel race that only a slow or
preempted vCPU opens produces exactly that pair.

Recorded case, PR #524's fast tier at `f67863c2`: 270 passed, 130 failed, and
254 of the failures were one kernel panic, `vconsole: no tx slot`. Each of the
130 was green alone, so each got the scheduling verdict, 130 times. The cause
was one kernel defect: `serial::BackendGuard::try_lock` built, and so dropped
and unlocked, a guard for the CPU that lost the exchange (fixed in
`1e5ef5c1`). Changing any test's `Sched` would have hidden it.

What reads it wrong:

- The per-test verdict names a cause the evidence does not decide. The
  `tests/CLAUDE.md` caveat already says it is a hypothesis; the line itself
  says it is a finding.
- Nothing groups the failures. Tests that fail wide on one shared headline (the
  same panic message, `toyos_build::alone::same_failure`) are one finding, and
  the report prints them as N separate classifications.

**Exit condition**: the shared-host green arm states the observation (fails
only beside other guests) without naming `Sched` as the cause, and the run's
summary groups wide failures that share a headline, printing the group's count
before any per-test classification. The staged pair for the grouping is the
`f67863c2` shape: many tests, one panic headline.
