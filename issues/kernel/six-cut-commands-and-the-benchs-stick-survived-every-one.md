---
status: open
kind: finding
opened: 2026-09-14
---

# Six cut commands, and the bench's stick survived every one

The T14 has lost its boot stick to three boots: runs 25 and 26 (`ccorpus`,
sustained multi-megabyte writes to `/log`) and run 33 (`metalcase`, the USB
probe's throughput measurement). Each hung, was ended by the 120 s boot
deadline, and came back with a SanDisk Ultra (0781:5581) that Linux enumerates
and then loops `reset SuperSpeed USB device number 2 using xhci_hcd` on, with no
`sd` device. Only a physical replug clears one: the T14's ports never drop VBUS,
and neither sysfs port-power nor Linux's own device resets reach it.

The reading taken from that was that the reset cut a Bulk-Only Transport command
mid-phase — a device that has taken a CBW and is waiting for its data or its CSW
(BOT 1.0 §5.1, §6.7.2–3) is one a port reset leaves stranded. **Six boots
measured that reading directly and it did not hold**, which is why the reset
path records the phase a device was left in and does not try to finish the
command.

## What was measured

Every arm below ran `origin/main`'s reset path with a stimulus staged on it, and
came back `stick_secs 0`, PASS, EXIT=0.

| run | what the device was left holding | what ended the machine | stick |
|---|---|---|---|
| 39 | a CBW with nothing queued for its data phase, after one 4 KiB write | the boot deadline at 120062 ms | survived |
| 43 | the same, after 4 MiB of writes | the boot deadline at 120065 ms | survived |
| 43 | a data TRB on the ring whose doorbell was never rung | the boot deadline at 120066 ms | survived |
| 43 | the data taken, with nothing reading its CSW | the boot deadline at 120065 ms | survived |
| 45 | a write **in flight**: the sweep never stopped, so the reset landed on a controller moving bytes | the hard-lockup detector at 60003 ms, because that sweep held `IF` clear | survived |
| 47 | the same, on this branch's kernel and with the sweep keeping its CPU interruptible | the boot deadline at 120066 ms | survived |

Run 47 is the sharpest, because it is the only one whose account says what the
*controller* was doing rather than what the driver had queued:

```
usb-quiesce: a Bulk-Only command was open in its data phase on slot 5 after
2000 ms, so this reset cuts it
usb-quiesce: the controller had that device's data endpoint Running with 236
TRB(s) it had not reached on the ring
```

Two hundred and thirty-six transfers ahead of the dequeue pointer on a Running
endpoint is not a bus that had gone quiet: the reset landed on a controller
mid-stream and a device programming flash, drove every port reset and took the
power away — and the stick enumerated 0 s after the machine came back. Run 45
said the same thing in the base kernel's own words, `1 bulk transfer(s) were
outstanding and 1 still is after 2000 ms, so this reset cuts them ... a device
cut in its data phase may need a physical replug before its next host can
enumerate it`. That warning is not, on this platform, true of a cut data
phase.

## What that leaves

**Neither the phase the reset finds the command in nor how busy the bus is when
it lands is what loses this stick.** The
three kills share the *hang*, not the reset: what a hang leaves the controller
and the device in — a storm of transfers with no CPU draining events, an
endpoint the driver abandoned and never recovered, a device mid-program with its
firmware in a state no host can name — is the unknown, and none of it is
reachable through the five states above.

**And it is unknowable through the stick**, because the stick is the channel and
the stick is what dies. Evidence about a hang of this kind has to leave the
machine some other way; that is the network track's question, not the USB
driver's.

## What is not claimed

That cutting a command is harmless in general. BOT §5.3.4 makes reset recovery
the device's obligation and a port reset clears strictly more than it asks for;
a device that does not honour it is one no software on a laptop without VBUS
control can clear. This bench's device honours it in all five states above. A
different device, or this one in a state no wedge reaches, may not.

## The present state, in the run's own words

Run 47 is the standing guard doing its job for the first time: one boot per
suite writes to the bench's own stick continuously, is reset out from under
itself, and is judged on whether the device comes back. `power::usb_load_chain`
PASSed on it, `boot.usbload.stick_secs` read 0 against a ceiling of 5, and the
whole account was on the page — which is the other thing that boot established,
because no wedge page this bench had taken before carried one.

## Exit condition

A reproduction of the hang itself — not of a reset — that leaves evidence off
the stick, and an account of what the controller and the device were in when it
happened. `boot.usbload.stick_secs` stands as the regression guard meanwhile, and it reds
if a reset ever does brick the device.
