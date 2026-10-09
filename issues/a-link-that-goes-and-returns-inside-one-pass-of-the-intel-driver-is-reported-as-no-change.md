---
status: open
kind: defect
opened: 2026-10-09
---

# A link that goes and returns inside one pass of the Intel driver is reported as no change

`toyos-i219`'s `begin_pass` (`toyos-i219/src/lib.rs`) reads the link only on
a pass that acknowledged `LSC`, and answers `link_changed: self.link !=
before`: the state after the pass compared with the state before it. `LSC` is
one bit, so a link that went down and came back at the same speed and duplex
before one pass acknowledged it is read as the link it was, and the pass
answers no change. `Nic::begin_pass` (`userland/netstack/src/i219.rs`) hands
netstack a link only where that is true, so `main`'s `link_up` stays true and
no pass tells the name's responder it had no link.

RFC 6762 §8 asks for probing and announcing whenever a responder "receives an indication of a network interface "Link Change"
event". The part gave that indication, the `LSC` cause, and the driver held
it and dropped it: the name is neither probed for nor announced on that
link, which may be another network's.

## What is known and what is not

Read from the source, and not measured. Not known: whether a real link can
go and return between two passes. A pass begins on the `LSC`
interrupt, so it needs netstack kept off the CPU for a whole renegotiation.
No machine has been read across a link change at all
(`issues/no-machine-has-read-an-intel-nic-across-a-link-change.md`).

## Owner

The driver's `Pass`, under stage 2 of
`issues/the-lan-is-not-yet-production-grade.md`.

## Exit

A host test in `toyos-i219` in which a pass that acknowledged `LSC` with
`STATUS` as it was before reports the change, and netstack's name has no
link on that pass.
