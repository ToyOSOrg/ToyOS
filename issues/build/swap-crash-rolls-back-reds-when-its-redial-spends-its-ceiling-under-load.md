---
status: open
kind: defect
opened: 2026-09-26
---

# swap_crash_rolls_back reds when its redial spends its ceiling under host load

`swap_crash_rolls_back` is red with the same two findings on origin/main's netd
and on a branch's: the host stream's redial after the swap was turned away
`metalswap::TURNED_AWAY_CEILING` (64) times and gave up, so init's `restored`
never reached the host, although the guest's own console shows the rollback
completing (`init: swap netd: restored`, then `logd: serving this boot's log on
port 41337`). It is the compromise
`issues/diagnostics/a-swaps-redial-asks-again-with-no-event-to-wait-on.md`
records, reached: the redial asks again at once, and on a loaded host the
ceiling runs out before `logd` listens again.

Seen with ten spinning host threads beside the run, on the review branch of
the netd receive-pipe fix: once in a full `-- swap` run of five, red again
alone in that same run; and once in three `-- swap_crash_rolls_back` runs
with netd reverted to origin/main (4b235d27), where the harness's alone re-run
was green and called the `Sched::Parallel` classification wrong. The other
five of those six runs, three on the branch's netd and two on main's, were
green. `cargo run -- --known-red
swap_crash_rolls_back` answers that it is not quarantined.

Exit condition: the redial waits on a guest-side event (the linked issue's
exit condition), or the test is shown green over a stated number of loaded
runs.
