---
status: open
kind: defect
opened: 2026-10-04
---

# The T14's iGPU faults read permission after translation goes off and on under its scanout

On the T14, when unit0, the integrated GPU's (`00:02.0`), has translation
turned off for microseconds and back on while the GPU's scanout reads through
it, the GPU's
reads sometimes fault `0x06` (read permission) through this kernel's identity
domain, which grants them. The kernel then halts on `DMA FAULT owner=kernel`.
Before `TE` goes back on, the hand-over does what VT-d Rev. 4.1 asks: `SRTP`
to a fresh root, then a global context-cache and a global IOTLB invalidation
with both drains, acknowledged (§6.6). A unit reporting `CAP.CM=0`, as unit0
does, caches no faulted translation (§6.2.4.1). What carries the fault across,
the GPU or the unit, is unknown.

Firmware does not hand the T14 over this way: its four units read
`gsts=0x40000000`. The state below is the one the `iommu-firmware-left`
actuator left at `9be0a8ac8` of pull request #700, which pointed every unit at
an empty root while unit0's identity domain was built.

Evidence, pull request #700, T14 boots:

- `9be0a8ac8` plus the `FSTS` probe: unit0's one record, before `fault::arm`
  clears it, is `00:02.0` read `0x9cb80000` reason `0x01` with `FSTS=0x3`
  (overflowed), from the empty root. Once `TE` is confirmed, `FSTS=0` and the
  record is clear. Next, `unit0 fault recording overflowed`, then
  `00:02.0` read `0x9cc39000` reason `0x06`. Every address is inside
  `rmrr0` (`0x9c000000..0xa07fffff`, scope `00:02.0`).
- `55a5ccb42`, the same empty-root actuator but handed over before the build,
  so the refusal and the hand-over are both logged at 0.153 s and `TE` stayed
  off until 0.155 s: no fault.
- `25ab31bd5` and `5e8cbb06e`, no refusal, and the hand-over and `translating`
  logged in the same millisecond: no fault.

- `9be0a8ac8` plus a walk probe: unit0's record before `fault::arm` is again
  `00:02.0` read `0x9ccc0000` reason `0x01` with `FSTS=0x3`. At the fault,
  `00:02.0` read `0x9cdb7000` reason `0x06`, raw high word
  `0xc000000600000010`: `AT=0`, `PP=0`, `EXE=0`, `PRIV=0`, `T2=0`, an
  untranslated read with no PASID. The unit's walk, from `RTADDR` `0xa18000`,
  each entry read from DRAM after `clflush`: root `0xa19001`, context
  `0xa04001:0x102` (`P=1`, `TT=00`, `AW=2`, `DID=1`), then `0xa05003`,
  `0xa08003` and the 2 MiB leaf `0x9cc00083`, present with read and write over
  `0x9cdb7000`. The GPU's PCI `STATUS` reads `0x0010`, no master or target
  abort, before the actuator, before the hand-over, at `TE` and at the fault.
  The unit refused a read the tables in memory grant.
- the walk probe plus 3 ms with `TE` off after each hand-over: the same
  empty-root refusals (`FSTS=0x3`, record `0x01`) before `fault::arm`, then no
  `DMA FAULT` and `Boot: complete`.

Pull request #1086, T14:

- One shared-debug boot (7ef0a48c8, today's actuator, which refuses nothing)
  halted on `00:02.0` read `0x9cea7000` reason `0x06`. It was the one fault in
  46 such boots, against 0 in 787 boots whose unit0 only turns translation on.
- `iommu-handover-cycles` (measurement branch `igpu-fault-measure`, e3b1562e7,
  never landed) moved every unit between two identical identity roots, three
  boots, with fault events held. 200 root switches under translation ran
  first, then 200 cycles of translation off, root, queued invalidation and
  translation on. Totals:
  - **switch: 0 of 600 cycles faulted;**
  - **off-on: 22 of 600 cycles faulted**, with 94 records.
- Every off-on record is `00:02.0`, a read, `0x06`, at an address inside the
  firmware framebuffer in data stolen memory (`BDSM=0x9c800000`,
  `BGSM=0x9c000000`, `TOLUD=0xa0800000`). The first comes 546 to 598 µs after
  translation is back on.
- Each printed walk from memory reads a present root and context entry and a
  read-write 2 MiB leaf.

So no period of refused reads is needed. Translation going off for
microseconds and back on under live scanout is enough, in about one cycle in
27, and the unit then refuses reads its tables grant. Turning translation on
for the first time since firmware is not: the 787 boots did only that. The 3 ms hold's one green boot is no evidence at that rate.
Neither the tables nor any cached state VT-d Rev. 4.1 lets the unit keep is
what refuses. Intel's 11th Generation Core specification update (631123-034,
errata TGL001 to TGL075) documents no erratum for VT-d or the iGPU's DMA.

Every hand-over still takes translation off, now inside protected memory
regions, so `iommu-firmware-left` still reaches this on the T14. Real T14
firmware leaves translation off, so no other boot does. How often a boot of
today's sequence halts is not measured. The estimate is between the cycles'
one in 27, a different sequence with no regions, and the pre-#1086 boots' 1
in 46. The row that arms the actuator rides a boot of its own, so a halt costs
only that boot.

**Exit**: this kernel's hand-over sequence is fixed so that the measurement's
off-on cycles fault no more than its switch cycles, or a documented erratum
for it is cited and its workaround applied. Either is judged on the T14 by
the measurement and by `iommu_firmware_left`'s row.
