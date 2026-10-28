---
status: open
kind: defect
opened: 2026-10-28
---

# A reset can leave a USB stick hung until its power is removed

On 28 October the AMD bench's boot stick (SanDisk 3.2Gen1, 0781:55a9, behind
xHCI 03:00.4, whose HCCPARAMS1 reads PPC=0) was lost twice:

- **11:48 UTC, a ToyOS deadline reset under load.** `stick_survives_a_reset_under_load`
  on #1089's image: the boot deadline reset the machine while `stick_load`
  wrote `/log`. Ubuntu then logged `usb 4-1: device not accepting address 2,
  error -62`, the firmware reported "ToyOS boot failed", and only removing AC
  power brought the stick back. The recovered log holds 7,807 whole load lines
  and no usbd, xHCI or kernel error.
- **12:39:45 UTC, an Ubuntu restart into the firmware's setup.** No ToyOS boot
  was involved. From then until the owner replugged the stick at 13:04:55, the
  controller read the stick's ports `Powered, Not-connected, RxDetect` on both
  its USB3 and its USB2 side at every boot. The replug brought the stick back
  at once.

The firmware option "Always On USB" was enabled on both benches; the owner has
since disabled it.

## What host and device owe across a reset

- A host controller reset terminates every transaction in progress. It drives
  a Hot or Warm Reset on USB3 root ports and no reset on USB2 ones. Its effect
  is "similar to a Chip Hardware Reset" (xHCI 1.2 §5.4.1, HCRST). After either
  reset the controller powers every root port (§4.19.4).
- **With PPC=0 no host software removes VBUS.** Each port is hard-wired to
  power and PP does not affect it (§5.4.8). The AMD bench's controller reads
  PPC=0, and the T14's ports have never dropped VBUS either
  (`issues/a-cut-bulk-only-command-does-not-brick-the-benchs-stick.md`).
- A device keeps its receiver termination at all times except in
  eSS.Disabled (USB 3.2 Revision 1.1 §7.4.1). Warm Reset exists to recover a
  device whose link state machine has hung, and an upstream port detects it
  in every link state except eSS.Disabled (§6.9.3, §7.4.2).
- A peripheral's upstream port that enters eSS.Disabled three times without
  VBUS going invalid moves to eSS.Disabled.Error. A USB 2.0 bus reset does not
  take it out of that state. Only a PowerOn reset does, which means VBUS
  (§7.5.1.2).

So the 12:39 reading, with no SuperSpeed termination and no USB2 attach, is a
device that no host signal reaches. Neither warm reset nor bus reset applies
while it presents neither. On these benches only taking its power away clears
it, by replug or by AC. **No action ToyOS takes after such a reset can recover
the stick. Only what precedes the reset can matter, if anything does.** The
12:39 loss had no ToyOS reset before it.

## The counts so far

| reset | bench, stick | lost / cycles |
|---|---|---|
| ToyOS deadline reset under `stick_load` (`usbload`) | AMD, 0781:55a9 | 1 / 6 (21 Oct: 3 passes; 28 Oct: a loss, then 2 passes) |
| every other ToyOS metal boot, each one Ubuntu restart, a ToyOS reset and the loader's reset | AMD, 0781:55a9 | 0 / about 230 orchestrator-recorded boots before 28 Oct |
| an Ubuntu restart with no ToyOS boot | AMD, 0781:55a9 | 1 / unknown (the owner's restarts are not counted) |
| hung ToyOS boots ended by the deadline (runs 25, 26, 33, 84) | T14, 0781:5581 | 4 losses (`issues/a-cut-bulk-only-command-does-not-brick-the-benchs-stick.md`) |

These counts do not separate ToyOS's reset paths from Linux's. One loss in
six under load and one with no ToyOS involved are both small samples. Both
AMD losses fell on one afternoon, after weeks with none.

## The reproduction

It runs on the T14 and writes its scratch stick, never a boot stick. A hung
boot stick strands a bench at the firmware's prompt. Cycles of four kinds,
each followed by Ubuntu's check of whether the scratch stick enumerates:

- `scratch_written_through_a_deadline_reset`: the boot deadline resets the
  machine while `scratch_load` writes the scratch partition through usbd. No
  USB register is touched first.
- `scratch_written_through_a_reboot`: `scratch_load reboot` asks for the
  orderly reboot after 64 MiB and goes on writing. usbd then resets every
  connected port and its controller (`toyos_xhci::hc::Hc::stop`).
- An Ubuntu restart while `dd` writes the scratch partition, and an idle
  Ubuntu restart: the controls.

Each ToyOS cycle also carries the Ubuntu restart into it and the loader's own
reset after it. A loss in one is attributed to the ToyOS reset only if that
boot's log shows `scratch_load` writing.

The T14's scratch stick is a SanDisk Ultra of the T14's own boot stick's model,
not the AMD bench's 0781:55a9. A difference between arms there says which
reset path a SanDisk stick is sensitive to. It does not measure the AMD stick.

## Exit

Enough cycles of each arm to bound the loss rate of ToyOS's two reset paths
against Ubuntu's restart on the same stick. Then either:

- the reset path that loses sticks more often changes what precedes the reset
  and is measured again; or
- the difference is shown absent, and this is recorded at
  `stick_survives_a_reset_under_load`.
