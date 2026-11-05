---
status: open
kind: defect
opened: 2026-11-04
---

# Every descriptor call takes one process-wide lock twice

A `read` or `write` through a descriptor (`sdk/std/sys/pal/mod.rs`'s `on_fd`)
holds its object for the call through `toyos::fd::hold`. That takes the
process's one descriptor-table lock (`sdk/toyos/src/fd.rs`, `TABLE`) to acquire
the slot and again to let it go. Two threads of one process doing I/O on
different descriptors meet on that lock twice per call.

The lock was a plain futex lock, so any collision slept in `FUTEX_WAIT` and
woke through `FUTEX_WAKE`. In the S0-a metal A/B (#1150, round 2), the pipe
benchmark's writer and reader threads (`osbench` `pipe.throughput.*`) made
about 125-150 futex waits a run on main. With the kernel no longer
serializing their pipe calls under its process lock, they made 128,000-153,000
on the T14. That was much of the small-pipe-write regression measured there.

`toyos::sync::lock_word` now spins briefly before sleeping, which turns a
collision into a short wait. Spinning bounds the cost of a collision; it does
not stop two threads meeting on a lock neither needs.

**Exit**: a descriptor's hold for one call takes no lock that a call on another
descriptor takes.
