---
status: open
kind: tooling
opened: 2026-09-01
---

# `log_flush_retry` reds two out of five runs, on two different assertions, and is not on the redlist

Measured on the dev host in one session while gating a branch whose whole diff
is doc comments and one tracker file. Five runs of
`cargo test --test toyos-build -- --nightly log_flush_retry` on the branch head
(`08ba695e`, `main` `750b1a72` merged in): **runs 1, 3, 4 green; runs 2 and 5
red.** Five runs of the same command on `750b1a72` itself, same session, same
host, detached: **run 2 red, four green.** So it reds on `main` alone, and this
branch did not introduce it.

`cargo run -- --known-red log_flush_retry` answers:

```
log_flush_retry: NOT ON THE LIST

  No measurement in this index has ever named it. That is not a claim that it is
  green — it is a claim that nobody wrote down a rate for it.
```

**It reds two ways, which is why this is not a scheduling classification.** The
wide run's assertion and the alone re-run's assertion are different in one of
the two red runs, and the harness says so itself:

```
  ALONE log_flush_retry: red again on a DIFFERENT failure — it failed twice, on two assertions, so this is not one defect reproduced and the divergence is itself the finding.
      wide:  the deadman never declared the volume failed:
```

and reproduces identically alone in the other:

```
  ALONE log_flush_retry: red again, the same failure both times — the defect is real. the deadman never declared the volume failed:
```

The two assertions are the test's `[hung]` and `[deadman]` arms:

```
FAIL log_flush_retry: no "transport broke on SCSI" in the log, so the staged hung device never met its recovery:
FAIL log_flush_retry: the deadman never declared the volume failed:
```

Both red boots reach `log-volume: partition mounted` and then either
`logd: cannot create /log/<stamp>.log: other error` or
`logd: /log has not answered (the sync: other error)`, which is the arm's own
staged refusal arriving where the assertion did not expect it — the guest is
alive and answering, not wedged.

The `[retry]` arm was green in every run seen, red or not:
`fsync: /log/<stamp>.log durable on attempt 2 after 29ms` and
`volume kept, checker silent, 41097 blob bytes byte-identical after a
once-refused flush`.

**Exit condition.** A `src/redlist.rs` row carries this name with the measured
rate rather than a `Seen`, or the arm is fixed at its owner so the name is green
at whatever rate a session can measure. `ALONE: GREEN` is not available as the
answer here: two of the three alone re-runs in the A/B above were red, one of
them on a different assertion than the wide run.
