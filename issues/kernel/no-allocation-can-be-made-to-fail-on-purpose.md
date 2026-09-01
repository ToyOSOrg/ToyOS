---
status: open
kind: track
opened: 2026-09-01
---

# No allocation in this kernel can be made to fail on purpose, so the terminal path is untested

`git grep alloc_error_handler -- kernel/` returns nothing: the kernel has no
allocation-error handler at all, which is what
`issues/kernel/no-alloc-error-handler.md` records. The global allocator can
return null before initialization and otherwise delegates straight to dlmalloc,
and there is no way to make the *next* allocation fail while a terminal observer
is still able to report.

An ordinary stress test cannot substitute. Exhausting the heap changes every
other thing the kernel is doing at the same time, and the report you need is
written by a path that must not itself allocate.

**What to build.** A test-only countdown actuator: arm it with N, and the Nth
allocation after arming fails. Enter it immediately before a known allocation.
Capture the result over serial and the panel — channels that do not allocate —
and enforce a host deadline so "nothing was reported" is a verdict.

**Two properties it owes.** Its own accounting must not allocate, and it must not
perturb allocator order — proved by showing that with the countdown disabled the
allocator's behaviour is byte-for-byte what it was. The result channel is
preallocated at arm time, before the failure it is there to report.

**Reuse.** Every allocation-failure and panic-path test wants this; today none of
them can be written honestly.
