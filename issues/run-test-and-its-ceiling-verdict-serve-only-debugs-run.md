---
status: open
kind: tooling
opened: 2026-10-03
---

# `run_test` and its ceiling verdict serve only `--debug`'s `run`

`QemuInstance::run_test`, `run_test_hooked` and `run_test_paced`
(`tests/common/qemu.rs`), and `ceiling_verdict`, the verdict their read loop
ends a wait on, have one caller: interactive debug mode's `run <test>`
(`tests/toyos.rs`, `qemu.run_test(test_name, Duration::from_secs(60))`). Every
guest test of the suite waits through `await_guest`, `drain_until` or a
screendump wait instead. So the checks that hold `ceiling_verdict` to its arms
(`tests/checks/qemu.rs`, and `a_stall_stays_red` in `tests/checks.rs`) and the
`run_test` shape `driven_binaries` reads (`tests/checks.rs`) guard that one
path.

## Measured

`git grep -c 'run_test(\|run_test_hooked(\|run_test_paced(' <rev> -- tests/toyos.rs tests/common`
counts 40 lines in `tests/toyos.rs` and 34 across sixteen other `tests/common`
modules at `06788146b^`, and one in `tests/toyos.rs`, the debug loop's, at
`06788146b`, the commit that cut the guest suite; the five in
`tests/common/qemu.rs` are the three functions' own.

## Owner

`issues/the-guest-suite-runs-only-what-no-cheaper-tier-reaches.md`, whose
first cut left them that one caller.

## What would close it

`--debug`'s `run` reads its test through a wait the suite uses, and
`run_test`, `run_test_hooked`, `run_test_paced`, `ceiling_verdict` and the
checks and readers that exist for them are deleted.
