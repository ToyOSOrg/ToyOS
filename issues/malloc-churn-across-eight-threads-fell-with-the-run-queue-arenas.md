---
status: open
kind: defect
opened: 2026-10-24
---

# `mem.malloc_churn.1048576.t8` fell with the run-queue arenas

Eight threads churning 1 MiB blocks through std's allocator, which takes one
futex mutex per call (`sdk/std/sys/alloc.rs`), run fewer operations on the T14
since #1061. The single-threaded row is flat. Measured in one session each
(kops/s, p50):

| boot set | `main` 32046264b | run-queue arenas 21b60ba8b | #1061 head |
|---|---|---|---|
| bisect | 3889 | 3536 | 3357 (round-1 head) |
| diary instrument | 4419 | 3406 | 3234 |
| C1E auto-promotion cleared | 3677 | — | 3411 |

Each cell is one boot, and `main` itself spans 3677–4419 across sessions. The
branch reads the same with C1E on or cleared, 3406 and 3413 against 3411. The
gap narrows under the control only because `main` fell, from 3875–3889 to
3677. So C1E is not what costs the branch here. The drop begins at the
arenas commit, which changes no scheduling decision by reading; it makes a wake
and a block cheaper. The diary shows scheduler events at well under one per
hundred operations, with no change in kind between `main` and the branch. The
cause is not named. The candidate is a contended mutex's handoff: a cheaper park
leaves less time in which a waiter finds the lock free before it parks.

Against this, the same commits halve the futex, pipe and IPC round trips
(#1061).

Owner: `issues/toyos-uses-what-modern-hardware-offers-for-speed.md`, the speed
track the change was made under.

**Exit**: the cause named by a measurement, such as futex waits and wakes per
operation on `main` and the branch in one session, and then either the
handoff fixed where it lives or the cost accepted by the owner against what it
buys.
