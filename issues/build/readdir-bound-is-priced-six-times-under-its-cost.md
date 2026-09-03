---
status: open
kind: tooling
opened: 2026-08-31
---

# `readdir_bound`'s committed price is a number no command now backs

`tests/test-durations:281` reads `readdir_bound 8578`, and `src/tiers.rs:312-313`'s
matching `Relegated { test: "readdir_bound", ci_ms: 8_578, .. }` row is priced
off it. The #343 review measured the test at roughly 50 s locally after the
bound fix (`Mounted::list` moving to `btree::collect_up_to`, landed at
`09250c97`) — nowhere near the committed 8,578 ms.

`src/tiers.rs:183-193`'s own doc on the `ci_ms` field says it is
"documentation, not a fixture" — `ci_profile_verdicts` checks a fresh
profile's tier *placement*, never this field against it — and "a nightly
run's measured profile is what refreshes these numbers." `--merge-durations
<dir>` (`src/durations.rs:187`) is that refresh: it folds a whole CI run's
shard artifacts into `tests/test-durations`. Nothing re-priced this row when
the bound fix landed, so it still carries whatever `readdir_bound` cost before
`/home`'s listing walk gained a bound — a different test in substance, wearing
the old test's price, until the next nightly's measured profile writes over
it.

Exit: the next nightly's artifact re-prices `readdir_bound` and the row takes
the measured value.

Provenance: adversarial review of PR #343.
