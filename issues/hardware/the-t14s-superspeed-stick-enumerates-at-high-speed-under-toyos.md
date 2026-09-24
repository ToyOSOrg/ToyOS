---
status: open
kind: defect
opened: 2026-09-21
---

# The T14's SuperSpeed stick enumerates at high speed under ToyOS

The bench's stick (SanDisk Ultra, 0781:5581) is a SuperSpeed device in a
SuperSpeed receptacle. Ubuntu on the same machine lists it at 5000M
(`lsusb -t`) minutes before a flash. Under ToyOS it is on root-hub port 1, the
USB2 half of that receptacle, at speed 3 (high speed):

- T14 run 74 (2026-09-21): slot 1, port 1, until a bus reset at 3.859 s sent it
  to port 13, where `port 13 connected, link already trained` enumerated it.
- T14 run 77 (2026-09-21, read out by run 78): slot 1, port 1,
  `PORTSC 0x00000e03`, speed 3, although Linux had listed it at 5000M before the
  flash.
- T14 run 75: port 13, slot 5 — the one boot that began with no power cycle of
  the stick since run 74's reset had moved it there.

So which half the stick is on at ToyOS's first look is decided before the
driver acts, by something between the firmware's hand-off and the boot scan,
and a bus reset on the USB2 half is enough to move it. Every disk operation on
port 1 runs at a tenth of the link the hardware has, and a port reset there
(`toyos_xhci::ladder`'s second rung) can move the device to another port and
lose the volume's disk number
(`issues/kernel/a-disk-taken-offline-is-never-brought-back.md`).

Not investigated. What is not known: what the firmware leaves the two ports in,
whether the boot scan's own reset of port 1 is what keeps the stick there, and
whether the USB3 port reads connected at the scan.

## Exit condition

A T14 boot whose kernel log shows the stick enumerated on the USB3 port of its
receptacle at the first scan, or the measurement that says why it cannot be.
