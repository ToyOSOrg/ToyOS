---
status: expected-red
kind: finding
opened: 2026-09-27
---

# `swap_crash_rolls_back`'s redial was turned away to its ceiling once on main's nightly

Main's nightly at 1ce71831 (run 36290616312), one guest shard, wide:

```
FAIL swap_crash_rolls_back: 2 finding(s):
  init's words on netd were ["accepted"] ending in None, where Restored is owed (the stream had 1 connection(s) before the ask and 1 after)
  the stream's redial was turned away 64 time(s), its ceiling of 64, and gave up
```

`ALONE swap_crash_rolls_back: GREEN` twice. It was not red on the nightly
before #527 (run 36285169430).

Twice on #536, one named test on a dev host loaded by another agent's LLVM
build, green alone right after: the stream is down for the failed binary's
whole probation (`toyos_swap::PROBATION_MS`, 5 s) until init restores netd,
the harness's redial ceiling is 64 refusals and not a time, and the console
of the same runs carries every word on time (failed at 5.953 s, restored at
5.985 s).

Not shown: what turned the redial away 64 times, and why.

**Exit**: a cause for a redial turned away to its ceiling on a swap that
rolled back, or a rate with enough runs to call it gone.
