---
status: open
kind: defect
opened: 2026-10-03
---

# A released function still names its slot's domain

`pcidev::release` (`kernel/src/pcidev/mod.rs`, `tear_down`) clears Bus Master
Enable and resets the function, and leaves its context entry naming the slot's
domain (`kernel/src/arch/x86_64/vtd/table.rs`, `bind`, is reached only from a
claim). The slot's next holder is attached to that same domain
(`slot_space`), so the released function's requests translate through the
next holder's grants for as long as it is that holder's. Nothing but the
released function's own obedience to `BME` stands between them: a function
that masters the bus with `BME` clear reaches another process's memory.

**Exit**: a released function's context entry names no domain once it is
quiet, read back off the unit's table on the T14 after its slot's next claim.
