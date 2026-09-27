---
status: open
kind: finding
opened: 2026-09-26
---

# `blocking_read_window` completed 21 of 500 round trips beside other guests

Fast tier at `62ef89a4` (PR #525's branch, other worktrees' guests running):
`blocking_read_stress: only 21 of 500 round trips completed inside 3s — a wake
was not delivered`; the process's own line gave `syscall_wall=3087ms` and
`cpu=1703ms` for pid 7, and cpu0 took 169 xHCI interrupts in the window. The
harness's re-run alone was green. `cargo run -- --known-red` answers NO.

In the same session the fast tier ran six times, three on the branch and three
on `main` at `d65446cc`, interleaved: this test was red once on the branch and
never on `main`, while `main`'s third run had five reds of its own that the
branch never showed. The branch's kernel differs from `main` in the claim
DMA paths, which this test does not reach, and in one boot log line.

**Exit**: a cause — a lost wake, or a 3 s budget a starved host cannot meet —
and, if it is the budget, the bound derived rather than measured.
