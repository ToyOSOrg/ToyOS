---
status: open
kind: defect
opened: 2026-09-06
---

# `log_reserve_window_negative` times out beside other guests

Red once in a full `cargo test` on this dev host while the metal fold was being
gated: `FAIL log_reserve_window_negative: [qemu] Boot timed out waiting for
===READY===; the console carried:` and then a boot that had reached the kernel's
first records (`actuators:
root=fe84bf013fe88c24e627180dc5c85ef0,log-nested-reserve,log-unbracketed-reserve`,
`PAT:`, `boot: memory map …`) and no further. The name took 121 s against its
committed 6,791 ms, and the harness's re-run pass reported `ALONE
log_reserve_window_negative: GREEN — it fails only beside other guests, so its
Sched::Parallel is wrong.` Two later runs of the same suite on the same tip were
green (322 passed, then 323 passed), and a third run of that session went red on
`blocked_dump` instead, which
`issues/build/parallel-tests-red-under-other-suites.md` already carries. Nothing
here investigates the mechanism: `ALONE: GREEN` is the harness naming a
hypothesis, and one red at 18x its price is not a rate.

Exit: a rate — the same suite run repeatedly with and without a second
worktree's build on the host — that says whether this is contention the harness
should schedule around or a defect in the guest's own boot, and the name is
either re-tiered or fixed at the cause.
