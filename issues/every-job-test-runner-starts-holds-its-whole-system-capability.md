---
status: open
kind: defect
opened: 2026-09-29
---

# Every job test-runner starts holds test-runner's whole system capability

`run_one` endows each job it spawns a `SysCap::duplicate` of test-runner's own
capability (`tests/guest/test-runner/src/main.rs`), and a duplicate carries
every right the original does (`sdk/toyos/src/syscap.rs`). The job also
inherits test-runner's whole namespace. On `tests/images/testcases.toml` the
row grants `device`, `dup`, `logread`, `power`, `roster`, `counters`, `trace`
and `inventory`, so every test binary may mint a device claim, read every
kernel record, reset or power off the machine, list every process, and hand
all of it to a child of its own. A right or a name added to that row reaches
every job the same way.

The binaries have no `[programs]` rows, so nothing declares what any of them
needs; `endowment_denied` narrows `power` away from the duplicate.

**Exit**: a job holds only the rights and names its test is declared to need,
narrowed by test-runner (`SysCap::narrowed`, `sdk/toyos/src/syscap.rs`), and
a job that asks for anything else is refused.
