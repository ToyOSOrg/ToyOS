---
status: open
kind: tooling
opened: 2026-09-28
---

# A thread join keeping its answer is gated only by a nightly guest

`toyos_proclife::join::Join` keeps the answer of the ask that collected the
zombie, and `a_join_asked_after_it_collected_keeps_its_answer` holds that.
`kernel/src/syscall/proc.rs`'s `sys_thread_join` asks through a copy of it held
in a `Cell` and writes the copy back after each ask. PR #564's round-2 review
dropped that write-back (`join.set(asked)`), so the ask after the wait collects
again and finds no such thread: every host test and the fast tier stayed
green, and only the nightly `fpu_isolation` went red, as it does for the same
mutation of the base.

**Exit**: dropping the write-back reds a host test or a fast-tier test. Owner:
orchestrator.
