---
status: open
kind: defect
opened: 2026-09-27
---

# A console holder's queued line can reach the console after the boot's last word

`quiesce_stops_the_machine` was red once, wide, at 618e68e2 (`wt/toyos-logtrack`,
PR #527), in a fast tier run as load beside twenty `log_stream_stalled_reader`
runs, 1-minute load average 28–97; green alone:

```
FAIL quiesce_stops_the_machine: 3 line(s) reached the console after the boot's last word:
  {5.024 tid=2 pid=6 test-runner} quiesce-writer: 1 4
  {5.123 tid=3 pid=6 test-runner} quiesce-writer: 2 4
  {5.292 tid=5 pid=6 test-runner} quiesce-writer: 4 3
```

The lines are stamped before the stop and reached the wire after `Rebooting.`.
The mechanism read off the code: `logd` puts a program's line in
`kernel/src/log/console.rs`'s queue, and `klogd` drains it a chunk of records
and then a chunk of queued lines per hold of the wire, whatever their age. The
stop stops every userland thread and writes its last word as a record, which
`drain_inline` or `klogd`'s next pass puts on the wire; a line still in the
queue then goes after it, inside `quiesce-late-word`'s window. `main` has no
queue — a holder wrote the wire itself — so this is the branch's.

The likely fix is a drain of the queue on the wire between the stop of every
holder and the last word, where nothing can add to it. No deterministic
stimulus exists yet: the red needs the queue non-empty at the last word, which
only a `klogd` slower than `logd` gives.

## Exit condition

No holder's line can follow the last word by construction, and a test that
leaves a line in the queue at the stop goes red without that and green with it.
