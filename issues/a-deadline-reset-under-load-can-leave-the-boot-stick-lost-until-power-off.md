---
status: open
kind: defect
opened: 2026-10-28
---

# A deadline reset under load can leave the boot stick lost until power-off

`stick_survives_a_reset_under_load` resets the machine with the boot
deadline while `stick_load` writes `/log` through usbd. No USB register is
touched before the reset register (#901 §6). On the AMD bench (xHCI 03:00.4,
a SanDisk 0781:55a9 at super speed on port 5) the stick failed to come back
after one such reset:
- the firmware reported "ToyOS boot failed";
- the port stayed dead through the host's own reboot;
- only a full AC power-off cleared it.

The bench's ports, like the T14's, do not drop VBUS across a warm reset.

**Counts so far, AMD bench**: 1 failure in 3 runs. #1089 at `31db18c19`
failed, then passed (131 s, 7239 KiB of load lines reached the stick before
the reset). Main at `5870531b2`, #1089's base, passed (107 s, 7237 KiB). It
is intermittent and **not shown to be #1089's**: one failure, with a pass on
the same image, does not separate that change (type-3 SystemIO idle handed
over) from the row's first runs on this bench. The T14 passes the row 5 of 7
historically.

**What the failed boot left** (https://github.com/ToyOSOrg/evidence/tree/5b888041ad0d4e4dec2d586259114880db681ba5/r3):
- **Nothing ToyOS wrote was wrong.** The recovered log partition holds
  `stick_load` lines 0 to 7806: every one 900 bytes, payload intact, no gap.
  No line from usbd, the xHCI or the kernel reports an error.
- **The reset itself came on time.** The log ends at 45.701 s, and the
  deadline was armed at 5.741 s for 40000 ms.
- So what was lost is the device's state across a reset that landed
  mid-write, the shape `issues/a-cut-bulk-only-command-does-not-brick-the-benchs-stick.md`
  found on the T14 with a hang before it, and here with none.
- The stick is the channel and the stick is what died. The reset's own
  moment, the phase the command was cut in and what the controller held, is
  unrecorded.

**Exit**: either the cause found, with the state the device was cut in
recorded off the stick, or enough AMD runs of the row on main to bound the
rate and decide whether the deadline path owes a port reset or power cycle
before the reset register.
