---
status: open
kind: defect
opened: 2026-09-30
---

# A bind whose device left aims the ladder at its empty port

Derived from the code, not staged.

`kernel/src/drivers/xhci/wait/msc.rs` has two exits for a device that left its
port. `scsi()` answers a round trip that broke as `Broke::Gone` itself: it
sends no recovery and leaves the disk to its port's teardown. `bring_up` does
not. Its TEST UNIT READY hands every break to `climb_until_in_step`, a `Gone`
one included, with `Left::Elsewhere`. The climb enters at the class reset, whose
steps are aimed at a port that reads empty. Only once that rung has ended does
the climb read the port and find the device gone.

The class rung's bound is spent on a device that is not on the bus, and a
device that left during its bind is handed to the teardown by another path
than one that left during a command. `main` at 3c24b6edb has the same shape:
its bind climbs a `Gone` break too, and the port rung's reset is where it
first reads the port empty.

**Exit**: the port is read once, at the climb's entry as well as after each
rung, so a break whose device left ends there before any rung. `scsi()`'s
`Broke::Gone` arm then goes, and a device that left is one exit.
