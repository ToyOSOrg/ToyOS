---
status: open
kind: finding
opened: 2026-11-11
---

# The kernel stacks' range keeps every page table it ever made

A thread's kernel stack is mapped in a slot of a kernel virtual range of its
own (`kernel/src/mm/stack.rs`). The stack's frames are its job's and go back
when it drops. The range's page tables are the kernel's, charged to no job,
and are never freed: a dropped stack's slot is reused only after a flush, and
freeing a table without one would leave it named by another CPU's
paging-structure cache. This is the orchestrator's decision, taken after
review r1 of #1211 and recorded on that pull request.

The bound is one 4 KiB table for each 8 slots ever used and one directory for
each 4096. The slots ever used are at most the most stacks alive at once and
64 more (`kernel::stackslots::FLUSH_BATCH`): a never-used slot is drawn only
where fewer than 64 retired ones wait and no flush is under way, and a spawn
that meets a flush waits for the slots it cleans. So 2 MiB of tables for a
process at its 4096-thread limit, and 128 MiB at the range's own limit of
262144 stacks alive at once. A spawn that cannot get a frame for one is
refused. The slots' own bookkeeping is 96 KiB of the kernel's image.

Not measured: no boot has read how many tables the range holds.

**Exit**: a census line shows the tables' count on a machine. If a workload
then shows the bound mattering, the tables of a 2 MiB page whose eight slots
are all clean can be freed behind the flush that made them clean.
