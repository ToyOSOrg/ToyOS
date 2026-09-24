---
status: open
kind: defect
opened: 2026-09-21
---

# `logd` ends the boot's log on one refused create, and nothing durable says so

T14 run 69 (`usb-transport-break` on the boot stick, this tree at `a9adf0d7`)
ran its job, synced, said `Rebooting.` at 5.808 s and read back `DONE`. Its
sealed record says

```
log: /log holds this boot to 0 ms and 302 record(s) committed after that reached no volume
```

and the stick carries `loader.log` and `attempts` and **no kernel log file at
all**. The disk was online to the end: the same record carries `usb-storage:
disk 0 does not implement SYNCHRONIZE CACHE (sense 0x05/0x20/0x00)` at 5.808 s,
which is a whole Bulk-Only round trip and a REQUEST SENSE answered by the stick
at the shutdown — and on an ordinary boot of this machine that line is written
at `logd`'s first `fsync` (run 64: 1.900 s), so in run 69 no `fsync` of `logd`'s
reached the device before the shutdown's own.

## What is known

- **The staged break lands inside `logd`.** The arm abandons the boot's first
  WRITE(10). Under QEMU (`tests/jobcase` on `Profile::Metal` with the arm, the
  boot `power::transport_break_chain` takes) one such boot has `spawn:
  /system/bin/logd` at 0.301 s, the break at 0.323 s and `logd: this boot's
  kernel log is …` after it: the write is `Volume::open` creating the file.
- **One refused create is no log for the boot.** `userland/logd/src/store.rs`'s
  `Volume::open` answers `None` on any error from `create`, with no retry, and
  `main` then runs to the end of the boot with `volume = None`. `policy::fate`
  does not see this path at all; it rules on appends and flushes, where every
  answer but a budget-refused flush is `GiveUp` for the rest of the boot.
- **What it says reaches nothing on this machine.** `logd: cannot create …` and
  `logd: no /log on this machine …` go to `logd`'s console, which on a laptop
  with no serial port is no channel, and they are not kernel records, so the
  black box's tail does not carry them either.
- Under QEMU the same boot's recovery takes in about a millisecond, the create
  succeeds and `/log` is complete, so no QEMU boot shows any of this.

## What is not known

Which error the create got in run 69. The disk never went offline, so it was
not a transport that gave up. Two candidates fit and neither is measured: the
re-issued WRITE(10) answered with CSW status 1 (`Scsi::Refused`, a device error
to every caller — a device that has just taken a class reset may owe a UNIT
ATTENTION), or the call ran out of `block::OPERATION` or of the driver's budget
after a break (`Scsi::Budget`, `WouldBlock`). Either leaves a `usb-storage:`
record, and the black box's recovery section now carries those across the reset
whether or not the volume got anything.

## Exit condition

A boot whose first device write is refused once and whose `/log` still holds
that boot's records — or, where `logd` does give a volume up, a record of why
on a channel that survives the machine, which its console is not.
