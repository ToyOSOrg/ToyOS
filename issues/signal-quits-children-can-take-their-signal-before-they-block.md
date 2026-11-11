---
status: open
kind: defect
severity: [hang]
opened: 2026-11-11
---

# `signal_quit`'s children can take their signal before they block

`libc_children`'s `signal_quit` ended `stalled driving the child that
reloads` once, in 1 of 3 local QEMU runs at `dd9bf974d` (#1111's head), on
the 2-CPU x86-64 boot. Its log is in
[`pr-1111/r2/tty`](https://github.com/ToyOSOrg/evidence/tree/8b3306f1ba244133a2c28e8d7c09fcc48c473c8e/pr-1111/r2/tty).

It is not `tty_interrupt`'s stall, which was a shell reading ahead on a pipe
it shares with the program it starts: `signal_quit` runs no shell, and each
child has pipes of its own.

**Read from the code, not reproduced.** The `reloads` child says `armed` and
then calls `pause`; the parent sends `SIGHUP` as soon as it has read
`armed`. libc runs a raised signal at once: in a blocking call of a thread
that leaves it unblocked, or else on libc's own thread
(`sdk/libc/src/sigreq.rs`). A `SIGHUP` raised before the child has entered
`pause` runs its handler on libc's own thread, and the `pause` that follows
waits for a handler that has already run. The child never says `reloaded`.
The `reads` child has the same window between `armed` and its `read`, and
`reloads` a second one between testing `ended` and `pause`.

Unmeasured: whether this is what the one sighting was. Nothing kept from it
says where the child was.

**Exit**: each of the two children blocks without a window a signal can fall
in, or the parent sends only once the child is in its call, as
`libc_read_restarts` orders its own by the roster; a run that holds the
child short of its call for a control shows the stall before and not after.
