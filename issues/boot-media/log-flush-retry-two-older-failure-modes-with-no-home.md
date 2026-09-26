---
status: open
kind: tooling
opened: 2026-09-08
---

# `log_flush_retry` has two older failure modes that lost their tracker file

`issues/boot-media/log-flush-retry-reds-two-ways-at-two-in-five.md` recorded
four ways `cargo test --test toyos-build -- --nightly log_flush_retry` had
been seen to red; that file was deleted (`d5c2d9c9`) once ROOT-in-memory gave
the `[hung]` arm's own mechanism — a stick going offline while userland is
still paged from it — a fix and six green runs. Two of its other ways were not
re-seen in those six runs and so were not carried anywhere:

- **The `[hung]` arm missing its "transport broke on SCSI" line.** One of the
  file's two originally-recorded assertions: the wide run reds on
  `the deadman never declared the volume failed` while the *alone* re-run of
  the same boot reds instead on
  `no "transport broke on SCSI" in the log, so the staged hung device never
  met its recovery` — two different assertions from the same staged fault,
  which was the file's reason for treating this as more than a scheduling
  classification.
- **A boot timeout before `===READY===`.** Measured 2026-09-07 on three
  separate points (`main` 71ed50cf, `metal-suite` 7f16914d and f63bce1d), each
  run alone: `log_flush_retry: [qemu] Boot timed out waiting for
  ===READY===`, with the boot never coming up at all — a third shape beside
  the two assertion failures, seen before ROOT-in-memory and on a branch that
  does not carry it.

Neither mode was seen in the six runs that closed the deleted file, so neither
is known fixed by that work.

## Exit condition

Re-measure `log_flush_retry` (wide and alone) enough times, on a tree that
carries ROOT-in-memory, to say whether either mode still occurs; if seen
again, a `src/redlist.rs` row carries it with a measured rate, or it is fixed
at its owner (`kernel/src/drivers/xhci` for the transport line, the boot
harness for the timeout). If not seen in that many runs, this file closes with
the count that supports it.
