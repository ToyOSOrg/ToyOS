---
status: open
kind: tooling
opened: 2026-10-09
---

# No machine has read an Intel NIC across a link change

A link that changes while the transmit ring holds unsent descriptors leaves
them the part's, on the 82574 and on the I219 alike
(`system/i219/driver/src/lib.rs`, "A frame is offered only to a ring with room for
it"): they are counted `descriptors.stranded`, the part is owed their
write-back within `STRANDED_DEADLINE_NANOS` (12.98 s) of the link being read
up, and a part that has not written them back by then is refused by name, so
the `i219` driver ends with "cannot be driven on" and netstack's link with
it. Every reading of that is the driver's own model.

**Both parts' documents say they keep the frame for the link that returns.**
The 82574 datasheet's Defer Count (317694 §10.2.7) and the I219 datasheet's
own (612523 §9.5.4.6) count a transmit deferred because "the link is not up".
Both give the same sixteen attempts the bound is built from: the 82574's
`TCTL.CT`, and the I219's Power Management Control register (PHY address 01,
page 769, register 21, bits 8:1, "Number of retries for a collided packet",
default 0x0F). No machine has read either part do it: no 82574 is in reach,
QEMU's `e1000e` is not in the harness, and no row can drop the T14's link.

**Whether a reset over a live ring hangs the I219 is unknown.** Nothing Intel
publishes that was found says it is safe or that it is not, which is why
neither part is reset to take a ring back. Read, and silent on it: the I219
datasheet (612523 rev 2.02), the 500 Series on-package PCH datasheet volumes
1 and 2 (631119-007 §18, 631120-002 §8), the 100 Series PCH datasheet volume
2 (332691) and specification update (332692-015), the 400 Series PCH
datasheet volume 2 (620855-002) and specification update (620856-009), the
300 Series PCH specification update (337349-012), the 7th generation U/Y
platform I/O specification update (334660-009), the I218 specification
update (rev 1.0), and the 82574 datasheet (317694 rev 3.4) and specification
update. intel.com's search finds no I219 specification update, sighting or
application note, and no specification update for the 500 Series on-package
PCH. The one piece of evidence the tree has is
`issues/the-t14-hung-after-rebooting-with-its-i219-faulted.md`: a T14 that
froze at reboot with that function left holding live rings, cause not
established.

**`open`'s own reset has the same exposure.** Every start of the `i219`
driver resets the function before it programs it. On a driver started again
over a function that still holds published descriptors,
`toyos_i219::quiesce` stops the receiver and the transmitter and the reset
then goes out over that ring. Nothing guards it, because nothing found says
what the guard is.

## What the rule costs

Three weaknesses of leaving the ring to the part, each still true and each
read by the row the exit names:

- **Stale frames leave on the link that returns**, up to
  `toyos_i219::TX_RING - 1` of them and as old as the outage. Or they are
  lost: the 82574 datasheet's §3.2 also has a transmission that "completes
  successfully even if the PHY fails to indicate CRS", as in "a link down
  situation", counted `TNCRS`, and a part that behaves so writes the
  descriptors back with nothing sent. A TCP segment, a DHCP message with a
  dead transaction id and an ARP request are each absorbed by the protocol
  that receives them. The one with a cost is an ARP frame carrying the old
  address onto a different network, which can displace a neighbour's entry
  there until its owner speaks.
- **The deadline bounds collisions and not deference.** 12.98 s is a full
  ring, sixteen attempts a frame, every attempt behind the largest back-off,
  at 10 Mb/s. Nothing bounds how long a station defers to others on a busy
  half-duplex segment, and no document read says how long after `STATUS.LU`
  a part resumes. A part still deferring at the deadline is refused, and that
  ends the machine's wired network.
- **The refusal is safe only while the `i219` row has no `restart`.**
  `[programs.i219]` carries none in any image, so a refused part is next
  reset by a reboot. With a `restart` the refusal becomes an automatic
  `open` over the ring the part kept, which is the reset this issue does not
  know to be safe. The virtio driver's row restarts; the Intel parts' do not.

## Owner

Stage 2 of `issues/the-lan-is-not-yet-production-grade.md`.

## Exit

None can be read yet without a hand on the cable: a link change under
traffic on the T14 is its cable pulled or its switch port cycled, and no row
can do either. The instrument is a link partner a row commands, owed by stage
2 of `issues/the-lan-is-not-yet-production-grade.md`, whose own exit ("link
survives unplug and replug") needs the same one. That row fills the ring,
drops the link and raises it, and reads:

- `descriptors.stranded` above 0 with the link still carrying, and on the
  partner whether the stranded frames arrived (stale frames) or `wire.sent`
  stood still while the descriptors came back (lost by the `TNCRS` reading);
  or the driver's "cannot be driven on" line naming the ring, where the part
  wrote nothing back;
- the same at 10 Mb/s half duplex against a partner that keeps the segment
  busy, for the deadline's false refusal;
- with the `i219` driver started again under traffic and the machine then
  rebooted, whether it comes back: the reset over a live ring. Until that is
  read, the `i219` row takes no `restart`.

An Intel document that states the I219's sequence for a reset over published
descriptors closes the second unknown without a machine: `open` then does
exactly that.
