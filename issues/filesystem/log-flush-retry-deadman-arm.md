---
status: open
kind: defect
opened: 2026-09-08
---

# `log_flush_retry`'s deadman arm reds on `metal-suite`, and the kernel's own refusal line is what is missing

`cargo test --test toyos-build -- --nightly log_flush_retry` fails on
`origin/metal-suite` (measured at d15f8e86, and again at 30d89859 with the
durability work on top — the same failure and the same words on both, so it is
the branch's and not that work's):

```
FAIL log_flush_retry: the deadman never declared the volume failed:
```

The test's second boot (`tests/common/volumes.rs`, "the deadman expires, and
that is a declared death") arms `fsync-budget-spent` and `fsync-deadman-now` and
then looks on the console for the kernel record
`fsync: <path> is not durable after N attempt(s) in …`
(`kernel/src/object/ops.rs`'s `fsync`). The boot produces `logd`'s own
give-up line —

```
logd: /log has not answered (the sync: other error) - this boot's log is on the
console only from /log/2026-09-07-235907.log
```

— which says `SYS_FSYNC` answered with something that was **not**
`WouldBlock` (`policy::fate`'s `GiveUp` arm takes every kind but that one). So
the volume died, but it died on an arm that never reaches the deadman's own
line: `fsync` returns a device error straight through, and only the
`Err(WouldBlock)` path with `deadman.reached` logs what the test reads.

The first boot of the same test passes, so the retry half is sound; what is
unresolved is whether the second boot's refusal is the deadman it is supposed to
stage or a different error arriving first, and the console does not say which.

## Two older ways it reddened, not re-seen since ROOT-in-memory

`d5c2d9c9^:issues/boot-media/log-flush-retry-reds-two-ways-at-two-in-five.md`
recorded four ways `log_flush_retry` had been seen to red; `d5c2d9c9` deleted
it once ROOT-in-memory gave the `[hung]` arm's own mechanism — a stick going
offline while userland is still paged from it — a fix and six green runs. Two
of its other ways were not re-seen in those six runs, so neither is known fixed
by that work:

- **The `[hung]` arm missing its "transport broke on SCSI" line.** One of that
  file's two originally-recorded assertions: the wide run reds on
  `the deadman never declared the volume failed` while the *alone* re-run of
  the same boot reds instead on
  `no "transport broke on SCSI" in the log, so the staged hung device never
  met its recovery` — two different assertions from the same staged fault,
  which was that file's reason for treating this as more than a scheduling
  classification.
- **A boot timeout before `===READY===`.** Measured 2026-09-07 on three
  separate points (`main` 71ed50cf, `metal-suite` 7f16914d and f63bce1d), each
  run alone: `log_flush_retry: [qemu] Boot timed out waiting for
  ===READY===`, with the boot never coming up at all — a third shape beside
  the two assertion failures, seen before ROOT-in-memory and on a branch that
  does not carry it.

## Exit condition

The deadman boot produces the record the test reads, or the test reads the
record that boot actually produces — decided by which error `SYS_FSYNC`
returned, which is a value nothing on that console prints today.

And `log_flush_retry` is re-measured (wide and alone) enough times, on a tree
that carries ROOT-in-memory, to say whether either older mode still occurs; if
seen again, it is disabled with a `src/redlist.rs` row and this file, or it is
fixed at its owner (`kernel/src/drivers/xhci` for the transport line, the boot
harness for the timeout). If not seen in that many runs, that half closes with
the count that supports it.
