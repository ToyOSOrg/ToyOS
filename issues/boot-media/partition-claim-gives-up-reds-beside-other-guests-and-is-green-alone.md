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

**A rate, from `wt/toyos-inspect` (PR #501, round-2 review fixes), same day,
different diff.** `cargo test --test toyos-build -- partition_claim_gives_up`
alone, 7 back-to-back runs on one dev host: 6 green, 1 red (the deadman case,
same message as above — the boot's own `/log` fsync tripped the actuator's
global deadman first: `log-volume: write ... the device would not answer`,
`fsync: /log/... is not durable after 1 attempt(s)`, before
`test_rs_partition_claimant` ever spawned). 3 runs on the pre-round-2 tree
(`git stash`) were 3 of 3 green. About 1 in 7 to 1 in 10, consistent with "1 in
3" above and with no rate either — both are single sessions.

Five back-to-back full `cargo test` runs in the same session (nothing else on
the host): 376/376, then `partition_claim_gives_up`'s deadman case alone (as
above), then `partition_claim_departure`'s `silent` case (`0 flushes were told
of the loss, not 1` — its own doc comment names the identical race, "another
claim flushes first — logd's `/log`") plus two unrelated `swap_*` names, then
376/376 again, then `syscall_window_nmi` (this repo's other wall-clock-storm
row, `issues/build/parallel-tests-red-under-other-suites.md`) plus this test's
*other* case (`unanswered`). No two of the five runs named the same failing
test, and none of the five failures is in a file this round's diff touches
(`kernel/src/block.rs`, `device.rs`, `gpt.rs`, `page_cache.rs`,
`fat32_adapter.rs`, `pcidev/mod.rs`, the xHCI PSIV decode, `toyos-abi`,
`toyos-inspect`, soundd's `inspect.rs`/`mix.rs`) — this round's own dedicated
tests (`inspect_reads_its_owners`, `toyos-inspect`'s and `toyos-abi`'s host
suites, soundd's host tests) stayed green across all five. `uptime` mid-session
read 1-minute 1.92 against 5- and 15-minute 8.30 and 11.26: a host still
carrying the previous runs' own weight, not a second worktree's. Confirms the
race (a global, unscoped `fsync_deadman_now`/`fsync_budget_spent` racing
whichever fsync the boot reaches first) is real and independent of this
session's diff, and widens the exit condition beyond `partition_claim_gives_up`
alone: `partition_claim_departure`'s `silent` case names the same premise in
its own doc comment and should get the same fix.
