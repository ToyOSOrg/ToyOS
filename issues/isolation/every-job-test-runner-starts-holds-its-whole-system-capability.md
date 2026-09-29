---
status: open
kind: defect
opened: 2026-09-29
---

# Every job test-runner starts holds test-runner's whole system capability

`run_one` endows each job it spawns a `SysCap::duplicate` of test-runner's own
capability (`userland/test-runner/src/main.rs:292-295`), and a duplicate
carries every right the original does (`toyos/src/syscap.rs:63-70`). The job
also inherits test-runner's whole namespace
(`userland/test-runner/src/main.rs:86-92`). On `tests/testcases` the row grants
`device`, `dup`, `logread`, `power` and `roster`
(`tests/testcases/system.toml:41`), so every test binary may mint a device
claim, read every kernel record, reset or power off the machine, list every
process, and hand all of it to a child of its own. A right or a name added to
that row reaches every job the same way.

The binaries have no `[programs]` rows, so nothing declares what any of them
needs: `test_rs_audio_idle_suspend` reads `roster` off the duplicate
(`tests/toyos-rust-tests/src/bin/audio_idle_suspend.rs:7-11`), and
`endowment_denied` narrows `power` away from it.

**Exit**: a job holds only the rights and names its test is declared to need,
narrowed by test-runner (`SysCap::narrowed`, `toyos/src/syscap.rs:121-128`), and
a job that asks for anything else is refused.
