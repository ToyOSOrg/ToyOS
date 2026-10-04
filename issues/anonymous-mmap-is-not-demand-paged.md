---
status: open
kind: defect
opened: 2026-08-08
---

# Anonymous `mmap` is not demand-paged

A claim this tree makes about itself that its own code does not keep. Found
by `wt/toyos-fpu` while building a page-fault workload for `fpu_isolation`, and
recorded rather than fixed: it is a memory-subsystem change with its own
blast radius.

**`sys_mmap` allocates and maps the whole region up front.** `PageAlloc::new` is
called for the full rounded size before anything is mapped, and
`alloc_and_map` maps every 2 MiB page of it (`kernel/src/syscall/vm.rs`'s
`sys_mmap`). So a first touch of a fresh anonymous mapping is an ordinary store
and never a `#PF`, and a program that reserves a large region pays for all of it
immediately. Measured: `syscall_cost` mapping 128 MiB and touching one byte per
page reported the guest's `peak=130MB` and its fault trace named two faults, both
in the ELF. CLAUDE.md's "Demand paging" is true of a *file-backed* segment and
of nothing else. Whether it should be lazy is a design question with a real
answer either way — the eager path is simpler and cannot fail late — but the
description and the code should agree.

The one demand-paged thing a userland program can still reach is a *writable
file-backed* page — `demand_paging_sse` and `fpu_isolation` both use one — at
2 MiB of test image per fault.
