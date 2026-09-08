---
status: open
kind: defect
opened: 2026-09-07
---

# A WEDGED report's newest records were cut by the loader's own log file, and nothing on the stick said so

T14 run 21, `deadlinewedge` on `8cc5dbee`. The boot deadline expired at 120153 ms,
sealed a `WEDGED` record, and the next loader pass printed it into `loader.log`
under `Previous boot's panic:`. The record that reached the stick carried 190 log
records spanning 0.148 s to 1.210 s and stopped there — short of `wedge: staged`,
`spawn: /system/bin/logd`, `spawn: /system/bin/test-runner` and
`spawn: /system/bin/reboot`, which are the records the seal exists for.

This was first read as the kernel's snapshot returning a middle window of the
ring. It is not. **The loader's log file stopped, mid-report.**

## What the stick actually says

| | |
|---|---|
| `loader.log` size, from its FAT directory entry (partition offset 553024, `2e 3f 00 00`) | 16,174 bytes |
| its last line | `\| [1.210 cpu6] CPU 6: joining scheduler`, complete and newline-terminated |
| `loaderlog::ENDS_AT_CHAIN` — *"the last boot is accounted for, so this pass resets the machine"* | **absent from the file, and absent from all 35,651,584 bytes of the partition image** |
| the same line on a healthy reporting pass (`toyos-usbreset`, `jobcase`, `DONE`) | present, and the file's last line |

`bootloader/src/main.rs` prints the finding's lines and then calls
`end_this_pass`, whose first statement is that line. It is unconditional. A file
that has the report's first 190 lines and not that line is a file the loader
stopped writing to while it was still printing the report.

## Why that also settles what the kernel sealed

The printed report is `said` (177 bytes) plus 13,389 bytes of records: 13,566.
`Report::tail` composing it had `room = TEXT_BYTES - 177 = 16,167`, and it has
exactly two outcomes — it writes the records whole, or it cuts them to a record
boundary within one record of `room`. 13,566 is 2,601 bytes short of `room` and
is neither. So the page held more than the file shows, and nothing in this run
shows the snapshot dropping its newest records.

The 190 printed record lines are byte-for-byte `kernel.log`'s lines 87..276
(only the em-dash differs, which `bootloader/src/blackbox.rs`'s `Ascii` renders
as three dots by design). `kernel.log` itself is `logd`'s, ends at 2.208 s, and
carries no `wedge: staged` — `logd` was wedged before it could write one.

## The three things that made this unreadable, in the order they cost

1. **`loaderlog::line`'s failure path reports to the console only.** A short
   write or an error takes the sink out and calls `refused`, which prints to the
   firmware console. The T14 has no serial port, so on the one machine this
   mechanism exists for the reason a `loader.log` ends is written nowhere a
   reader can reach. The next pass could carry it; nothing does.
2. **A `WEDGED` or `DONE` report is printed with no byte count.** `State::Panic`
   prints `{PREVIOUS_PANIC} {} bytes off {PHYS:#x}` and the other two print no
   length at all — which is the one number that tells a cut seal from a cut
   file, and it was the number missing here.
3. **The cut itself said nothing.** Fixed at the page:
   `toyos_blackbox::Report::tail` now writes `DROPPED_OPENS_WITH` with the count
   of records it dropped above the ones it kept, paid for out of the records'
   own room. The loader's half and the panel's `Backfill`, which drops the
   oldest into a full `SNAPSHOT_CAP` just as silently, are still unsaid.

## Why the loader stopped is not yet known

Two candidates, and the run does not separate them: a failed or short write to
the FAT file at ~16 KiB, or the 60 s firmware watchdog `main` arms firing while
the pass flushed one FAT write per line for ~200 lines. The wall clock puts the
whole reporting pass at roughly 17 s between the wedge's reset and Ubuntu's ssh
(231 s total, 149 s of it before the reset), which argues against the watchdog
without excluding it.

**Exit condition**: a `deadlinewedge` rerun where the stick carries the
`WEDGED` report's byte count, the drop line with its count, and
`ENDS_AT_CHAIN` — and, if the file still stops, a line on the stick that says
why it stopped.
