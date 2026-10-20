---
status: owner
kind: question
opened: 2026-10-20
---

# A function that truncates its DMA addresses faults in its domain

Every domain's addresses start a quarter of the way up the units' translated
width (`kernel/src/iommu/window.rs`), which on the AMD bench is
`0x400000000000`. The bench's HDMI audio function `03:00.1` (`1002:1637`),
reachable since P1 of `issues/there-is-no-wifi.md` hands BARs over in place,
does not reach that high: given its CORB and RIRB at `0x400010000000` it
read and wrote `0x10000000`, `0x10000400` and `0x10000410`, the same offsets
with the bits above its real width dropped. It advertises 64-bit addressing
(GCAP `0x4001`, 64OK, HDA 1.0a §3.3.2), the `hda` driver writes both halves
of each ring base, and `03:00.6` (`1022:15e3`) at the same domain base works
in the same boot. The unit refuses every one of those accesses and the claim
is refused with it, so nothing is corrupted; the function plays nothing, and
`amdvi_translates_the_stick_controller`, which refuses any DMA fault, is red
on the AMD bench for this reason alone.

Evidence: the AMD `testcases` boot at `33adc6b47`,
https://github.com/ToyOSOrg/evidence/blob/b0148546c1c8b77c0fca941fdee729c35349ad1a/p1/bench/round6/amd/testcases/kernel.log
(lines 337 to 345).

Owner: the domain layout, which #916 settled. The orchestrator put the
choice to the owner, recommending that each domain start at the lowest gap
above RAM that clears every declared window rather than a quarter of the way
up, with no per-device quirk.

Exit: the owner's choice implemented, and `amdvi_translates_the_stick_controller`
green on the AMD bench with `03:00.1` claimed.
