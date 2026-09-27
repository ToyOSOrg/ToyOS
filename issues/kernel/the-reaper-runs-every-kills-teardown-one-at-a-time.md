---
status: open
kind: defect
opened: 2026-09-27
---

# The reaper runs every kill's teardown one at a time

`kernel/src/reaper.rs` is one kernel thread. It takes whichever owed kill has
every thread released, so one victim that is slow to leave the CPU holds up no
other kill; but the teardown it then runs (`process::finish_kill`, the same
`teardown_tail` as exit: handle drops, mapping frees, the zombie marks, the
publish) runs on that one thread. A kill's exit is published only after every
teardown the reaper took before it has finished. Before the reaper, each killer
ran its own victim's teardown and waited on nothing else.

Who waits on a publish: `/system/bin/init`'s cut-over (`old.kill(); old.wait()`),
sshd's session end, and test-runner's deadline kill.

What is measured: with blockd's `--silence-write` holding a write unanswered,
killing the stuck client (or blockd itself) and then an idle process B, B's
kill-to-publish was median 1.04 ms, max 6.4 ms over eight rounds, and in
several rounds B was published before the process killed first. No long
teardown has been measured; the one PR #549's review names is a file's flush
on a USB stick.

## Exit condition

A kill's publish waits on no other process's teardown — teardowns run
concurrently, or on the thread that owes them — with a guest test that holds
one victim's teardown (a file whose flush the device does not answer) and
shows a second victim's exit published within its own teardown's time.
