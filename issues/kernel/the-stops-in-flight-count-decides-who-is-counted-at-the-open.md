---
status: open
kind: defect
opened: 2026-09-18
---

# The stop's `in_flight` count decides who is counted at an operation's open, and a process can become a holder before its close

`kernel/src/block.rs`'s `counted()` reads `log::user::holds_the_log` once, when
`begin_operation` opens, and `OpenOperation` carries the answer to its drop so
the two ends of one operation cannot disagree. A process becomes a holder at
its first `SYS_LOG_READ` (`kernel/src/arch/syscall/machine.rs`'s
`note_log_holder` call), and that is per process, not per thread. So a thread
that opened an operation while its process held nothing stays counted after a
sibling thread's first read makes the process a holder: the first stage then
leaves that thread running, and a record whose sweep stopped everything it
named can still read `in_flight` of one.

`quiesce_stops_the_machine` reds on `in_flight != 0`, so the consequence is a
false red on a correct kernel, never a false green.

## The bound

A multi-threaded process holding `Rights::LOG` is not hypothetical:
`test-runner` on `tests/testcases` is one — a main thread and its `deadline`
thread (`userland/test-runner/src/main.rs`), reading the log inside its
`log-gate` and `log-close` builtins. What no committed config arranges is the
coincidence: that process's *first* read landing while a sibling thread is
inside a block-device operation, in the span between the first stage's last
sweep and its read of `block::userland_operations()`. Every later read changes
nothing, because the holder is already recorded. Unmeasured: nothing stages
it, and nothing has shown it.

## Exit condition

Either the count is taken per thread the stage actually stops — decided at the
stop, from the sweep's own verdict on the thread, rather than at the open from
a table that can change under it — or holder status is fixed before a process
can open an operation (at the endowment `init` makes, which is an ABI question
and lands on its own pull request). The entry closes when a record's
`in_flight` cannot count a thread its own sweep left running.

Owner: whoever next changes `block::counted` or the carve-out's definition;
`issues/kernel/nothing-bounds-the-log-writer-below-the-boots-last-word.md` is
the same carve-out's other open end.
