---
status: open
kind: finding
opened: 2026-09-26
---

# `blocking_read_window` completed 26 of 500 round trips beside other guests

Fast tier at `1e5ef5c1` (PR #524's branch; the host carried the branch's own
twelve guest slots and another worktree's suite at the same time):
`blocking_read_stress: only 26 of 500 round trips completed inside 3s — a wake
was not delivered`. The harness's re-run alone was green in 2 s
(`at least 193 held windows a post landed in (0 -> 256)`). `cargo run --
--known-red blocking_read_window` answers NO.

What the red run's own log says against its sentence: the two processes spent
`cpu=1639ms` and `cpu=1998ms` of the 3.5 s they ran (`syscall_wall=3535ms` and
`3599ms`), and cpu0 took 522 interrupts, 456 of them xHCI — a guest that was
running and slow, not one parked on a wake that never came. So the verdict's
cause is unread: the test waits host seconds and names a lost wake when they
run out, which a starved guest satisfies as well as a lost wake does.

Not done: an A/B against `main` under the same load, which is what would say
whether this branch moved it; nothing on the branch weakens an ordering the
wake path uses.

**Exit**: the verdict tells a lost wake from a slow guest (the round trips'
progress over the window, not only the count at its end), and a cause for
this run.
