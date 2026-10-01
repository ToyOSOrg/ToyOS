---
status: open
kind: tooling
opened: 2026-10-01
---

# `a_clean_cannot_land_inside_a_build` judges a flat 700 ms window

`src/buildlock.rs`'s `clean_racing_a_build` touches `builder-mid` and then
reads whether the cleaner child wrote `cleaner-done` within a flat 700 ms
(`appeared(&root.join("cleaner-done"), Duration::from_millis(700))`).

- Unlocked, it asserts the clean finished inside the window: a cleaner the
  host does not schedule for 700 ms reds the test with the lock working.
- Locked, it asserts the clean did not finish inside the window: that shows
  the lock held only if an unheld clean would have finished in 700 ms, which
  this run does not measure.

Not seen red.

**Exit condition**: the unlocked arm waits on `cleaner-done` under a ceiling
that fails loudly, and the locked arm reads an order of events (the clean
after the build's lock is let go), not the absence of one inside a window.
