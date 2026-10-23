---
status: open
kind: defect
opened: 2026-10-23
---

# The kernel heap's fragmentation is charged to nobody

Kernel memory a process causes is charged in bytes (`kernel/src/mm/charge.rs`),
and the frame allocator reserves whole superframes for what the charges add up
to (`kernel/pure/frames.rs`). The heap's footprint is not bounded by its live
bytes: `dlmalloc` keeps free chunks it cannot reuse for a request of another
size. So a process that keeps its charged total constant while it changes the
sizes of what it holds forces the heap to grow with no new charge:

- with every frame taken, it frees every other small name it made on `/tmp`
  and makes larger ones in their place, or does the same with port badges or
  namespaces, whose sizes it chooses;
- that growth takes a spare superframe while one is left, then a kept one;
- past the kept superframes, `alloc` answers null and the kernel panics,
  naming the allocation.

Derived from the allocator's design, not measured. The charges' estimates
overcount (a B-tree entry is charged about three times its bytes), which
absorbs some of it, but nothing bounds it. The host model in `frames.rs` does
not see it: it holds the heap at exactly what its charges need.

**Exit**, either of:

- size-segregated slabs for what processes cause, each class's tail bounded,
  so a class's footprint is a function of its live count and charged as such;
- charges taken against the heap's footprint rather than its bytes, so growth
  the heap needs is charged to the call that caused it.

Either with a guest case in `kernel_objects_refused` that churns name sizes
with every frame taken and is refused, never panics.
