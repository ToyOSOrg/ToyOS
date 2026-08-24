---
status: open
kind: defect
opened: 2026-08-24
---

# The write-back drain's second budget retry panics `iod`: already armed on a subject

`iod::body` arms `writeback::WORK` once and holds that arm across its whole
loop (`iod.rs`, and the comment on `writeback::WORK` says so: "it holds one arm
across its whole loop"). Inside that loop it calls `writeback::drain_all`, whose
budget-retry ladder calls `block::between_attempts(attempt)`. For `attempt >= 2`
that parks via `completion::wait_until` → `completion::arm`, which asserts
`!inbox.is_armed()`. The `iod` task is already armed on `WORK`, so the second
retry trips the assert:

```
PANIC: panicked at src/completion/mod.rs:180:5:
completion::arm: this task is already armed on a subject
  kernel::completion::arm
  kernel::block::between_attempts
  kernel::writeback::drain_all
  kernel::iod::body
```

`between_attempts(1)` only `yield_now()`s (no arm), so a single budget refusal is
harmless — which is why `writeback_durability` is green alone and green in CI:
the `fat-mirror-write-refuse` actuator stages exactly one refusal, the drain
retries once at `attempt == 1`, and it never reaches the parking branch. The
panic needs the drain to reach `attempt >= 2`, i.e. a *second* budget refusal on
the same drain pass. Reproduced on 2026-08-24 on the dev host under a 12-wide
`cargo test`: the injected mirror refusal plus a real USB operation-budget
timeout under host contention (`usb-storage: ... ran out of its operation
budget`, `transport broke ... no answer in the status phase in 2000 ms`) drove
the drain to `attempt == 2` and `between_attempts(2)` panicked `iod`. The
machine-wide panic reds whichever guest was on that boot — here
`writeback_durability`, which the harness's `ALONE` re-run then found green,
naming its `Sched::Parallel` rather than the cause.

Landed with #267 (the write-back drain's budget retry; `block::between_attempts`
is shared with `SYS_FSYNC`'s fsync loop in `object/ops.rs`, which parks on the
*syscall thread's* own watch and is not pre-armed — so the defect is specific to
the drain running inside `iod`'s standing arm, not to the ladder itself).

Found while merging the FAT read-side revocation (#262) onto main; the merge did
not touch `iod.rs`, `block.rs`, `completion/` or `writeback.rs`, and the panic
backtrace is entirely inside those. Filed, not fixed: it is #267's, and the fix
is the owner's to shape — either the drain releases `iod`'s standing arm before
`between_attempts` parks, or `between_attempts` yields rather than parks when the
task is already armed, or the drain retry does not run under a held arm.
