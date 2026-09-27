---
status: open
kind: defect
opened: 2026-09-27
---

# `logd` flushes the records its own refused flush made, for as long as flushes are refused

`logd` makes the volume durable after every round it wrote a line in. A
flush whose first attempt is budget-refused and then retried commits four
kernel records: the storage driver's `not issued` and `ran out of its
operation budget`, the volume's `the device would not answer in the caller's
own budget`, and `fsync: <path> durable on attempt 2`. Those records are the
next round's lines, so that round flushes again. While every flush's first
attempt is refused, the log never goes quiet. It grows by those four records
per round, rotates, and the boot's own early files are rotated away.

Measured at dbf4ace5 with `fsync-budget-spent` refusing every flush's first
attempt, as it did before this branch. In the 2 s after
`home_budget_refusal_retried`'s guest finished, 220 and 131 `/log` flushes
were retried (two runs, `cargo test --test toyos-build -- --nightly
home_budget_refusal_retried`, TCG on the dev host). In CI (run 36285169430,
KVM) the same storm reached `_0003.log` by 28 s, and neither
`home_budget_refusal_retried` nor `log_flush_retry` saw `===READY===`.

The actuator now refuses once per file, so no test stages this any more. On
hardware the loop needs a device whose every flush overruns its operation
budget. Each round then costs at least that budget, so the loop is slow, but
it never ends while the device stays that slow.

**Exit**: a refused-then-retried flush's own records do not by themselves make
`logd` flush again without end, shown by a boot that refuses every flush's
first attempt and whose log goes quiet.
