---
status: assigned
kind: defect
opened: 2026-10-04
---

# A domain's device addresses overlap the root bridges' memory windows

`vtd::table::Domain` hands out device addresses from `1 << (translatable - 2)`
up to `1 << translatable`, and checks only that the floor is above RAM
(`pmm::top()`). Nothing holds the range against the windows firmware declared
the root bridges decode (`KernelArgs::root_bridge_windows`).

On the T14 `MGAW` is 39, so every domain logs `addresses from 0x2000000000 to
0x8000000000`, and firmware declares `mem 0x4000000000..0x603dc00000` and `mem
0x603dc00000..0x8000000000`; the iGPU's `bar2` sits at `0x4000000000`. The
upper half of every domain's range is an address a bridge or switch below a
root port may route peer-to-peer before the unit sees it (PCIe Base §2.4), so
a descriptor carrying one reaches another function's registers rather than
faulting. A claim domain reaches it after about 4,096 claim/release cycles of
one slot (`issues/kernel/a-claim-spends-device-addresses-its-slot-never-gets-back.md`);
on the T14 it matters below the Thunderbolt root ports with a dock attached.

Owner: the IOMMU track's stage 1, branch `wt/toyos-iommu1`. Exit: a domain's
range ends below the first root-bridge window or reserved region that reaches
above its floor, and a domain with no room left there is refused — held by a
`const` assertion over the T14's windows, and by the `domain_ends_below_the_host_bridges`
metal row, which reds on a domain record whose range meets a window firmware
declared.
