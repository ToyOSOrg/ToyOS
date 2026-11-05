---
status: open
kind: defect
severity: [kernel-from-userland]
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

## Options

Recorded for when kernel memory is next reshaped; none is chosen.

1. **Size-class slabs for what processes cause.** Each class grows by pages
   and charges its class size per object. A class's footprint then follows
   its live count only while its pages stay full: a process that frees all
   but one object per page holds every page at one object's charge. So slabs
   bound the tail only if each class's pages are charged whole to one job.
   This also adds a second allocator beside dlmalloc.
2. **Charge the heap's footprint.** A growth is charged to the call that
   triggered it. This is unfair across jobs (one job's frees make the holes
   another job's sizes cannot reuse), and a superframe outlives the charge
   that grew it.
3. **A per-job arena.** A job's charged objects come from pages charged to
   that job alone, so its fragmentation spends only its own quota and nobody
   else's. That is option 1's whole-page charge made the rule. Kernel objects
   are `Box`, `Vec` and `BTreeMap` on the global allocator, so this needs
   allocator-aware containers (`allocator_api` is unstable) or an arena
   carried by every charged object's type.

**Lean: 3.** It is the only option whose bound holds against a hostile
holder by construction, by charging pages rather than bytes to the job that
holds them. 1 converges on it once its tail problem is fixed. The cost is
threading the job's arena through every charged allocation.

**Exit**: a process's charged kernel objects cannot grow the heap past what
is charged to it, with a guest case in `kernel_objects_refused` that churns
name sizes with every frame taken and is refused, never panics.
