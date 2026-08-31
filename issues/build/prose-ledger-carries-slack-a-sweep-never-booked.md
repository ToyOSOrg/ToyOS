---
status: open
kind: defect
opened: 2026-08-31
---

# 25 files sit under their `src/prose-ledger` entry, unbooked

`src/prosegate.rs`'s own test prints every file measured below its ledger
row (`shrinkage`, `:298-311`) but only asserts on refusals — a file *under*
its entry never reds, so the win sits unbooked until someone runs the test
with output and rewrites the rows. Re-measured at `origin/main` (2bceaadb),
2026-08-31, `cargo test -p toyos-build --lib
prosegate::tests::no_file_carries_more_prose_than_the_ledger_permits --
--nocapture`:

```
25 file(s) are under their entry. A sweep books the win by replacing these rows in src/prose-ledger and lowering `DATED_TOTAL` to match:
bcachefs/src/block_io.rs 112 0
bcachefs/src/btree.rs 120 0
bcachefs/src/fs.rs 348 0
kernel-loom/tests/poison_set.rs 15 0
kernel/src/arch/syscall/dispatch.rs 93 0
kernel/src/arch/syscall/vm.rs 81 0
kernel/src/drivers/xhci/hid.rs 25 0
kernel/src/net.rs 14 0
kernel/src/page_cache.rs 45 0
kernel/src/revoke_selftest.rs 6 0
kernel/src/sched/poison.rs 14 0
kernel/src/scheduler.rs 149 0
kernel/src/tmpfs.rs 19 0
src/libc.rs 29 0
src/tiers.rs 336 26
tests/toyos-rust-tests/src/bin/dlopen_dedup.rs 12 0
tests/toyos-rust-tests/src/bin/fat_backing_revoked.rs 56 0
tests/toyos-rust-tests/src/bin/netd_listener_forgery.rs 14 0
tests/toyos-rust-tests/src/bin/pipe_flag_forgery.rs 15 0
tests/toyos-rust-tests/src/bin/readdir_bound.rs 40 0
tests/toyos.rs 4965 23
toyos-cc/tests/ppnumber.rs 18 0
toyos-gpt/src/lib.rs 166 0
toyos-gpt/tests/parse.rs 86 0
userland/netd/src/main.rs 241 0
```

The #345 review reported slack on a different file set entirely
(`poll_wake_pipe.rs`, `ftruncate_flush_race.rs`, `bar.rs`, `interleave.rs`,
`model.rs`) — none of those numbers hold at `origin/main` today:
`poll_wake_pipe.rs` and `ftruncate_flush_race.rs` do not exist in this tree
(`rg -l` over the whole checkout finds neither), and `src/prose-ledger`'s
current rows for `toyos-pci/src/bar.rs` (109), `toyos-proclife/src/interleave.rs`
(150) and `toyos-proclife/src/model.rs` (79) don't match the reviewed numbers
and aren't in the shrinkage list above — that review ran against an unmerged
branch's tree, not `main`'s. The underlying defect class is real and current
on `main` regardless, per the list just measured.

Exit: a sweep replaces each row above in `src/prose-ledger` with its measured
`comments dated` pair and lowers `DATED_TOTAL` by the sum of the `dated`
deltas.

Provenance: adversarial review of PR #345 (file set), re-measured against
`origin/main` for this filing.
