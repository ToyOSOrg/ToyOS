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
refuse the part the moment a claim on it is granted, and what `IVAR` answers on
an MSI part has never been read: no hand-over of `00:1f.6` has reached the
driver yet, so nothing has run the write.

Owned by the stage-2 I219 worker: the first `nic.accepted(regs::IVAR, …)` on the
bench either passes or names the register that has to be driven differently.
