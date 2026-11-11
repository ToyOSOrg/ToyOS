---
status: open
kind: defect
opened: 2026-11-11
---

# A `-c` shell cannot tell a child its quit asked from one that landed after it

A quit of a `-c` shell asks or ends the children there. A child that lands
after it holds the reason unread, or none if the shell had read it, and that
quit does not ask it (`kernel/proclife/src/quit.rs`). So the shell asks each
child it starts once an interrupt has reached it, right after that child's
spawn (`system/shell/src/quit.rs`, `Asked::landed`).

When the interrupt and the landing both fall within that one spawn, no step
of the shell's tells which came first. A child that listens between its
landing and the shell's look, and has already been asked by the walk or read
the inherited reason, is asked a second time. It is ended if it has not read
the first. `shell-copies`'s model shows exactly those orders. A child that
never listens, such as `cat`, is not affected: it ends in every order.

Exit: the kernel either tells the spawner whether the child landed holding a
reason its parent had not read, so the shell asks only a child nothing asked,
or asks a child that lands under an asked parent itself. Either changes the
quit's rule, which is the kernel's.
