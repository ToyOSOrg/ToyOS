---
status: assigned
kind: defect
opened: 2026-09-13
---

# The I219's PHY is never brought up

The ThinkPad T14's `8086:15fc` at `00:1f.6` is handed to netd and `I219::open`
returns `Ok`, but no link ever comes up: nothing in `toyos-i219` touches the
PHY.

## What `toyos-i219` does not do

The crate is written from the *Intel 82574 GbE Controller Family Datasheet*
(317694-018), and its module header says so. The 82574 is a discrete PCIe
controller with its PHY on the same die. The T14's `8086:15fc` is not one: on
that part the MAC is in the PCH and the PHY is separate silicon the Management
Engine also drives, reached over an internal interface rather than over the
register file this crate knows.

The driver's whole bring-up of the link is a MAC reset (`CTRL.RST`, waited on
until it self-clears) followed by `CTRL.SLU`. On this part that is a MAC reset
with no PHY handling on either side of it: nothing acquires the semaphore the
ME shares the PHY through, nothing resets or re-configures the PHY after the
MAC reset, and nothing takes it out of power down. `CTRL.SLU` lets the MAC
*see* a link signal; it does not make the PHY produce one.

## What the hardware has shown

Metal run 36 held a boot open for twenty seconds with the function claimed and
`IMS` armed including `LSC`, and the end-of-boot census read `userdev=0` on
every CPU: the part raised nothing on its own.

Metal run 57 separated that from a delivery defect. netd wrote `LSC` to `ICS`
once after arming the mask, and the kernel recorded
`pcidev: slot 0 took its first message on vector 0x28`. MSI from this function
reaches a CPU on the T14, so the silence of run 36 is the part having no cause
to raise — which is what a link that never comes up looks like from outside.

## Exit

Closes when a T14 boot records the I219's link coming up. The bring-up is
PR #453's (`i219-phy`); the PCH part's datasheet is the source.
