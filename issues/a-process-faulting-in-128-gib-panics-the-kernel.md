---
status: open
kind: defect
opened: 2026-10-04
---

# A process that faults in 128 GiB panics the kernel

Every demand fault pushes the 2 MiB page it filled onto
`ProcessData::demand_pages` (`kernel/src/process.rs`, `handle_page_fault`), a
`Vec` of 24-byte `PageAlloc`s that nothing bounds but physical memory. The push
from 65,536 entries to 65,537 doubles it to a 3,145,728-byte allocation, past
`mm::MAX_HEAP_ALLOC` (2,093,056), where `KernelAllocator::alloc` asserts: one
unprivileged process touching 128 GiB of its own mappings panics the kernel.

No machine this tree boots has that much memory — the T14's PMM manages
16,020 MiB — so it is unreached, not unreachable. The 24 bytes are
`size_of::<PageAlloc>()`, read off a build of this tree; the rest is
arithmetic over them.

Owner: orchestrator. Exit: the ledger is held in pieces no larger than one
heap allocation, or a fault past a per-process bound is refused by name, and a
constant assertion ties whichever it is to `MAX_HEAP_ALLOC`.
