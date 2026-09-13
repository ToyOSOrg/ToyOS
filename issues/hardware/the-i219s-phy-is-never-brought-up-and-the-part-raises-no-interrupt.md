---
status: open
kind: defect
opened: 2026-09-13
---

# The I219's PHY is never brought up, and on the T14 the part raised no interrupt at all

Metal run 36 is the first boot that handed the ThinkPad T14's `00:1f.6` to a
process. The kernel half worked: the BAR moved to `0xae200000`, the function was
handed over on slot 0 with vector `0x28` on MSI, netd mapped the register window
and took a 2 MiB DMA grant. Then nothing. Over the twenty seconds `lan_hold`
held the boot open, the end-of-boot census read `userdev=0` on every one of the
eight CPUs (`t14-run36/lancase-green.log`).

**`I219::open` returned `Ok`.** `netd`'s `Nic::undrivable` panics, a panic is an
exit, and no `exit: netd` record appears on that boot — netd was still alive
when the runner rebooted the machine. So every `accepted()` check passed, `IMS`
was written with `RXT0 | RXO | RXDMT0 | LSC | RXQ0 | OTHER`, and the driver went
on to wait for a link-status change that never came.

## What `toyos-i219` does not do

The crate is written from the *Intel 82574 GbE Controller Family Datasheet*
(317694-018), and its module header says so. The 82574 is a discrete PCIe
controller with its PHY on the same die. The T14's `8086:15fc` is not one: on
that part the MAC is in the PCH and the PHY is separate silicon the Management
Engine also drives, reached over an internal interface rather than over the
register file this crate knows.

`grep -n 'MDIC\|PHY\|EXTCNF\|swflag' toyos-i219/src/` finds two doc comments and
no code. The driver's whole bring-up of the link is:

```
let held = regs.read(regs::CTRL);
regs.write(regs::CTRL, held | ctrl::RST);         // wait for self-clear
...
regs.write(regs::CTRL, wanted | ctrl::SLU);
```

On this part that is a MAC reset with no PHY handling on either side of it:
nothing acquires the semaphore the ME shares the PHY through, nothing resets or
re-configures the PHY after the MAC reset, and nothing takes it out of power
down. `CTRL.SLU` lets the MAC *see* a link signal; it does not make the PHY
produce one. A PHY left unconfigured by a reset that did not restore it never
brings the link up, `LSC` never fires, and `userdev=0` is exactly what that
looks like from outside.

## The other reading, and it is not excluded

**No claimed function on the T14 has ever delivered a message.** MSI through
this machine's interrupt remapping unit is unproven: the arming is recorded
(`iommu: irte5 source=00:1f.6 ... vector=0x28 apic=0x0 dst=0x0 trigger=edge`,
`PCI 00:1f.6: msi address=0xfee000b8 data=0x00000000`), and nothing has ever
come back through it. A silent part and an undelivered message are the same
`userdev=0`.

`kernel/src/pcidev` now records the arrival of a claimed function's first
message (`pcidev: slot N took its first message on vector 0xNN`), so the two are
distinguishable the moment either one happens — but on a boot where neither
does, they still are not.

## The experiment that separates them, and it costs one boot

`ICS` (Interrupt Cause Set, `0x000C8`) is write-only and raises the causes
written to it. It is defined in `toyos-i219/src/regs.rs` and no code writes it.
A driver that writes one enabled cause to `ICS` at the end of `open` and reports
whether a message followed answers the question outright:

- a message arrives — delivery works, and the link is the defect;
- no message arrives — the part is speaking to nobody, and the defect is in the
  arming, the remapping entry, or the delivery path.

Either answer is worth a boot of the machine, and it reaches no address whose
routing is unknown.

Owned by the stage-2 I219 worker. The datasheet for the PCH part is the source;
`e1000e`'s `ich8lan` handling is the shape of the answer and is not in this tree.
