---
status: open
kind: defect
opened: 2026-10-04
---

# A function refused under translation faults read permission after the hand-over

On the T14, when the integrated GPU (`00:02.0`, unit0) has its scanout reads
refused for about 2 ms (0.153 s to 0.155 s) by a unit left translating, and that unit is then
handed over and programmed, the GPU's next reads fault `0x06` (read
permission) through this kernel's identity domain, which grants them, and the
kernel halts on `DMA FAULT owner=kernel`. `vtd::enable` and `Unit::hand_over`
(`kernel/src/arch/x86_64/vtd/mod.rs`) do what VT-d Rev. 4.1 asks before `TE`
goes back on: `SRTP` to a fresh root, then a global context-cache and a global
IOTLB invalidation with both drains, acknowledged (§6.6). A unit reporting
`CAP.CM=0`, as unit0 does, caches no faulted translation (§6.2.4.1), so a
refusal before the hand-over leaves nothing in the unit the spec lets fault
after it. What carries it across, the GPU or the unit, is unknown.

Firmware does not hand the T14 over this way: its four units read
`gsts=0x40000000`. The state is the one the `iommu-firmware-left` actuator left
at `9be0a8ac8` of pull request #700, which pointed every unit at an empty root
while unit0's identity domain was built. Today's actuator translates through
the identity domain and refuses nothing.

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

So the tables are not what refuses, and neither is any cached state VT-d Rev.
4.1 lets the unit keep. The `0x06` needs both a period of refused reads and
`TE` back on within microseconds of the hand-over: three boots of that
actuator with no hold faulted (`9be0a8ac8` alone, with the `FSTS` probe and
with the walk probe), one with a 3 ms hold did not, and `55a5ccb42`,
`TE` off about 2 ms after its refusals, did not.

**Exit**: on the T14, the reproduction (the selftests boot at `9be0a8ac8`)
reaches `Boot: complete` with no `DMA FAULT` line, through a hand-over that
waits on an event the unit or the function reports. A timed hold with `TE`
off does not close this: it is a flat wait no hardware document mandates, it
widens the untranslated window
`issues/a-unit-left-translating-passes-dma-untranslated-while-programmed.md`
records, and it rests on one green boot against three reds.
