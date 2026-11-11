---
status: owner
kind: question
opened: 2026-11-11
---

# The kernel stacks' range keeps every page table it ever made

A thread's kernel stack is mapped in a slot of a kernel virtual range of its
own (`kernel/src/mm/stack.rs`). The stack's frames are its job's and go back
when it drops. The range's page tables are the kernel's, charged to no job,
and are never freed: a dropped stack's slot is reused only after a flush, and
freeing a table without one would leave it named by another CPU's
paging-structure cache.

The bound is one 4 KiB table for each 8 slots ever in use at once and one
directory for each 4096. The slots ever in use are at most the most stacks
alive at once, 64 more (`kernel::stackslots::FLUSH_BATCH`), and one for each
spawn that raced a flush: 2 MiB of tables for a process at its 4096-thread
limit, and 128 MiB at the range's own limit of 262144 stacks. A spawn that
cannot get a frame for one is refused. The slots' own bookkeeping is 96 KiB
of the kernel's image.

The range and this allocation are the orchestrator's decision, taken after
review r1 of #1211 and recorded on that pull request; the owner has not ruled
on either.

Not measured: no boot has read how many tables the range holds.

**For the owner**: whether a kernel allocation that is charged to no job and
never returned is accepted at this bound. If it is not, the tables of a
2 MiB page whose eight slots are all clean can be freed behind the flush that
made them clean.
