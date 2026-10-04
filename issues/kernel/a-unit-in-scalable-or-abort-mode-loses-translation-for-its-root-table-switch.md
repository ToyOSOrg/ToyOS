---
status: open
kind: defect
opened: 2026-10-04
---

# A unit in scalable or abort mode loses translation for its root-table switch

A unit firmware leaves translating with a root table in scalable or abort-DMA
mode (`RTADDR_REG.TTM` not `00`) and `CAP.ESRTPS` clear may not have its root
table pointer set while translation is on (VT-d Rev. 4.1 §6.6). Handing such a
unit over to this kernel's root table therefore turns translation off for that
one switch, and for that moment every function behind the unit reaches all of
memory. `issues/kernel/a-unit-left-translating-passes-dma-untranslated-while-programmed.md`
removes the gap where the switch may stay translating; on these units it may
not.

Evidence: the specification alone. No machine here has such a unit: every unit of the
T14 reports `CAP` bit 63 (`ESRTPS`), `ECAP` bit 43 (`SMTS`) and bit 52
(`ADMS`) clear.

**Ruled** (owner, 2026-10-04, "Accept and file"): "Allow the one-moment gap on
those machines, log it, and file it as a known weakness with a fix (older
protection registers) as its exit."

Owner: the orchestrator, under `issues/kernel/the-iommu-refuses-nothing-yet.md`.

**Exit**: on such a unit that reports `CAP.PLMR` (bit 5) and `CAP.PHMR`
(bit 6), `PMEN`'s protected memory regions (§11.4.8.1) cover all of memory
before translation goes off for the switch and are released only once it is
back on; a host test reads that order, and is red with the `PMEN` step
removed. It is a host test because no machine reaches the path: QEMU's unit
always reports `ESRTPS` and implements `PMEN_REG` as read-only zero, and the
T14's units report `ESRTPS` clear but support neither mode.

A unit that reports either bit clear has no fix under this exit: §11.4.8.1
treats `PMEN_REG` as read-only there, and the same section plans the
protected memory registers for deprecation, pointing new software at
abort-DMA mode. Such a unit keeps the gap, logged, until a fix that does not
rest on `PMEN` is found.
