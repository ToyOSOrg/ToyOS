---
status: open
kind: defect
opened: 2026-11-11
---

# A dropped kernel stack never goes back to the frame allocator

A kernel stack is carved out of a superframe the heap's accounting holds, and
the page under it is a hole in the direct map (`kernel/src/mm/stack.rs`). A
thread's stack that drops is kept for the next thread and its memory is never
returned: a superframe with a hole in it cannot be reissued. So the stack pool
holds what the largest number of threads alive at once took, 132 KiB each
plus the area for its user machine state, until the machine stops. The charge
for a stack is given back when its thread ends, so the memory is counted to
nobody while it waits.

Not measured: no boot has read how much the pool holds after a thread storm.

**Exit**: a superframe every stack of which is free has its guard pages mapped
again and goes back to the frame allocator, with a bound on how often one is
carved again; or a census line shows the pool's size and the owner accepts the
bound.
