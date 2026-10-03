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

Ruled out by reading: the identity domain's leaf for `0x9cc00000` is
`SL_READ | SL_WRITE | SL_LARGE`, in tables built by the code the green boots
ran. Each line is written back with `clflush` (`ECAP.C=0`). The context entry
is `TT=00`, `AW=2`, `DID=1`, the same in every arm. `0x01`, not `0x06`, is
what the empty root answers.

Staged on pull request #700, not yet run: `9be0a8ac8` plus a probe that logs
the fatal record raw (`AT`, `PP`, `PRIV`, `EXE`, `T2`), the GPU's PCI `STATUS`
before the actuator, before the hand-over, at `TE` and at the fault, and the
unit's walk for the fault from `RTADDR` down, each entry read from DRAM after
`clflush`. A second image adds 3 ms with `TE` off after the hand-over.

**Exit**: on the T14, the reproduction (the selftests boot at `9be0a8ac8`)
reaches `Boot: complete` with no `DMA FAULT` line, with the hand-over or
programming changed to whatever the probe readings name.
