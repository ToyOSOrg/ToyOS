---
status: open
kind: tooling
opened: 2026-09-30
---

# The two-watch poll model counts a dropped watch's answer as its completion

`kernel/loom/tests/loom_watch.rs`'s
`a_poll_on_two_watches_racing_both_posts_completes_exactly_once` moves both
worlds into their producer threads, so both watches drop before its assertion,
and a watch's drop fires every live entry as `Fire::Gone`, which the model's
`Entry` counts as a post. Its "never by neither" half cannot fail: a poll no
post and no recheck completed is completed by the drop.

**Evidence:** with each producer posting before it makes its condition true (a
lost completion by construction), `cargo test -p toyos-sched-loom --test
loom_watch a_poll_on_two_watches` is EXIT=0, 1 passed. `poll_racing`, the
single-watch model beside it, had the same shape and holds its world past the
assertion since #634.

**Exit:** the model keeps both worlds alive past its assertions, and the
mutation above reds it.
