---
status: open
kind: defect
opened: 2026-08-29
---

# A guest that ran past-EOF hole writes wedges minutes later, with every task parked and nothing runnable

Found while closing `issues/kernel/lseek-past-eof-is-silently-clamped.md`, and
the reason that close is reverted rather than landed. The reverted fix and its
test are one commit, `4c7a8f7e6219f3cc563c03cff5354c0e0765bd69`, reverted by
`af64feab1ba9b1ac2d65dbe4f5fb14137750818c` — the next investigation starts by
reverting that revert. The fix was small: `ops::seek` stops clamping to EOF,
one `MAX_FILE_SIZE` ceiling (2^44, the `u32` page index's reach) refused by
seek, write and ftruncate, and ftruncate no longer moving the seek pointer.
Its test (`lseek_past_eof` — POSIX as the oracle; hole writes on /tmp and
device-backed /home, then ceiling refusals on /tmp) answered
`PASS  lseek_past_eof  (37ms)` alone and `PASS  lseek_past_eof  (67ms)` inside
the run below that then wedged.

Every run below is `cargo test` (the full fast tier), dev host, 12-wide, TCG,
2026-08-29, one session. With the whole test active the tier ran **5** times
and the shared boot carrying it wedged in **4**: `port_poll_churn`, later in
the same guest, never finished, while the kernel's periodic reporter kept
ticking to a machine with nothing runnable —

```
[kernel 282.476 cpu0] sched: cpu=0 ready=0 dying=0 parked=3 current=None trips=7687
[kernel 322.800 cpu0] sched: cpu=0 ready=0 dying=0 parked=3 current=None trips=8467
```

(run 1's capture, first and last of the window the harness kept; run 4 and
run 5 show the same shape at `parked=5`, e.g.
`[kernel 322.904 cpu1] sched: cpu=1 ready=0 dying=0 parked=5 current=None trips=8316`).
Three of the four wedges read, in run 1's words,
`timed out after 300s, with the guest still talking 2s ago (366 console
line(s) while it ran)` — runs 4 and 5 differ only in the seconds and the
line count — and the talking is the reporter above; the fourth was
`STALLED: 180s of guard expired, and the guest had said nothing for the last
179s of it`, and the last kernel line its capture holds is the test's own
sibling thread leaving cleanly:
`[kernel 25.887 cpu1 tid=2] exit: test_rs_port_poll_churn tid=2 code=0 cpu=1134ms`.
The fifth run answered `PASS  port_poll_churn  (7s)` (that run's one red was
`exit_wait_storm`, the tracked family below).

The attribution arms, each with its denominator, all the same command:

- **main (`48437ca4`), no test: 0 wedges in 2 runs.** Each run had one red of
  the tracked loaded-host family (`i8042_mouse` once, `exit_wait_storm` once,
  both `ALONE: GREEN`), never the wedge.
- **branch, test file removed from the tree: 0 wedges in 1 run** (94.2 s wall,
  `i8042_mouse` again its one red).
- **branch, test present but neutered** (same binary and name, `main` gated
  behind `if false`): **0 wedges in 1 run** —
  `test result: ok. 301 passed, 301 total (88.2s)`, the session's only fully
  green tier. 301 is that run's whole fast tier: 300 names main runs plus
  `lseek_past_eof`.
- **branch, hole arms only / ceiling arm only: 0 wedges in 1 run each.**

**Every arm below the first is n = 1 or n = 2 against a wedge that fired 4 of
5**, so none of them localizes anything by itself — a ~20 % green rate makes
one green run weak evidence everywhere, including the neutered arm that
carries the attribution. What the arms support jointly is only the direction:
the wedge followed the test's filesystem activity, not its name, timing or
the branch's other commits (it reproduced with the early-post revert in and
out — the confusion that mis-blamed fa101ed0's subject is corrected in
af64feab's message).

What is *not* known: the mechanism, and whether the wedge is the fix's or a
latent one the fix unlocks — a write landing past EOF through a preserved
seek position was unrepresentable before it, so this workload has never run
on any earlier tree. The harness keeps only the last 60 capture lines, all
reporter noise in three of the four wedges, so where the first waiter got
stuck is in no capture taken so far. Suspect the write-back path first: a
wedged `iod` parks every later spawn at its binary open, which is the shape
of a whole boot going quiet.
