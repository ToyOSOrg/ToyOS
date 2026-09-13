---
status: open
kind: defect
opened: 2026-09-13
---

# A `tlb_shootdown_waits` red on CI is not a slow host

`tlb_shootdown_waits` asserts that `munmap` does not return before the last
CPU in the shootdown's target set has answered, by holding one CPU's answer
back and timing the call while a sibling thread provably executes on another
CPU. Its redlist row (`src/redlist.rs`, instrument CI) records the red as the
test's own control firing — the one assertion in the suite that cannot tell a
slow host from a broken measurement — and points at the dev-host load class.

CI is not that class: one guest per machine, nothing beside it. It has now
red there twice, 24 days apart, each time green when re-run alone in the same
job:

- PR #150 run 32334225614, job 96320634405 (`guest (5)`), 2026-08-20.
- PR #451 run 34761663167, job 103735485106 (`guest (10)`), 2026-09-13, on a
  branch whose whole diff is two sentences in `src/CLAUDE.md`:
  `munmap returned in 19584ns with the last CPU answering 20000000ns late,
  while a sibling on another CPU provably executed through the call — it
  freed the pages without waiting for the flush`.

So one of two things is true, and nothing in reach says which: the kernel
frees a mapping's pages before every CPU that may hold it in its TLB has
flushed — a use-after-free of physical memory under a racing thread, which
is a kernel defect of the first order — or the test's precondition
(`sibling_running_elsewhere`) does not establish that the sibling's CPU is in
the target set the kernel computes, so a fast return is correct and the
control is unsound. The re-run-alone green is not evidence either way: it is
one more sample of a rate.

**Exit condition.** An instrument that tells the two apart: the kernel's
shootdown records its target set and each CPU's answer time in a record the
test reads back beside its own timing, so a red says which CPU the kernel
waited for and which it did not. Then either the race is fixed with that
record as the negative control, or the test's precondition is tightened to
the target set the kernel computes and the row is retired against that.

Until then the row stands, and a CI red under this name is this defect and
not a re-run.
