---
status: open
kind: defect
opened: 2026-09-16
---

# A stick that answers late is broken by the two-second abandon

The first two T14 records of the mass-storage transport breaking open the same way:
the device took a READ(10)'s CBW, delivered its data, and had not produced the
13-byte CSW two seconds later.

- T14 run 24, the C corpus boot's kernel log, where the third attempt
  completed:

  ```
  [28.833 cpu2] usb-storage: 00:14.0 slot 5 transport broke on SCSI 0x28: no answer in the status phase in 2000 ms
  [28.833 cpu2] xHCI: 00:14.0 slot 5 endpoint 3 is Running, recovering
  [28.833 cpu2] xHCI: 00:14.0 slot 5 endpoint 4 is Running, recovering
  [28.840 cpu4] usb-storage: 00:14.0 slot 5 transport broke on SCSI 0x2a: command phase completion code 4
  [28.844 cpu4] usb-storage: 00:14.0 slot 5 transport broke on SCSI 0x2a: status phase completion code 4
  [28.845 cpu4] usb-storage: 00:14.0 slot 5 SCSI 0x2a completed on attempt 3
  ```

- T14 run 55, the black box the next boot's loader printed. The page dropped
  its 227 oldest records, the first break among them, and its tail reads:

  ```
  [3.471 cpu4] exit: logd pid=4 code=-1 cpu=2143ms
  [3.471 cpu5] usb-storage: 00:14.0 slot 5 transport broke on SCSI 0x28: status phase completion code 3
  [3.478 cpu5] usb-storage: 00:14.0 slot 5 transport broke on SCSI 0x28: command phase completion code 4
  ```

  and so on, the two alternating, 22 breaks in the 85 ms to 3.556 s, until

  ```
  [5.556 cpu0] xHCI: Set TR Dequeue timed out
  [5.556 cpu0] usb-storage: 00:14.0 slot 5 reset recovery failed; disk is offline
  ```

  `cpu=2143ms` on a process 3.47 s into the boot is read here as one 2 s wait
  spent inside a disk transfer: an inference, since the record of that wait
  was dropped.

- T14 run 84, read by run 85, a scout image carrying this driver's ladder. The
  boot's first READ(10), 0.6 s in and on the only CPU yet running, went to a
  stick whose TEST UNIT READY, INQUIRY, READ CAPACITY and serial string had each
  just completed in step, tag checked:

  ```
  [2.602 cpu0] usb-storage: 00:14.0 slot 5 transport broke on SCSI 0x28: no answer in the command phase in 2000 ms; break 1 of 3 running
  [2.602 cpu0] usb-storage: 00:14.0 slot 5 transport broke on the class reset's TEST UNIT READY: status phase completion code 3 (Babble Detected); break 2 of 3 running
  [2.757 cpu0] usb-storage: 00:14.0 slot 5 the port reset took: addressed and configured again, the device answered TEST UNIT READY under its own tag 0xe
  [4.758 cpu0] usb-storage: 00:14.0 slot 5 transport broke on SCSI 0x28: no answer in the data phase in 2000 ms; break 3 of 3 running
  ```

  The device went offline behind a warm reset, and neither the firmware nor
  Linux could use it until it was replugged (`lsusb` listed it, no disk). Read
  here, and only an inference: the stick's own protocol answered every reset
  while its media did not — the command that needed the media was the one
  that stopped, before and after a warm port reset that took.

A CSW that takes longer than two seconds is legal USB: a device may withhold
its handshake for as long as it likes, and a consumer flash stick doing
housekeeping after a 114 MB `dd` does. The driver cannot wait longer —
`USB_TIMEOUT_NS` is `block::OPERATION`, the longest stretch a CPU may hold
pinned with preemption off, and no disk wait in this kernel can park
(`kernel/CLAUDE.md`). So it abandons the transfer, stops the endpoint, and
issues Reset Recovery to a device that was not broken, only slow.

**What follows is an inference; nothing in either record reads the device's
state.** A Babble Detected Error on a 13-byte status read is a device sending
more than 13 bytes where its host asked for a CSW, and a device still holding
the data or the CSW of a command its host has abandoned would do that. BOT §3.1
has the class reset ready the device for the next CBW; whether this stick's
does is not measured, and neither is why the two codes alternate.

The recovery is now the class's own sequence and the give-up takes the device
offline, so a slow stick costs the boot its disk rather than a storm and a
wedge. It still costs the disk.

## What would fix it

A Bulk-Only command that outlives one block operation. The CSW's TRB is on the
ring and the device will answer it; the operation refuses on its budget as it
does today (`Scsi::Budget`, `BlockError::BudgetExpired`), and the caller's own
retry (`file_backing`'s, which asks again on `BudgetExpired` alone) finds the
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
