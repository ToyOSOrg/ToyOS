---
status: open
kind: finding
opened: 2026-08-03
---

# A QMP-driven test cannot share a boot with another one

Measured 2026-08-03: a guest that exits the instant it has its answer left its
last lines — including the runner's `===TEST_END===` — undrained until something
else ran, so on a shared boot the next member opened its console window over
output the previous one was still draining into and read the wrong thing. The
first member passed, the second timed out with its own complete and correct
output visible in the serial, and the third failed instantly on an empty window.

Two workarounds are in the tree, and they are workarounds. `keep_the_ring_moving`
in `tests/toyos.rs:5516` injects keys nothing is listening for, purely so the
ring keeps draining; and the four layout tests take a boot each rather than a
group, which costs three boots.

**The kernel-side mechanism this rested on is closed (2026-08-24), and the
workarounds have not been re-measured against that.** `klogd` is made runnable
at the commit of the record it will drain, so the last `log!` line before a
quiet period reaches the wire without waiting for the next piece of work
(`kernel/src/log/console.rs`). What that does *not* settle is this entry, for
two reasons a re-measurement has to separate: `===TEST_END===` is a userland
`write` to a console rather than a kernel `log!`, which is a different path out;
and nothing has re-run a shared boot with the keys removed. `keep_the_ring_moving`'s
own doc comment still describes the ring as sitting one line behind, and is one
of the two things to fix or confirm.
