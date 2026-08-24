---
status: open
kind: defect
opened: 2026-08-23
---

# `syscall_window_nmi` under-counts window arrivals on a contended host

Seen once in passing, on a dev host running a second worktree's 12-wide suite
against the same twelve guest slots (2026-08-23):

```
FAIL syscall_window_nmi: 44 window arrivals against 572 in Ring 3. Every
iteration passes through both exactly once, so they are of one order; a 10x
shortfall says the arrivals are not being classified where they land
```

Green in the same session's alone re-run (4 s) and green again on a quiet
re-run of the same tree. `cargo run -- --known-red syscall_window_nmi` says
`NOT ON THE LIST`, so no rate has ever been written down for it and this is the
first datum rather than a regression against one.

Two readings and nothing here separates them: the storming CPU genuinely lands
in the three-instruction window less often when the host is oversubscribed —
which would make the assertion a bound on the *host* rather than on the
classification — or arrivals really are being classified somewhere else and the
contention only makes it visible. The assertion is a ratio, so the first
reading has to be excluded before the second is investigated, and that needs a
rate measured on a host whose company is recorded (`tests/CLAUDE.md`).

Not `Sched::Parallel` being wrong. The harness suggests that on every alone-green
red, and re-classifying a red whose mechanism is unknown answers nothing.

**2026-08-25, promoted to `defect`.** A test that reds on a loaded host with no
rate written down is an unadjudicated red, and CLAUDE.md's rule is that such a
red is fixed at its owner rather than re-run away. The act is a measurement: a
window-arrival rate taken across widths on hosts whose company is recorded, so
the host reading can be excluded before the classification reading is
investigated. Until that exists nothing can decide whether the assertion bounds
this kernel or the dev host. Owed by whoever next runs a load sweep on this
instrument; `cargo run -- --known-red syscall_window_nmi` still answers `NOT ON
THE LIST`.
