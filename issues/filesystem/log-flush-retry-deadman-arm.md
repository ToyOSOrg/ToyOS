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

**Exit condition.** The deadman boot produces the record the test reads, or the
test reads the record that boot actually produces — decided by which error
`SYS_FSYNC` returned, which is a value nothing on that console prints today.
