---
status: open
kind: defect
opened: 2026-09-24
---

# `home_budget_refusal_retried` never sees its `/home` record while `/log`'s own retries flood the ring

**Owner:** the `/home` fsync path: `kernel/src/object/ops.rs::fsync` over
`kernel/src/bcachefs_adapter.rs`, and its judge in
`tests/common/storage.rs::home_budget_refusal_retried`.
**Exit condition:** it is known whether the `/home` fsync's first attempt was
refused and its record dropped, or never refused at all. The cause is fixed
there, and the test is green on the nightly again.

Red on 8 runs from 09-09 to 09-24: seven nightly runs and one dispatch run. The
`ALONE` re-run was red too on 7 of them. The exception is 09-17 (job
105128291931), where it was green. Each red run fails the same way:

```
FAIL home_budget_refusal_retried: no `fsync: /home/... durable on attempt` line — the retry never ran:
```

Each run's log is under
`/Users/jan/.claude/jobs/2280e09e/tmp/scratchpad/flake-census/logs/<job>.log`,
with jobs 102381293976, 102787750252, 103694736798, 103908808233,
105128291931, 105642821532, 107098149016 and 107546143128. Here is what those
logs show:

- **The guest succeeds every time.** `test_rs_home_fsync_budget` exits 0, and
  `budget-refused fsync retried to durable: 12329 bytes on /home` is on the wire.
  The guest cannot tell a retried fsync from one that was never refused, so that
  line proves nothing about the retry.
- **No red capture has a kernel `fsync: /home/…` record.** The one green `ALONE`
  capture has one:
  `[kernel 14.357 cpu0] fsync: /home/f9-budget.bin durable on attempt 2 after 1ms`.
- **`fsync-budget-spent` refuses every fsync in the boot once, `/log`'s included,
  and the retry of each one writes a record, which logd then writes and fsyncs.**
  - Each red job carries 817 to 2,272 `usb-storage: … SCSI 0x2a not issued`
    lines.
  - `/log` rolled over its 1 MB file 4 to 9 times within the job.
  - The `/home` record would come from the same `crate::log!` call site as all of
    those.
- **The judge's staging check reads the wrong speaker.** It asks for any
  `not issued` line, and its message says "no NVMe refusal". But
  `NVMe: … not issued` appears 0 times in all eight jobs, the green one included.
  So the usb-storage refusals on `/log` are what satisfy the check.

The two readings are not yet separated:

1. The record was committed and then dropped. The kernel ring is under a storm
   of the same record from `/log`.
2. The `/home` first attempt was never refused. The actuator's spent operation
   did not reach the bcachefs flush, so it was durable on attempt 1 and wrote no
   record.

The fact that NVMe logs no `not issued` even on the green run suggests the
refusal on `/home` happens above the driver. That is not shown.

Nothing else proves this path. `nvme_home_roundtrip` fsyncs `/home` with no
refusal staged. `home_backing_revoked` exists only as a name in
`tests/toyos-rust-tests/src/bin/fat_backing_revoked.rs`'s doc and is registered
nowhere. The only other stager of `fsync-budget-spent` is `log_flush_retry`,
which covers FAT `/log` over USB and not bcachefs `/home` over NVMe.
