---
status: open
kind: defect
opened: 2026-10-28
---

# A reset can leave a USB stick hung until its power is removed

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

## What host and device owe across a reset

- A host controller reset terminates every transaction in progress. It drives
  a Hot or Warm Reset on USB3 root ports and no reset on USB2 ones. Its effect
  is "similar to a Chip Hardware Reset" (xHCI 1.2 §5.4.1, HCRST). After either
  reset the controller powers every root port (§4.19.4).
- **With PPC=0 no xHCI driver removes VBUS.** Each port is hard-wired to
  power and PP does not affect it (§5.4.8). The AMD bench's controller reads
  PPC=0, and the T14's ports have never dropped VBUS either.
- A device keeps its receiver termination at all times except in
  eSS.Disabled (USB 3.2 Revision 1.1 §7.4.1). Warm Reset exists to recover a
  device whose link state machine has hung, and an upstream port detects it
  in every link state except eSS.Disabled (§6.9.3, §7.4.2).
- A peripheral's upstream port counts its entries to eSS.Disabled.Default in
  `tDisabledCount`, which only invalid VBUS or a successful port configuration
  exchange resets; at a count of 3 it moves to eSS.Disabled.Error, which a USB
  2.0 bus reset does not leave and only a PowerOn reset does (§7.5.1.2.2-4).
- A peripheral whose SuperSpeed port has powered off with VBUS still present
  connects on its USB 2.0 interface (§10.18.1).

**Only the 12:39 shape is read here.** A device presenting neither SuperSpeed
termination nor a USB 2.0 attach is outside its specified behaviour, since
§10.18.1 has it connect on USB 2.0; and it is one no USB signal reaches,
because there is nothing attached to warm-reset or bus-reset. No xHCI or USB
action after the reset can recover it. Whether a platform transition removes
VBUS here (S5, now that "Always On USB" is disabled, or a cold reset) is
unmeasured. The 11:48 and the T14 shapes attached, so this reading does not
carry over to them. The reset just before the 12:39 loss was Ubuntu's.

## The counts so far

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

## The reproduction

It runs on the T14 and writes its scratch stick, never a boot stick. Cycles of
four kinds, each followed by Ubuntu's check of whether the scratch stick
enumerates and reads:

- `scratch_written_through_a_deadline_reset`: the boot deadline resets the
  machine while `scratch_load` writes the scratch partition through usbd. No
  USB register is touched first.
- `scratch_written_through_a_reboot`: `scratch_load reboot` asks for the
  orderly reboot after 64 MiB, and usbd resets every connected port and its
  controller in the stop (`toyos_xhci::hc::Hc::stop`).
- An Ubuntu restart while `dd` writes the scratch partition, and an idle
  Ubuntu restart: the controls.

**What a loss is attributed to.** Each ToyOS cycle carries the Ubuntu restart
into it, the ToyOS reset, and the loader's warm `ResetSystem` after the pass
that reads the black box (`loader/src/main.rs`, `end_this_pass`); no Linux
arm has that last reset. A boot log showing `scratch_load` writing clears the
Ubuntu restart, so a loss is attributed to the ToyOS reset or the loader's
warm reset after it. Only the deadline arm against the reboot arm isolates
the kernel's reset path, since both carry the same restart before and the
same loader reset after.

**What one session yields.** A lost stick needs a hand on it, so a session
ends at its first loss and yields at most one. Zero losses in n cycles of an
arm bounds its rate at about 3/n (95%): ten cycles bound it at about 26%,
which cannot tell 1/6 from zero. Telling the AMD's observed 1/6 from a rate
of 1/30 takes about 90 loss-free cycles of an arm; comparing two arms' rates
takes several losses in each, and so attended sessions.

The T14's scratch stick is inferred, from a sector count equal to the boot
stick's, to be of the boot stick's model, a SanDisk Ultra, until the request's
first step records it. Either way it is not the AMD bench's 0781:55a9: a
difference between arms there says which reset path a SanDisk stick is
sensitive to, not the AMD stick's rate.

## Exit

Enough cycles of each arm, in attended sessions, to bound the loss rate of
ToyOS's two reset paths against Ubuntu's restart on the same stick. Then
either:

- the reset path that loses sticks more often changes what precedes the reset
  and is measured again; or
- the difference is shown absent, and this is recorded at
  `stick_survives_a_reset_under_load`.
