---
status: open
kind: tooling
opened: 2026-09-01
---

# `ftruncate_flush_race` reds intermittently, and no instrument says which of two opposite things happened

The test races a truncate against a flush the `ftruncate-flush-stall` actuator
holds open for 400 ms, and panics with "the resize does not serialise with the
metadata window" when no attempt waited 150 ms. It reds on the dev host without
that being true, and nothing in the capture separates the two cases.

Measured on the dev host (14 cores), one session, against a branch carrying the
shrink-mark work and against the same tree with that kernel change reverted:

| arm | condition | runs | result |
|---|---|---|---|
| branch | alone | 6 | 6 pass; waits 355.1 / 356.2 / 357.5 / 358.2 / 358.6 / 369.4 ms |
| kernel change reverted | alone | 3 | 1 fail, then 2 pass (attempt 0, 358.0 and 356.3 ms) |
| branch | full nightly tier | 3 | 2 fail, 1 pass |
| kernel change reverted | full nightly tier | 1 | pass |

Both arms fail and both pass, and the only same-configuration comparison with
more than one sample per side is the *alone* one, where the branch is 6 for 6
and the reverted arm failed once. So the numbers say intermittent; they do not
name a cause and they do not point at a diff.

**No mechanism is established, and one plausible reading is already refuted.**
`STALLED WINDOW HELD` is not evidence that the truncate arrived after the
window: `SYS_FTRUNCATE` and `SYS_FSYNC` take the same VFS lock and the stall
spins inside it, so a serialised truncate can never land in the window and the
line prints on every attempt, passing or failing. `tests/common/volumes.rs`
requires `HELD` for a *pass* and refuses `BROKEN`, which is the same statement
from the other side.

What the capture does not hold is when `set_len` entered the kernel relative to
the stall's start. Without it a short `waited` is either a resize that did not
serialise — the defect this gate exists for — or a stimulus that arrived after
the 400 ms window closed, which is a retry; and the guest's own docstring says
the loop is one-sided precisely because a sleep can overshoot. Whoever takes
this decides what to measure; this entry is the rate and the refutation, not a
design.

`cargo run -- --known-red ftruncate_flush_race` says `NOT ON THE LIST`.
