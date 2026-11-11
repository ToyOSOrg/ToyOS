---
status: open
kind: defect
severity: [hang]
opened: 2026-09-07
---

# A T14 boot wedges between a job's exit and the next spawn's record, three times in five boots

Run 19 of the metal loop, `metal-suite` c4e1811a, images derived from
`tests/images/testcases.toml`. Three boots never came back inside `metal::return_secs` and
the owner cut power to each:

| image | job list | hangs |
|---|---|---:|
| `testcases` | four jobs | 0 of 1 |
| `testcases-mkdir` | `test_rs_mkdir_cap`, `echo`, `reboot` | 1 of 3 |
| `testcases-readdir` | `test_rs_readdir_bound`, `echo`, `reboot` | 2 of 2 |

**Every one of the three stops at the same two records**, and the passing boot
of the same image writes the next one 46–100 ms later:

```
exit: <job> pid=7 code=0 cpu=<n>ms
ELF: 3741 relocations indexed (RELATIVE + GLOB_DAT + TPOFF)
spawn: TLS 1 modules, total_memsz=144          <- the file ends here
spawn: /system/bin/reboot pid=9 ... dst=4 ...  <- only in a boot that came back
Syncing filesystems... / ... / Rebooting.
```

No `PANIC:`, no `SEGFAULT`, no fault record, no `LOCK CONTENTION`, no
`DEADLOCK`, no `retire_task: task not released`, no `tlb: cpu N has not
flushed`. Every one of those tripwires would have fired inside 40 s and the
machine was down for six minutes. The spawning CPU is `cpu6` in all three.

## The evidence is bounded by logkeeper, and that is the whole difficulty

**The stick's file is what `logkeeper` flushed, not what the kernel emitted.** So a hang anywhere from that spawn through the reset register write
leaves exactly this tail, with everything after it still in the record ring and
never on the stick. The window is **[the reboot binary's spawn → the reset
register write]**, and the candidates in it are all consistent with what the
file shows:

- The spawn's device reads after `spawn: TLS`. These boots ran
  `symbols::read_backtrace_table`, 2 MiB of symbol tables off the boot stick in
  512 SCSI READ(10) commands, each holding the block `Handle` and `XHCI` with
  preemption off; #711 deletes it, because the kernel no longer reads a
  program's symbol tables. The device read left between the two records is the
  build-id note: at most a page per `PT_NOTE`, one note in an lld-linked
  program, through the same block path. A wedge in that path stays a candidate;
  the 2 MiB read that matched the healthy 46–100 ms gap does not.
- `quiesce`'s `flush_disks` and `hand_back`, which landed at `88841f56` a few
  minutes before the first hang; `hand_back` takes `XHCI` while `logkeeper`'s own
  write to the same stick may still be in flight.

Nothing on the stick can separate them, and the black-box page was lost to the
power cut each time — a warm reset preserves it, a cold one does not.

## Two eliminations, both measured rather than argued

**The runner's own deadline reaches `SYS_SHUTDOWN` on this hardware.** Run 20
killed `usbread` at 67.3 s (`exit: usbread pid=8 code=137`) and the deadline
thread carried the boot through `Syncing filesystems...` and `Rebooting.` from
`cpu0 tid=1`; the machine reset and came back. So the shutdown path itself is
not what these boots died in — what is left is what happens when the job list
*completes* and the reboot binary is spawned, or a deadline thread that never
ran.

**The VFS lock is not in the window.** Run 20 also shows `vfs::lock()` held
across a 32 s stick write with other CPUs at 200M spins, and a spawn does take
that lock. But it took it at `loader/mod.rs:370` and in `load_needed_libs` (deleted
since: an executable relocates itself, and nothing loads a `DT_NEEDED`), both
*before* the `ELF: … relocations indexed` and `spawn: TLS … modules` records
that every hung boot wrote; everything after them reads the already-open backing
and takes no VFS lock. So it is not this.

## What now exists to answer it

`kernel/src/deadline.rs` ends such a boot without a hand: a bound armed off
the parameter line and polled from the timer interrupt entry on every CPU,
which writes the reset register itself. What it no longer leaves is a record:
the black-box page that carried the log ring's tail across the reset went with
the memory-overwrite request, and a record surviving a reset is
`issues/a-crash-record-survives-a-reset-outside-ram.md`.

**The two records these boots stopped at are no longer written.** A spawn
writes one record, the `spawn: pid=… <path>` that only the boots that came
back carry, so the next occurrence's landmark is that record's absence after
the job's `exit:`: the window opens at the job's exit and takes in the whole
spawn, the VFS-lock sites this file eliminated for run 19 among them.

**Exit condition**: what the machine was doing between a job's `exit:` record
and the next `spawn:` record, named with evidence — a record that crossed the
reset, or a reproduction with a console — and then whatever that names.
