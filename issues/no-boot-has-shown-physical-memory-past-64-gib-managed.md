---
status: open
kind: defect
opened: 2026-10-12
---

# No boot has shown physical memory past 64 GiB managed

The frame allocator's records were a constant 32,768 bits, one per 2 MiB
frame, and a usable span past 64 GiB panicked the boot. They are now sized at
boot from the firmware map's usable span and laid out of memory it names
(`kernel/src/mm/pmm.rs`'s `init`), so nothing bounds the span by a constant.
The records must lie below the boot map's 4 GiB (`toyos_bootmap::BOOT_MAP_BYTES`),
which `init` refuses by name: about 4.2 MiB of them per 4 GiB of span, in one
usable stretch. **The bound is the span, not the RAM**: the records cover
every superframe from the lowest usable address to the highest, holes
included, so firmware that places modest DRAM in widely separated windows,
as some AArch64 servers do, can need more records than any stretch below
4 GiB holds. Records per usable region would remove that.

Neither bench reaches 64 GiB: the firmware calls 16,885,280,768 bytes usable
on the T14 (its `pmm:` line, and an identity domain ending at `0x45f800000`,
https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6114828923) and
16,505,958,400 on the AMD bench
(https://github.com/ToyOSOrg/ToyOS/pull/852#issuecomment-6107031099), and no
guest the harness boots has that much.

**Exit**: a boot whose firmware map's usable span passes 64 GiB reaches
`Boot: complete` with every usable frame managed.
