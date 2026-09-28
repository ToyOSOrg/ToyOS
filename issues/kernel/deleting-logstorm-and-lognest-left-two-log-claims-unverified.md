---
status: open
kind: finding
opened: 2026-09-28
---

# Deleting `logstorm`/`lognest` left two log claims unverified

`issues/kernel/the-kernel-still-creates-threads.md`'s K3 deletes
`kernel/src/log/storm.rs` and `kernel/src/log/nested.rs` — the kernel threads
that generated log-write load for four guest tests
(`log_conservation_smp1`, `log_nested_emit`, `log_reserve_window`,
`log_reserve_window_negative`) — and, with them, every test that used those
producers. Two real kernel claims were checked by nothing else, and are now
checked by nothing at all.

**Same-CPU interrupt reentrancy inside `emit`'s IF/TF-off bracket.**
`log_nested_emit` and `log_reserve_window` (plus its negative control
`log_reserve_window_negative`, the only test that ever exercised
`arch::IrqGuard`'s bracket by removing it) staged a self-IPI from a kernel
thread — with `IF` set, unlike inside a syscall — landing inside another
`emit`'s own reservation/publication window. `kernel/src/log/nested.rs`'s own
header called this "the one case loom cannot express and the host cannot
stage": `kernel-loom/tests/log_record.rs` models cross-CPU memory ordering
only, since loom has no interrupts and no CPU flags to reenter. Nothing
else in the tree drives this case: `read.rs`'s `Descent::advance` (a shard's
sequence order is its timestamp order) rests on `emit`'s bracket alone, and
that rests on nothing now.

**`SYS_LOG_READ`'s conservation law under concurrent multi-shard write
load.** `log_conservation_smp1` was the only guest test that read the log
while it was being written fast enough to exercise the ring's drop-oldest
path and the reader's lost/read accounting concurrently — ordinary kernel
log traffic is far too sparse to reach it. No other test replaces this.

Reintroducing either check needs a producer that raises the load without a
kernel thread — for instance a syscall that a userland test binary calls in
a tight loop, one bound per CPU, rather than a persistent kthread — which is
a redesign outside K3's scope (delete only). Recorded here rather than
silently, per the owner's ruling that a tracked weakness stays "known,
tracked, still true," never unmentioned.
