---
status: open
kind: tooling
opened: 2026-10-09
---

# No machine has read an Intel NIC across a link change, and two things about the I219 there are unknown

A link that changes while the transmit ring holds unsent descriptors is
answered per part (`toyos-i219/src/lib.rs`, "A frame is offered only to a ring
with room for it"), and every reading of either answer is the driver's own
model.

**The 82574** is reset and brought up again, by its own document: §10.2.6.1
("Software should combine this with a reset if the packets in the FIFO need to
be flushed") and §10.2.6.7. The same document's Defer Count (§10.2.7) says the
part would otherwise have kept those frames for the link that returns. No
82574 is in reach, and QEMU's `e1000e` is not in the harness.

**The I219 is not reset over a ring it holds descriptors in**, because two
things are unknown of it:

- **Whether it writes those descriptors back when the link returns.** The
  82574's Defer Count is the only sentence on the subject in any document
  read, and it is the other part's. The driver gives the I219 the slowest
  time that document lets a full ring leave (`STRANDED_DEADLINE_NANOS`,
  12.98 s) from the link being read up, and then refuses it by name: netstack
  ends with "cannot be driven on".
- **Whether a reset over a live ring hangs it.** Nothing Intel publishes that
  was found says it is safe or that it is not. Read, and silent on it: the
  I219 datasheet (612523 rev 2.02), the 500 Series on-package PCH datasheet
  volumes 1 and 2 (631119-007 §18, 631120-002 §8), the 100 Series PCH
  datasheet volume 2 (332691) and specification update (332692-015), the 400
  Series PCH datasheet volume 2 (620855-002) and specification update
  (620856-009), the 300 Series PCH specification update (337349-012), the 7th
  generation U/Y platform I/O specification update (334660-009), the I218
  specification update (rev 1.0), and the 82574 datasheet (317694 rev 3.4) and
  specification update. intel.com's search finds no I219 specification
  update, sighting or application note, and no specification update for the
  500 Series on-package PCH. The one piece of evidence the tree has is
  `issues/the-t14-hung-after-rebooting-with-its-i219-faulted.md`: a T14 that
  froze at reboot with that function left holding live rings, cause not
  established.

**`open`'s own reset has the same exposure.** Every start of netstack resets
the function before it programs it. On a netstack started again over a
function that still holds published descriptors, `toyos_i219::quiesce` stops
the receiver and the transmitter and the reset then goes out over that ring.
Nothing guards it, because nothing found says what the guard is.

## Owner

Stage 2 of `issues/the-lan-is-not-yet-production-grade.md`.

## Exit

None can be read yet without a hand on the cable: a link change under
traffic on the T14 is its cable pulled or its switch port cycled, and no row
can do either. It becomes a row's when the T14's link partner is something a
row commands. That row fills the ring, drops the link and raises it, and
reads `descriptors.stranded` above 0 with netstack still serving (the part
writes them back) or netstack's "cannot be driven on" line naming the ring
(it does not, and the driver's answer is then a document or an owner's ruling
away). The reset over a live ring is read by a row that starts netstack again
under traffic and reboots: the machine comes back, or it does not.

An Intel document that states the I219's sequence for a reset over published
descriptors closes the second unknown without a machine: the driver then
does exactly that, in `open` as well.
