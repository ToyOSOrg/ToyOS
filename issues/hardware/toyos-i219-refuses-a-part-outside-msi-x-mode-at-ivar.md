---
status: open
kind: defect
opened: 2026-09-08
---

# `toyos-i219` refuses a part outside MSI-X mode at `IVAR`, and the T14's I219 is one

`toyos-i219`'s `open` writes §10.2.4.9's `IVAR` and reads it back, refusing a
part that does not take the write:

```
nic.regs.write(regs::IVAR, ivar::ALL_ON_VECTOR_ZERO);
nic.accepted(regs::IVAR, ivar::ALL_ON_VECTOR_ZERO)?;
```

§10.2.4.9 defines that register only "in MSI-X mode" and says nothing about what
a part outside it answers, so the read-back is a guess refused rather than a
guess driven on.

The T14's `00:1f.6` is outside that mode: Linux's own reading of the same
function is `IR-PCI-MSI-0000:00:1f.6 … enp0s31f6` in `/proc/interrupts` and
`mode=msi` at `/sys/bus/pci/devices/0000:00:1f.6/msi_irqs/162`. So the driver may
refuse the part the moment a claim on it is granted.

**It did not refuse, and not refusing proved nothing.** Metal run 36 is the
first boot on which a claim on `00:1f.6` reached the driver: `I219::open`
returned `Ok` — netd's `Nic::undrivable` panics, a panic is an exit, and that
boot carries no `exit: netd` record — so the read-back accepted the write.

`accepted` asserts `read & wrote == wrote`, which a register reading all-ones
satisfies and so does any reserved location that simply takes a write. The part
is outside MSI-X mode by Linux's own reading of it, §10.2.4.9 defines the
register only inside that mode, and what `0x000E4` *is* on this part is still
unknown. The write is still a guess; it is now a guess that costs nothing
observable rather than one that refuses a hand-over.

That boot produced no interrupt at all
(`issues/hardware/the-i219s-phy-is-never-brought-up-and-the-part-raises-no-interrupt.md`),
so whether this register matters here is unmeasured either way.

Owned by the stage-2 I219 worker: name what `0x000E4` is on this part from its
own datasheet, and either write it for a reason or stop writing it.
