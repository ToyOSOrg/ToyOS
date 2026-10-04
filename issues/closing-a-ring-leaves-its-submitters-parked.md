---
status: open
kind: defect
opened: 2026-09-26
---

# Closing a ring leaves a thread parked in its submit

`kernel/src/inbox/mod.rs`'s `InboxRef::drop` tears the ring down — takes its
state, withdraws its polls, unmaps its page — and posts nothing to the ring's
own `watch`. A sibling thread parked in `inbox_submit` with a `u64::MAX`
deadline waits there for a completion; its predicate reads a torn-down ring as
satisfied, but nothing wakes it to read it, so it stays parked until something
else happens to post that watch, which after the teardown nothing does.

It predates the watch: main's teardown posted no completion to the waiter's
inbox either.

## Exit condition

The teardown posts the ring's watch after it takes the state, and a guest test
with one thread parked in an unbounded submit and a sibling closing the ring's
handle sees the submit return.
