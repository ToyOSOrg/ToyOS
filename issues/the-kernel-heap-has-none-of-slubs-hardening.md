---
status: open
kind: defect
opened: 2026-09-29
---

# The kernel heap has none of SLUB's hardening

The kernel heap is `dlmalloc` 0.2.13 (`kernel/src/mm/alloc.rs:500,513`). A
free chunk carries its list links as plain pointers (`src/dlmalloc.rs:62-67`),
and unlinking one writes through both without checking that either points
back (`src/dlmalloc.rs:1118-1131`), so one heap overflow into a free chunk is an
arbitrary write. Chunks come back in a fixed order, and every type of every
size shares one arena. Linux at `Ubuntu-6.8.0-142.142` sets three defaults
against this on the T14: `CONFIG_SLAB_FREELIST_HARDENED` stores each free
pointer XORed with a per-cache secret and its own address
(`mm/slub.c:479-490`), `CONFIG_SLAB_FREELIST_RANDOM` shuffles each slab's
free list (`mm/slub.c:2228,2290`), and `CONFIG_RANDOM_KMALLOC_CACHES` spreads
each size over 16 caches chosen by call site and a per-boot seed
(`include/linux/slab.h:340-341,398-401`).

**Ruled** (owner, 2026-10-04, "Change the goal"): "Close the issue with the
allocator's first stage under that stronger, ToyOS-shaped goal instead of
copying Linux's SLUB features one by one." The goal, as put to him: no
allocator bookkeeping stored inside objects, every free checked, and data
kept apart from pointers. The first stage is the kernel's front of
`issues/toyos-has-its-own-allocator.md`, which owns this issue.

**Exit**: the kernel heap is that front, and its host tests hold the goal:
no free-list link or size is stored in memory an object occupies, so a write
past one object into a freed neighbour is followed by an allocation that
returns sound memory; a free of a pointer the heap did not hand out, and a
second free of one, each panic; and an allocation holding pointers never
shares a page with one holding only data. Each of those tests reds against
`dlmalloc`, or against the front with that property removed. The tests are
the orchestrator's reading of the goal, not his.
