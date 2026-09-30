---
status: open
kind: tooling
opened: 2026-08-23
---

# `syscall_window_nmi` under-counts window arrivals on a contended host

```
FAIL syscall_window_nmi: 44 window arrivals against 572 in Ring 3. Every
iteration passes through both exactly once, so they are of one order; a 10x
shortfall says the arrivals are not being classified where they land
```

Also seen in the orchestrator's own runs at a main-level head, `c5d09bb6`:

```
FAIL syscall_window_nmi: 24 sprayed window arrivals against 515 in Ring 3. Every
iteration passes through both exactly once, so they are of one order; a 10x
shortfall says the arrivals are not being classified where they land
```

Two readings and nothing here separates them: the storming CPU genuinely lands
in the three-instruction window less often when the host is oversubscribed —
which would make the assertion a bound on the *host* rather than on the
classification — or arrivals really are being classified somewhere else and the
contention only makes it visible. The assertion is a ratio, so the first
reading has to be excluded before the second is investigated, and that needs a
rate measured on a host whose company is recorded (`tests/CLAUDE.md`).

Not `Sched::Parallel` being wrong. The harness suggests that on every alone-green
red, and re-classifying a red whose mechanism is unknown answers nothing.

**2026-08-25.** A test that reds on a loaded host with no
rate written down is an unadjudicated red, and CLAUDE.md's rule is that such a
red is fixed at its owner rather than re-run away. The act is a measurement: a
window-arrival rate taken across widths on hosts whose company is recorded, so
the host reading can be excluded before the classification reading is
investigated. Until that exists nothing can decide whether the assertion bounds
this kernel or the dev host. Owed by whoever next runs a load sweep on this
instrument.

**Its test is deleted**: `4600f6754` took `syscall_window_nmi` out, and
`539977050` its IST-off control in `syscall_window_nmi_controls` with
`nmi-without-ist`, the storm's hold in the syscall entry and the report the
two read. `git revert 539977050 4600f6754` brings both back.
`syscall_window_nmi_controls` keeps its nested arm.
