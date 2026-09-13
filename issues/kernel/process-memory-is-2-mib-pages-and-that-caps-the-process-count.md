---
status: open
kind: track
opened: 2026-09-08
---

# Process memory is 2 MiB pages, and that caps the process count

**Owner premise (2026-09-08):** ToyOS is a gold-standard OS for modern
hardware with no restrictions for modern hardware. "The right simplification
for this OS" was never the premise. A design that caps what modern hardware
can do is a restriction, and 2 MiB-only paging caps the process count.

**What is to be built:** process memory — stacks, thread-local blocks, heaps,
small data and code segments, and device windows handed to a process — moves
to 4 KiB pages. 2 MiB stays where it pays: large mappings, DMA grants, the
IOMMU's superpages, the kernel's own mappings. One paging design serves both,
on x86-64 and on the ARM64 the tree is kept portable for
(`issues/kernel/arm64-is-a-decision-nobody-has-made.md`).

**Blocked on:** the owner's word to start. Recorded, not started, at the
owner's instruction.

**The constraint that makes this a track, measured on the T14** (run 29
readback, `PMM: 100/16038MB used` at 11 s with four user processes and three
kernel threads live; the same record's rows: stack 16 pages held, init-tls 5,
elf 4, mmap 7, demand-page 4): a user process holds about 10 to 14 MB before
it does any work, one 2 MiB page each for its code and data, its thread-local
block, its first heap mapping and its stack, a second stack page as it grows.
A thousand such processes is the whole 16 GB machine. Packing a process's
small regions into one shared page lowers the floor to about 4 MB, since every
stack still needs its own page with an unmapped neighbour as its guard; that
moves the ceiling to a few thousand and not past it. x86-64 offers no page
size between 4 KiB and 2 MiB.

**What this removes as a side effect:** a device window mapped at 4 KiB no
longer shares a 2 MiB page with a neighbour's registers, so the relocation
that `kernel/src/pcidev/` does before a hand-over is no longer needed for a
window firmware placed apart from its neighbours. Placing a window inside the
host bridge's firmware-reported apertures stays correct under either page size.
