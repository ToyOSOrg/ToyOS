---
status: open
kind: defect
opened: 2026-10-28
---

# The T14's iGPU faulted inside its RMRR after a hand-over took translation off

One T14 boot under `iommu-firmware-left` (shared-debug, at 7ef0a48c8) halted
1 ms after `Boot: peripherals ready`:

```
iommu: unit0 fault recording overflowed — earlier faults are lost
iommu: DMA FAULT owner=kernel unit0 stream=00:02.0 addr=0x000000009cea7000 access=read reason=0x06 domain=unrecorded bme=cleared unitfaults=1 streamfaults=1 first=y read-permission
```

What the boot establishes:

- `00:02.0` is the iGPU, alone in unit0's scope. No driver runs it, so the
  read is the scanout firmware left running. `rmrr0` reserves
  `0x9c000000..0xa07fffff` for it, and the root bridges' memory starts at
  `0xa0800000`, so the region ends at `TOLUD`. That is where the 11th
  Generation Intel Core datasheet (631122 §2.5) puts GTT and data stolen
  memory. The address lies inside the region.
- Reason 6h is a read that met a second-stage entry with `R` clear (VT-d 5.0
  Table 30, `LGN.3`). The identity domain maps all of `0x0..0x45f800000` read
  and write, so unit0 did not translate by the tables it had been given.
- Fault reporting is armed after `hand_over` and clears every older record,
  and `IF` is set throughout `vtd::init`. So the fault was taken after unit0
  was back on through this kernel's root, after units 1 to 3 were programmed.
- unit0 has one recording register (`nfr=1`), and the overflow shows at least
  one more fault after that.

On the T14 under `iommu-firmware-left`, unit0's translation went on, off and
on again while the scanout read through it. Every other T14 boot only turns
it on. The orchestrator's logs to 2026-10-28 hold 787 boots with one
transition and no iGPU fault, and 46 cycled boots with this one. That
supports translation going off under live DMA as the trigger, at p ≈ 0.055,
but does not establish it.

**The change made**: where firmware sets `DMA_CTRL_PLATFORM_OPT_IN` and this
kernel's root translates every RMRR as firmware must, the hand-over leaves
translation on. The T14's root is then switched under translation (§6.6)
rather than through off and on. Under `iommu-firmware-left` the T14 now makes
the same single transition as every other boot.

**What is not yet known**: whether the off-and-on cycle is what faults. The
`iommu-handover-cycles` actuator repeats it and the switch under translation
hundreds of times each, with fault events held, and prints each fault with
the cycle that preceded it and the walk the tables give from memory. Its
row, `iommu_handover_cycles`, runs only by name.

Owner: the IOMMU track (`issues/the-iommu-refuses-nothing-yet.md`).

**Exit**: `iommu_handover_cycles` on the T14 places the fault. If the
`switch` cycles fault, this change does not cure it, and the defect is
re-scoped to what does. If only the `off-on` cycles fault, the cause is
recorded at `Unit::hand_over`, where translation still goes off for a unit
whose in-flight DMA is undeclared. Close once `iommu_firmware_left` passes on
the T14 with unit0 keeping translation on.
