---
status: open
kind: defect
opened: 2026-09-07
---

# A T14 boot wedges between a job's exit and the next spawn's record, three times in five boots

Run 19 of the metal loop, `metal-suite` c4e1811a, images derived from
`tests/testcases`. Three boots never came back inside `metal::return_secs` and
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

## The evidence is bounded by logd, and that is the whole difficulty

**The stick's file is what `logd` flushed, not what the kernel emitted.** In a
passing boot the records from the reboot binary's spawn through `Rebooting.`
reach the file only because `quiesce` calls `log::wait_for_durable()` before the
reset. So a hang anywhere from that spawn through the reset register write
leaves exactly this tail, with everything after it still in the record ring and
never on the stick. The window is **[the reboot binary's spawn → the reset
register write]**, and the candidates in it are all consistent with what the
file shows:

- `symbols::read_backtrace_table` reads 2 MiB of symbol tables off the boot
  stick — 512 SCSI READ(10) commands, each holding the block `Handle` and
  `XHCI` with preemption off. It is the only device call between the two
  records and the only thing in the window that takes the 46–100 ms the healthy
  gap is.
- `quiesce`'s `flush_disks` and `hand_back`, which landed at `88841f56` a few
  minutes before the first hang; `hand_back` takes `XHCI` while `logd`'s own
  write to the same stick may still be in flight
  (`issues/kernel/quiesce-runs-while-userland-still-does-io.md`).
- `log::wait_for_durable` itself.

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
across a 32 s stick write with other CPUs at 200M spins
(`issues/kernel/the-vfs-lock-is-held-across-a-usb-write-for-thirty-seconds.md`),
which is within 2x of `Lock::lock`'s deadlock panic, and a spawn does take that
lock. But it takes it at `loader/mod.rs:370` and in `load_needed_libs`, both
*before* the `ELF: … relocations indexed` and `spawn: TLS … modules` records
that every hung boot wrote; everything after them reads the already-open backing
and takes no VFS lock. So it is a real hazard one step from a panic and it is
not this.

## What now exists to answer it

`kernel/src/deadline.rs`: a bound armed off the parameter line and polled from
the timer interrupt entry in both rings on every CPU, which on expiry seals a
`WEDGED` record carrying **the tail of the log ring** into the black box and
writes the reset register itself. Every metal image carries it
(`toyos_tco::WEDGE_BOUND_MS`, 120 s). The next occurrence therefore ends itself
without a hand and leaves the records `logd` never wrote, which is the one
channel that crosses a reset without `logd`.

**Exit condition**: a `WEDGED` record off the stick naming what the machine was
doing after `spawn: TLS 1 modules`, and then whatever that names.

**The mechanism works and the instrument is not yet sharp enough.** T14 run 21
proved the deadline: a boot wedged on purpose ended itself at 120153 ms against
its 120000 ms bound, sealed a `WEDGED` record, and the machine was back in 231 s
with no hand. But the sealed tail carried 190 of that boot's 295 records and
**dropped the newest ten** —
`issues/diagnostics/the-panels-snapshot-returned-a-middle-window-of-the-ring.md`
— and the newest ten are what this file is waiting for. That is the next thing
to fix, before the next hung boot spends its one seal on the wrong window.
