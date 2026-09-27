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

## Exit condition

No holder's line can follow the last word by construction, and a test that
leaves a line in the queue at the stop goes red without that and green with it.

## The stop now drains the queue

On `nightly-green2` the stop drains the queue on the wire right after
`quiesce::stop()`, before `Syncing filesystems...`
(`log::console::drain_for_the_stop`). `console-queue-at-the-stop` is the
deterministic stimulus: it queues one line after every holder is stopped and
keeps `klogd` off the queue from the stop's claim on.
`quiesce_stops_the_machine` arms it and judges the line above the last word.
- The drain disabled as a checked patch: `cargo test --test toyos-build --
  --nightly quiesce_stops_the_machine` EXIT=1, `1 line(s) reached the
  console after the boot's last word: console: a holder's line, queued once
  the stop had stopped every holder`, wide and alone. The power-off's
  `serial::flush_final` wrote it.
- With the drain: EXIT=0.

What is left of "by construction" is a stop that did not stop every holder,
which it reports at alert level. A holder that still runs after the drain
can queue a line that follows the last word.
