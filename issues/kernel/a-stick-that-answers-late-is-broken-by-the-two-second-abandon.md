---
status: open
kind: defect
opened: 2026-09-16
---

# A stick that answers late is broken by the two-second abandon

Both T14 records of the mass-storage transport breaking open the same way:
the device took a READ(10)'s CBW, delivered its data, and had not produced the
13-byte CSW two seconds later.

- Run 24 (`/Users/jan/.claude/jobs/2280e09e/tmp/t14-run24/ccorpus.log:2191`):
  `transport broke on SCSI 0x28: no answer in the status phase in 2000 ms`,
  both endpoints Running. The next command's CBW got a USB Transaction Error,
  its data phase a STALL, its status phase another Transaction Error, and the
  third attempt completed.
- Run 55 (`/Users/jan/.claude/jobs/2280e09e/tmp/t14-run56/lancase-repeat.log`,
  the black box at lines 63 on): `exit: logd pid=4 … cpu=2143ms` beside a
  boot 3.47 s old, which is one 2 s wait spent inside a disk transfer; the
  227 records the page dropped hold the first break, and the tail shows what
  followed it — a status phase answered with a data packet (Babble Detected),
  a CBW answered with no handshake (USB Transaction Error), alternating for
  ninety milliseconds until the controller stopped answering commands.

A CSW that takes longer than two seconds is legal USB: a device may withhold
its handshake for as long as it likes, and a consumer flash stick doing
housekeeping after a 114 MB `dd` does. The driver cannot wait longer —
`USB_TIMEOUT_NS` is `block::OPERATION`, the longest stretch a CPU may hold
pinned with preemption off, and no disk wait in this kernel can park
(`kernel/CLAUDE.md`). So it abandons the transfer, stops the endpoint, and
issues Reset Recovery to a device that was not broken, only slow — and a device
that does not abort its pending CSW on a Bulk-Only Mass Storage Reset (BOT §3.1
says it shall; this SanDisk Ultra evidently does not always) is then one phase
ahead of the host for the rest of the boot: the stale CSW arrives as the next
command's data, and the next command's data arrives where the CSW was asked
for, which is the babble.

The recovery is now the class's own sequence and the give-up takes the device
offline, so a slow stick costs the boot its disk rather than a storm and a
wedge. It still costs the disk.

## What would fix it

A Bulk-Only command that outlives one block operation. The CSW's TRB is on the
ring and the device will answer it; the operation refuses on its budget as it
does today (`Scsi::Budget`, `BlockError::BudgetExpired`), and the caller's own
retry — ten milliseconds to two seconds later, under a 120 s deadman — finds the
command still open on that device and resumes waiting on the same TRB instead
of issuing a new CBW into a device holding half a command. Nothing is abandoned
until the deadman, and Reset Recovery runs only for a break the device
reported. `stop::OpenCommand` already publishes where the round trip stands;
what is missing is the driver keeping it across `with_disk` and `dispatch_event`
recording a mass-storage completion nobody is waiting on.

## Exit condition

A T14 boot in which a CSW arrives more than two seconds after its CBW and the
command completes on the caller's retry with no `transport broke` record, or a
QEMU boot with `usb-slow-device` widened past the operation budget doing the
same.
