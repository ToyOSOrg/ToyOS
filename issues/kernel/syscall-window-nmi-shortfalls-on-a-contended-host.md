---
status: expected-red
kind: tooling
opened: 2026-08-23
---

# `syscall_window_nmi` under-counts window arrivals on a contended host

First seen on a dev host running a second worktree's 12-wide suite against the
same twelve guest slots (2026-08-23):

```
FAIL syscall_window_nmi: 44 window arrivals against 572 in Ring 3. Every
iteration passes through both exactly once, so they are of one order; a 10x
shortfall says the arrivals are not being classified where they land
```

Green in the same session's alone re-run (4 s) and green again on a quiet
re-run of the same tree. `cargo run -- --known-red syscall_window_nmi` said
`NOT ON THE LIST` at the time, so no rate had ever been written down for it.

It has since red in the orchestrator's own runs, on three different branches,
each with the same "N sprayed window arrivals against M in Ring 3 ... a 10x
shortfall" message:

| log | head | arrivals | Ring 3 |
|---|---|---|---|
| `536r15-nightly.log:956` | `069722c3` | 39 | 436 |
| `536r14-nightly.log:736` | `06c6195f` | 36 | 557 |
| `557r2-fast.log:822`     | `c5d09bb6` | 24 | 515 |

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
instrument. Disabled at `src/redlist.rs` behind this file until then;
`cargo run -- --known-red syscall_window_nmi` now answers disabled.
