---
status: open
kind: tooling
opened: 2026-09-27
---

# `fs_turns` judges the scheduler with the lock

`tests/toyos-rust-tests/src/bin/fs_turns.rs` asserts that every writer sharing
one directory's connection made at least 16 passes while the first made 32. A
writer the scheduler — or the host, under TCG — keeps off-CPU between its reply
and its next request holds no ticket in that time, and the others pass it with
no lock at fault, so the floor reds on a slow schedule as well as on a lock that
lets its releaser take it straight back.

## Exit condition

The verdict separates ticket-order handover from a lock that barges whatever
the schedule: a check on the order requests are served in against the order
they were queued in, or the handover decided in a host-tested crate and
checked there.
