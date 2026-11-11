---
status: open
kind: defect
severity: [hang]
opened: 2026-11-11
---

# `signal_quit`'s children can take their signal before they block

`libc_children`'s `signal_quit` sometimes ends `stalled driving the child
that reloads`, by its own 120 s ceiling, on the 2-CPU QEMU x86-64 boot.
Seen twice:

| when | where | revision |
|---|---|---|
| by 2026-11-01 | local QEMU, 1 of 3 runs, [`pr-1111/r2/tty`](https://github.com/ToyOSOrg/evidence/tree/8b3306f1ba244133a2c28e8d7c09fcc48c473c8e/pr-1111/r2/tty) | `dd9bf974d` (#1111's head) |
| 2026-11-11 | the guest runner, run 42931710932, host quiet | `9ad2edd7d` (#1209's branch and a control in the shell) |

It is not `tty_interrupt`'s stall, which was a shell reading ahead on a pipe
it shares with the program it starts: `signal_quit` runs no shell, and each
child has pipes of its own.

**The cause.** The `reloads` child says `armed` and then calls `pause`; the
parent sends `SIGHUP` as soon as it has read `armed`. libc runs a raised
signal at once: in a blocking call of a thread that leaves it unblocked, or
else on libc's own thread (`sdk/libc/src/sigreq.rs`). A `SIGHUP` raised
before the child has entered `pause` runs its handler on libc's own thread,
and the `pause` that follows waits for a handler that has already run. The
child never says `reloaded`.

A control holds the child 200 ms short of its `pause` after `armed`, on main
`9349705ce`: the row stalls the same way, run 42937578767. Its patch and log
are in
[`pr-1209/r1`](https://github.com/ToyOSOrg/evidence/tree/9eaaac0994e1468a9135af6ce22188462f8567ac/r1).

Read from the code and not measured: the `reads` child has the same window
between `armed` and its `read`, and `reloads` a second one between testing
`ended` and `pause`.

A C program cannot close the window for `pause` or `read`: only the kernel's
roster says a thread is parked, which is why `libc_read_restarts` is a Rust
program.

**Exit**: no child of `signal_quit` blocks in a call a signal can fall
short of. Each either holds its signal blocked until a call that unblocks it
(`sigsuspend`, `ppoll`), or its claim moves to a program that can order the
signal after the block; the control above then passes.
