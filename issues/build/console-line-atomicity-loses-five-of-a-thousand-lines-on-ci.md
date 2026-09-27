---
status: expected-red
kind: defect
opened: 2026-08-20
---

# `console_line_atomicity` loses five of a thousand lines on CI, one guest per machine

First sighting on the CI instrument: PR #166 run `32364721784`, `guest (10)`,
`writer A declared 1000 whole lines and the capture carries 995`, `ALONE:
GREEN` in the same job. The diff it rode on is an issues-and-prose audit, so
the tree is not a suspect.

CI runs one guest per machine with `--jobs 1`, so whatever loses five of a
writer's thousand lines there is not host contention between suites — which
sharpens the question the test was built to ask rather than settling it: the
loss is inside one guest's own console path.

Split out of `issues/build/parallel-tests-red-under-other-suites.md`, whose
rate table was never this test's — CI's single-guest-per-machine shards rule
out the contention shape that file is about.

**Exit condition.** Re-enabled when a reproduction identifies which of the two
sides drops the five lines — the writer's own buffering, the console's queue,
or the harness's capture — with a byte-level trace across the drop. Owner: the
console path (`tests/toyos-rust-tests/src/bin/console_line_atomicity.rs` and
the kernel console driver it writes through); nobody is holding it yet.
