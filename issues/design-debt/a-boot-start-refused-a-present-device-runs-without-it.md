---
status: open
kind: defect
opened: 2026-09-24
---

# A boot start refused a device the machine has runs without it

A boot start's device refusal is fatal only for a device its `[programs]` row
marks as required; for any other, init logs the refusal loudly and starts the
program without that device (owner, 2026-09-30). No row can mark a device
required, so every boot start refused a device the machine has —
`NotSupported`, `AlreadyExists`, `ResourceExhausted` — runs without it:
`/system/bin/init`'s `start` says the refusal in the kernel's own word
(`init: netd: pci:8086:10c9 is on this machine and could not be handed over`)
and starts the program.

**Exit**: a `[programs]` row can mark a device required, a host test holds the
mark and init's choice between a fatal refusal and a logged one, and a metal
row or, where none can, a guest test shows a boot start refused a required
device is fatal.
