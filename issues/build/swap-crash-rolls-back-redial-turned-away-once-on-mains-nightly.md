---
status: open
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
`cargo run -- --known-red swap_crash_rolls_back` answers NO.

Not shown: why `logd` was still turning the redial away, 64 times, after the
old netd was gone.

**Exit**: a cause for a redial turned away to its ceiling on a swap that
rolled back, or a rate with enough runs to call it gone.
