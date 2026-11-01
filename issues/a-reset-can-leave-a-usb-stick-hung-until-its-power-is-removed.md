---
status: none
kind: rejected
opened: 2026-10-28
---

# A reset can leave a USB stick hung until its power is removed

**Declined by the owner on 1 November 2026:** whether ToyOS's resets lose a
USB stick more often than Linux's restart does is not worth bench time. Since
#1095 no stick is in a bench boot's path, so a lost stick no longer strands a
bench. The ToyOS-side guard, a reset under a write to a stick a bench row
drives, stays owed by
`issues/a-cut-bulk-only-command-does-not-brick-the-benchs-stick.md` and
`issues/no-bench-row-drives-usb-storage-since-the-benches-boot-off-nvme.md`.

## The losses

On 28 October the AMD bench's boot stick (SanDisk 3.2Gen1, 0781:55a9, behind
xHCI 03:00.4, whose HCCPARAMS1 reads PPC=0) was lost twice, in two shapes:

- **11:48 UTC, a ToyOS deadline reset under load.** `stick_survives_a_reset_under_load`
  on #1089's image at `31db18c19`: the boot deadline reset the machine while
  `stick_load` wrote `/log`. No USB register is touched before that reset
  (#901 §6). Ubuntu then logged `usb 4-1: device not accepting address 2,
  error -62`, so the device attached and would not take an address. The
  firmware reported "ToyOS boot failed", the port stayed dead through Ubuntu's
  own reboot, and only removing AC power brought the stick back.
  - **Nothing ToyOS wrote was wrong.** The recovered log partition holds
    `stick_load` lines 0 to 7806, every one 900 bytes, payload intact, no gap.
    No line from usbd, the xHCI or the kernel reports an error.
  - **The reset came on time.** The log ends at 45.701 s; the deadline was
    armed at 5.741 s for 40000 ms.
  - The moment of the reset, the phase the command was cut in and what the
    controller held are unrecorded: the stick was the channel, and it died.
  - Evidence: https://github.com/ToyOSOrg/evidence/tree/5b888041ad0d4e4dec2d586259114880db681ba5/r3.
    The recovered image itself is not published, as it carries the machine's
    identifiers.
- **12:39:45 UTC, an Ubuntu restart into the firmware's setup.** No ToyOS boot
  was involved in it. From then until
  the owner replugged the stick at 13:04:55, the controller read the stick's
  ports `Powered, Not-connected, RxDetect` on both its USB3 and its USB2 side
  at every boot: no attach on either side. The replug brought the stick back
  at once. This reading is as the orchestrator reported it; no log of it
  is linked here.

The T14 lost its own boot stick (a SanDisk Ultra, 0781:5581) four times, in a
third shape: it enumerates, and Linux loops `reset SuperSpeed USB device` with
no `sd` device (`issues/a-cut-bulk-only-command-does-not-brick-the-benchs-stick.md`).

The firmware option "Always On USB" was enabled on both benches; the owner has
since disabled it.

## The counts

| reset | bench, stick | lost / cycles |
|---|---|---|
| ToyOS deadline reset under `stick_load` (`usbload`) | AMD, 0781:55a9 | 1 / 6 (21 Oct: 3 passes; 28 Oct: the 11:48 loss, then 2 passes, one of them main at `5870531b2`, #1089's base) |
| the same row | T14, 0781:5581 | 0 confirmed / 7 (21 Oct, #971's rounds: r6, r8, r14 and r15 passed; r3 and r4 failed on their logs with the stick back, r3's load having written memory and not the stick; r5's boot exited 1 with no readback and no recorded reason) |
| every other ToyOS metal boot, each one Ubuntu restart, a ToyOS reset and the loader's warm reset | AMD, 0781:55a9 | 0 / about 230 (the orchestrator's AMD boot records, counted, not audited) |
| an Ubuntu restart with no ToyOS boot | AMD, 0781:55a9 | 1 / unknown (the owner's restarts are not counted) |
| hung ToyOS boots ended by the deadline (runs 25, 26, 33, 84) | T14, 0781:5581 | 4 losses |

These counts do not separate ToyOS's reset paths from Linux's, and #1089 is
not shown to be the cause of 11:48: one loss, with a pass on the same image.
Both AMD losses fell on one afternoon, after weeks with none.
