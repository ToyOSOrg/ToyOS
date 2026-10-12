---
status: open
kind: defect
opened: 2026-10-12
---

# Physical memory past 64 GiB panics the boot

The frame allocator's bitmap is a constant 32,768 bits, one per 2 MiB frame
(`kernel/src/mm/pmm.rs:36-43`), and `init` spans it from the lowest usable
frame to the highest and asserts the span fits
(`pmm.rs:104-118`): `pmm: physical memory exceeds 64 GB`. The bound is on
the span, not on the RAM, so a machine with 64 GiB installed already reaches
it once firmware places memory above the 32-bit MMIO hole. Inferred from the
code; no machine with that much memory has booted ToyOS. Neither bench
reaches it: the firmware calls 16,885,280,768 bytes usable on the T14
(its `pmm:` line, and an identity domain ending at `0x45f800000`,
https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6114828923) and
16,505,958,400 on the AMD bench
(https://github.com/ToyOSOrg/ToyOS/pull/852#issuecomment-6107031099).

**Exit**: a boot whose firmware map's usable span passes 64 GiB reaches
`Boot: complete` with every usable frame managed, and nothing in
`kernel/src/mm/pmm.rs` sizes the bitmap by a constant.
