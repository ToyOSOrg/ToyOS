---
status: open
kind: defect
opened: 2026-09-14
---

# The bench's wire went dark for six days and nothing says why

The ThinkPad T14's onboard I219 stopped linking on 2026-09-08 and did not
link again until 2026-09-14, through some thirty boots and several full
power cycles, with the cable in place throughout (the owner's statement).
Ubuntu's own driver loaded and named the part on every one of those boots
and never once printed `NIC Link is Up`.

## What is measured

Read off the machine's journal, one `grep -c "NIC Link is Up"` per boot:

| boot, UTC | link-up messages |
|---|---|
| 2026-09-08 16:07, the metal loop's own wire read | the wire answered: `enp0s31f6 at 192.168.1.46`, ping at 57 s |
| 2026-09-08 17:16, metal run 33 | the boot hung; away 283 s; the stick came back dead |
| 2026-09-08 17:21 and every boot to 2026-09-14 09:52 | 0 |
| 2026-09-14 09:57 onward | 1 per boot, 1000 Mb/s full duplex |

The outage's first boot is the one that followed a hang, and that hang is
also the event that left the USB stick unenumerable. That is the whole of
the correlation.

## What is not established

**That ToyOS caused it.** Metal run 49 on 2026-09-14 hung the same way,
away 303 s, and left the stick dead again — and the wire came up on the
very next boot, `NIC Link is Up` at 10:17:23. So a hang does not take the
link with it, and one correlated pair is not a mechanism. Nothing in reach
distinguishes "a boot left the PHY unable to link" from "the link partner
or the contact was out for six days and came back", because no reading was
taken of the wire between boots while it was dark.

## What would tell them apart

The loop already reads the wire before it flashes
(`the claimed function 0000:00:1f.6 is enp0s31f6 at …`). It must also read
it *after* the machine returns and put both in the readback: the carrier
bit, the negotiated speed, and the count of link-up messages in the boot
the machine just took. Then a dark wire is bracketed to one boot and its
image, the way `stick_secs` brackets the stick, and the next occurrence
names its own cause instead of being reconstructed from a journal six days
later.

**Exit condition.** That reading is in every readback, and either an
outage is attributed to one boot — which makes this a defect with a
mechanism and a fix, the hand-back the USB reset already performs — or a
hundred boots pass with the wire read clean on both sides and this file
closes as the bench's own flake.
