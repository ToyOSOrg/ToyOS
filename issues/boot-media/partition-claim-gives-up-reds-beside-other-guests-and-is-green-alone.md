---
status: open
kind: tooling
opened: 2026-09-25
---

# `partition_claim_gives_up` reds beside other guests and is green alone, and nobody has a rate for it

Seen on 2026-09-25 in a full fast tier on the dev host (branch `wt/toyos-spawncwd`,
head f9088ea1 plus round-3 review fixes), one run in three:

```
FAIL partition_claim_gives_up: deadman: the kernel never said "partclaim: a
write still refused after 1 attempt(s)":
  PASS  partition_claim_gives_up  (6s)
  ALONE partition_claim_gives_up: GREEN — it fails only beside other guests,
  so its Sched::Parallel is wrong. The run stays red on the classification.
```

`cargo run -- --known-red partition_claim_gives_up` answers "NO, not
quarantined — its failure fails the suite."

Re-run immediately after, alone, on the same tree and in the same session:
**green, 2 of 2**, 8-10 s each. So the observation is the same shape as
`usb_short_read`'s and `kernel_log_file`'s: 1 red in 3 with no rate behind it,
under a deadman whose 120 s ceiling and single retry attempt are timed against
wall-clock, not against the guest's own progress.

Not the diff it was seen from: that branch touches `bcachefs/`,
`kernel/src/bcachefs_adapter.rs`, `kernel/src/main.rs`'s DATA-volume arms,
`src/image.rs`, `tests/common/storage.rs` and a new `home_absent` guest
binary — nothing under `tests/common/partclaim.rs` or the kernel's `partclaim`
module.

Owed to whoever next runs a session free to measure it and, if it reproduces,
to file the `src/redlist.rs` row.
