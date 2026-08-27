---
status: open
kind: defect
opened: 2026-08-27
---

# `syscall_window_nmi` reds when the host is somebody else's too

One sighting, dev host, 2026-08-27, a 288-name `cargo test` run at 92 guests
with a second worktree's suite on the same machine:

```
FAIL syscall_window_nmi: the storm never reported — is `syscall-window-nmi` on?
  FAIL  syscall_window_nmi  (1505s)
  ALONE syscall_window_nmi: GREEN
```

The isolated re-run in the same session took **5 s** and reported the storm in
full — `3000 sent, 3000 taken, 43 in the window, 140 in Ring 3, 663 syscalls
made under the storm`. The committed price is 6,825 ms, so the wide-run reading
is a **220x wall stretch**: the guest was still working and had not finished,
which is what its own message says when the storm line has not arrived yet.

## Why this is filed rather than re-classified

`ALONE: GREEN` is the harness naming a *hypothesis* — that the name's
`Sched::Parallel` is wrong — and this file is not that claim.
`tests/CLAUDE.md` is explicit: the harness suggests scheduling, the mechanism
decides, and nothing here has measured a mechanism. What is measured is one
red at 220x its price and one green at 1x.

`cargo run -- --known-red syscall_window_nmi` answered **NOT ON THE LIST** when
this was opened; `src/redlist.rs` now carries a row sourced here so the next
agent who meets it is told whose it is.

## What it is not

Not the branch it was found on. That branch changed the syscall entry's
displacement *spelling* (`const` operands for the same immediates, byte-identical
machine code) and added two per-CPU stores per syscall for the panic path's
`in_syscall` bracket. Neither moves a 6.8 s test to 1,505 s, and the same tip
runs it green alone in 5 s.

## What would settle it

A rate. One sighting has no denominator, which is why there is no number in the
row: the same suite run repeatedly on a host with and without a second
worktree's build on it is what turns this into either a contention class the
harness should schedule around or a defect in the storm's own pacing.
`issues/build/parallel-tests-red-under-other-suites.md` is where the family
lives, and this name is not on it.
