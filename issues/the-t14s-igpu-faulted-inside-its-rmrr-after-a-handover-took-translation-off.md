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
  read is the scanout firmware left running.
- `rmrr0` reserves `0x9c000000..0xa07fffff` for it. The root bridges' memory
  begins at `0xa0800000`, where the 11th Generation Intel Core datasheet
  (631122 §2.5) puts the top of graphics stolen memory. The address lies
  inside the region.
- Reason 6h is a read that met a second-stage entry with `R` clear (VT-d 5.0
  Table 30, `LGN.3`). The identity domain maps all of `0x0..0x45f800000` read
  and write, so unit0 did not translate by the tables it had been given.
- Fault reporting is armed after `hand_over` and clears every older record,
  and `IF` is set throughout `vtd::init`. So the fault was taken after unit0
  was back on through this kernel's root.
- unit0 has one recording register (`nfr=1`), and the overflow shows at least
  one more fault after it.

Under `iommu-firmware-left` on the T14, unit0's translation went on, off and
on again while the scanout read through it. Every other T14 boot only turns
it on. The orchestrator's logs to 2026-10-28 hold 787 boots with one
transition and no iGPU fault, and 46 cycled boots with this one (p ≈ 0.055).
That suggests translation going off under live DMA. It does not establish it.

Since pull request #1086, a unit whose firmware reports
`DMA_CTRL_PLATFORM_OPT_IN`, whose RMRRs the identity domain holds, and whose
root is in legacy mode keeps translation on. The hand-over switches its root
under translation (§6.6). Under the actuator the T14 now makes a single
transition. On a machine whose firmware reports nothing, translation still
goes off first.

**Measurement**: the `iommu-handover-cycles` actuator, on a measurement
branch and never landed, moves every unit between its root and an identical
copy. It runs 200 switches under translation first, so no cycle in that phase
follows translation going off, then 200 cycles of translation off and on.
Fault events are held throughout, and each record is counted against the
cycle it followed. Its `off-on` omits the queue and interrupt-remapping
toggles of the original hand-over, so an `off-on` phase without faults does
not clear that sequence. Results go to the pull request's evidence.

Owner: the IOMMU track (`issues/the-iommu-refuses-nothing-yet.md`).

**Exit**, after the measurement boots:

- If the `switch` phase faults, keeping translation on does not cure this.
  The defect is re-scoped to what does.
- If only the `off-on` phase faults, turning translation off under live DMA
  faults on this unit. This file stays open, for the path that still does
  that on machines whose firmware reports nothing, until that path no longer
  takes translation off.
- If neither phase faults, the measurement is inconclusive. The cause stays
  unknown and this file stays open.

It closes when no remaining hand-over path is known to fault.
