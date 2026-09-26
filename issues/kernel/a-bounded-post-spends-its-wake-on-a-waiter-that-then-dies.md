---
status: open
kind: defect
opened: 2026-09-26
---

# A bounded post spends its wake on a waiter that then answers Killed

`toyos-sched/src/watch.rs`'s `Watch::post_n` counts a waiter whose word it
claimed `Committing → WakeQueued` (`Notify::PrePark`) as woken. That waiter's
own commit reads the kill bit before the claim (`park::WaitTicket::commit`) and
answers `Commit::Killed`, so the claim was spent on a thread that unwinds and
never looks at the futex word. `futex_wake(addr, 1)` then answers 1 while a live
waiter registered on the same token stays parked: on a shared frame, a lost
wake for a process the killed one had nothing to do with.

It predates the watch: main's `completion::post_n` counted the same pre-park
claim through `scheduler::wake_sched`.

## Exit condition

A bounded post never counts a waiter whose commit answers `Killed` — a model in
`toyos-sched/loom/tests/loom_watch.rs` with two waiters on one token, one of
them killed while committing, and a `post_n(token, 1)` racing it, where the live
one must be reached.
