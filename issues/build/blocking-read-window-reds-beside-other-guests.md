---
status: open
kind: finding
opened: 2026-09-26
---

# `blocking_read_window` reds beside other guests, naming a lost wake a slow guest also satisfies

The verdict, in the fast tier: `blocking_read_stress: only N of 500 round trips
completed inside 3s — a wake was not delivered`. The harness's re-run alone is
green.

Sightings, all on 2026-09-26:

- **26 of 500** at `1e5ef5c1` (PR #524's branch; the host carried the
  branch's own twelve guest slots and another worktree's suite at the same
  time). The re-run alone was green in 2 s (`at least 193 held windows a post
  landed in (0 -> 256)`).
- **21 of 500** at `62ef89a4` (PR #525's branch, other worktrees' guests
  running). The process's own line gave `syscall_wall=3087ms` and
  `cpu=1703ms` for pid 7, and cpu0 took 169 xHCI interrupts in the window. In
  the same session the fast tier ran six times, three on the branch and three
  on `main` at `d65446cc`, interleaved: this test was red once on the branch
  and never on `main`, while `main`'s third run had five reds of its own that
  the branch never showed. The branch's kernel differs from `main` in the
  claim DMA paths, which this test does not reach, and in one boot log line.
- **On `main` too, at the same rate.** A same-session A/B, runs of `main`
  (`d65446cc`) and of PR #524's branch started together so both arms carried
  one load (six suites at once): `main` 16 of 17 green, the branch 17 of 18,
  and each arm's one red is this sentence (`only 26 of 500` on `main`, `only 27
  of 500` on the branch; the branch's red spent `cpu=1568ms` and `cpu=1532ms`
  of its window). The rate is the same on both arms, so no branch moved it.

**What the red runs' own logs say against the sentence.** In the 26-of-500
run the two processes spent `cpu=1639ms` and `cpu=1998ms` of the 3.5 s they ran
(`syscall_wall=3535ms` and `3599ms`), and cpu0 took 522 interrupts, 456 of them
xHCI — a guest that was running and slow, not one parked on a wake that never
came. So the verdict's cause is unread: the test waits host seconds and names a
lost wake when they run out, which a starved guest satisfies as well as a lost
wake does.

**Exit**: the verdict tells a lost wake from a slow guest (the round trips'
progress over the window, not only the count at its end), and a cause for these
runs — a lost wake, or a 3 s budget a starved host cannot meet, and if it is
the budget, the bound derived rather than measured.
