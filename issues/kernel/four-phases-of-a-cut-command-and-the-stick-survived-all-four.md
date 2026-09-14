---
status: open
kind: finding
opened: 2026-09-14
---

# Four phases of a cut command, and the bench's stick survived all four

The T14 has lost its boot stick to three boots: runs 25 and 26 (`ccorpus`,
sustained multi-megabyte writes to `/log`) and run 33 (`metalcase`, the USB
probe's throughput measurement). Each hung, was ended by the 120 s boot
deadline, and came back with a SanDisk Ultra (0781:5581) that Linux enumerates
and then loops `reset SuperSpeed USB device number 2 using xhci_hcd` on, with no
`sd` device. Only a physical replug clears one: the T14's ports never drop VBUS,
and neither sysfs port-power nor Linux's own device resets reach it.

The reading taken from that was that the reset cut a Bulk-Only Transport command
mid-phase — a device that has taken a CBW and is waiting for its data or its CSW
(BOT 1.0 §5.1, §6.7.2–3) is one a port reset leaves stranded. **Four boots
measured that reading directly and it did not hold.**

## What was measured

Every arm below ran `origin/main`'s reset path — the one that settles TRBs and
not the command they belong to — with a stimulus staged on it, and ended at the
deadline. Every one came back `stick_secs 0`, PASS, EXIT=0.

| run | what the device was left holding | stick |
|---|---|---|
| 39 | a CBW with nothing queued for its data phase, after one 4 KiB write | survived |
| 43 | the same, after 4 MiB of writes | survived |
| 43 | a data TRB on the ring whose doorbell was never rung | survived |
| 43 | the data taken, with nothing reading its CSW | survived |
| 45 | a write **in flight**: the sweep never stopped, so the reset landed on a controller moving bytes | survived |

Run 45 is the sharpest: the kernel's own account on that page reads

```
usb-quiesce: 1 bulk transfer(s) were outstanding and 1 still is after 2000 ms,
so this reset cuts them ... a device cut in its data phase may need a physical
replug before its next host can enumerate it
```

followed by every port reset and power-off — and the stick enumerated on the
next host anyway. The warning that line carries is not, on this platform, true
of a cut data phase.

## What that leaves

**The phase the reset finds the command in is not what loses this stick.** The
three kills share the *hang*, not the reset: what a hang leaves the controller
and the device in — a storm of transfers with no CPU draining events, an
endpoint the driver abandoned and never recovered, a device mid-program with its
firmware in a state no host can name — is the unknown, and none of it is
reachable through the four states a deliberate wedge can stage.

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

## Exit condition

A reproduction of the hang itself — not of a reset — that leaves evidence off
the stick, and an account of what the controller and the device were in when it
happened. `boot.usbload.stick_secs` stands as the regression guard meanwhile: it
is one boot per suite that writes to the bench's own stick continuously and is
reset out from under itself, and it reds if a reset ever does brick the device.
